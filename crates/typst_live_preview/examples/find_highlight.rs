//! Debug harness for find-in-preview highlights.
//!
//! Starts Tinymist through the same LSP commands the preview pane uses, renders
//! every page, and prints the rectangles a search query produces. This makes it
//! possible to compare highlight widths against the per-character advances
//! coming from the preview IR without opening the editor.
//!
//! ```sh
//! cargo run -p typst_live_preview --example find_highlight -- \
//!   /path/to/tinymist /path/to/doc.typ "query"
//! ```

// This harness needs wall-clock timeouts; the workspace's disallowed-methods
// lint points at the GPUI executor, which this desktop-free example lacks.
#![allow(clippy::disallowed_methods)]

mod common;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use common::start_preview;
use typst_live_preview::{
    PageAccessibility, PageTextPosition, PageTextSelection, SessionEvent, SessionHandle,
    connection::PreviewConnectionConfig,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let tinymist = args.next().unwrap_or_else(|| "tinymist".to_string());
    let document = args
        .next()
        .map(PathBuf::from)
        .context("usage: find_highlight <tinymist> <document.typ> <query>")?;
    let query = args
        .next()
        .context("usage: find_highlight <tinymist> <document.typ> <query>")?;

    let source = std::fs::read_to_string(&document)
        .with_context(|| format!("reading {}", document.display()))?;
    let document = document
        .canonicalize()
        .with_context(|| format!("resolving {}", document.display()))?;
    let uri = format!("file://{}", document.display());
    let root = document
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
        .display()
        .to_string();
    let entry = document.display().to_string();

    let task_id = "typst-find-highlight";
    let (mut server, port) = start_preview(&tinymist, &root, &uri, &source, &entry, task_id)?;
    eprintln!("preview data plane on 127.0.0.1:{port}");

    let (events_tx, events_rx) = smol::channel::unbounded();
    let session = SessionHandle::spawn(PreviewConnectionConfig::loopback(port), events_tx);

    smol::block_on(async {
        let (metas, accessibility) = render_all(&events_rx, &session).await?;
        for (page_index, accessibility) in accessibility.iter().enumerate() {
            report_matches(page_index, accessibility, &query);
        }
        eprintln!("rendered {} page(s)", metas);
        Ok::<_, anyhow::Error>(())
    })?;

    session.shutdown();
    server.notify(
        "workspace/executeCommand",
        serde_json::json!({
            "command": "tinymist.doKillPreview",
            "arguments": [task_id],
        }),
    );
    server.shutdown();
    Ok(())
}

async fn render_all(
    events: &smol::channel::Receiver<SessionEvent>,
    session: &SessionHandle,
) -> Result<(usize, Vec<PageAccessibility>)> {
    let metas = loop {
        match next_event(events).await? {
            SessionEvent::Document { pages } => break pages,
            SessionEvent::Error(error) => bail!("session error: {error}"),
            SessionEvent::Disconnected => bail!("session disconnected before the document arrived"),
            _ => {}
        }
    };
    let requested = metas
        .iter()
        .map(|page| (page.index, page.content_hash))
        .collect::<Vec<_>>();
    session.request_pages(1.0, requested);

    let mut pages = metas.iter().map(|_| None).collect::<Vec<_>>();
    let mut remaining = metas.len();
    while remaining > 0 {
        match next_event(events).await? {
            SessionEvent::Rendered {
                index,
                accessibility,
                ..
            } if index < pages.len() => {
                if pages[index].replace(accessibility).is_none() {
                    remaining -= 1;
                }
            }
            SessionEvent::Error(error) => bail!("session error: {error}"),
            SessionEvent::Disconnected => bail!("session disconnected before every page rendered"),
            _ => {}
        }
    }

    let pages = pages
        .into_iter()
        .enumerate()
        .map(|(index, page)| page.with_context(|| format!("page {index} did not render")))
        .collect::<Result<Vec<_>>>()?;
    Ok((metas.len(), pages))
}

fn report_matches(page_index: usize, accessibility: &PageAccessibility, query: &str) {
    if query.is_empty() {
        return;
    }

    for (run_index, run) in accessibility.text_runs().iter().enumerate() {
        let matches = find_occurrences(run.text(), query);
        if matches.is_empty() {
            continue;
        }

        let character_widths = run
            .character_bounds()
            .iter()
            .map(|rect| rect.width())
            .collect::<Vec<_>>();
        let uniform = character_widths
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < 1e-6);
        eprintln!(
            "page {} run {}: {:?} chars={} uniform={} run_bounds={:?}",
            page_index + 1,
            run_index,
            run.text(),
            run.character_count(),
            uniform,
            run.bounds(),
        );

        for (start, end) in matches {
            if end > run.character_count() {
                continue;
            }
            let matched = run
                .text()
                .chars()
                .skip(start)
                .take(end - start)
                .collect::<String>();
            let selection = PageTextSelection {
                anchor: PageTextPosition {
                    run_index,
                    character_index: start,
                },
                focus: PageTextPosition {
                    run_index,
                    character_index: end,
                },
            };
            let rects = accessibility.selection_rects(selection);
            let widths = &character_widths[start..end];
            eprintln!(
                "  match {matched:?} chars {}..{} -> {:?}, widths={:?}, total={:.3}pt",
                start,
                end,
                rects,
                widths,
                widths.iter().sum::<f64>(),
            );
        }
    }
}

/// The same ASCII-case-insensitive character matching the preview's find bar
/// uses.
fn find_occurrences(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let haystack = text.chars().collect::<Vec<_>>();
    let needle = query
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if needle.len() > haystack.len() {
        return Vec::new();
    }
    (0..=haystack.len() - needle.len())
        .filter(|start| {
            haystack[*start..*start + needle.len()]
                .iter()
                .map(|character| character.to_ascii_lowercase())
                .eq(needle.iter().copied())
        })
        .map(|start| (start, start + needle.len()))
        .collect()
}

async fn next_event(events: &smol::channel::Receiver<SessionEvent>) -> Result<SessionEvent> {
    futures_lite::future::or(
        async {
            events
                .recv()
                .await
                .map_err(|_| anyhow::anyhow!("session closed"))
        },
        async {
            smol::Timer::after(Duration::from_secs(30)).await;
            bail!("timed out waiting for a session event")
        },
    )
    .await
}
