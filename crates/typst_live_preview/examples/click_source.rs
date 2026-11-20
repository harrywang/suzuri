//! Verifies click-to-source against a real Tinymist.
//!
//! The pane turns a click on a page into a `src-point` message on the preview
//! data plane; Tinymist resolves it against the compiler's span map and answers
//! with an LSP `window/showDocument` request. This harness reproduces the whole
//! path without a GUI, through the production [`SessionHandle`]:
//!
//! 1. start Tinymist and the preview exactly like the pane,
//! 2. wait for the document, then ask the session to rasterize page 1,
//! 3. pick the centre of the first text run from the page accessibility data,
//! 4. call `request_source_jump` like a mouse click does,
//! 5. assert that `window/showDocument` arrives with the file and a selection.
//!
//! ```sh
//! cargo run -p typst_live_preview --example click_source -- /path/to/tinymist
//! ```

// This harness needs a wall-clock timeout around an async channel; the
// workspace's disallowed-methods lint points at the GPUI executor, which these
// desktop-free examples do not have.
#![allow(clippy::disallowed_methods)]

mod common;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use common::start_preview;
use typst_live_preview::connection::PreviewConnectionConfig;
use typst_live_preview::{PageAccessibility, SessionEvent, SessionHandle};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let tinymist = args.next().unwrap_or_else(|| "tinymist".to_string());
    let fixture = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview.typ")
    });

    let source = std::fs::read_to_string(&fixture)
        .with_context(|| format!("reading {}", fixture.display()))?;
    let fixture = fixture
        .canonicalize()
        .with_context(|| format!("resolving {}", fixture.display()))?;
    let uri = format!("file://{}", fixture.display());
    let root = fixture
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
        .display()
        .to_string();

    let task_id = "typst-click-source";
    let entry = fixture.display().to_string();
    let (mut server, port) = start_preview(&tinymist, &root, &uri, &source, &entry, task_id)?;
    eprintln!("preview data plane on 127.0.0.1:{port}");

    let (events_tx, events_rx) = smol::channel::unbounded();
    let session = SessionHandle::spawn(PreviewConnectionConfig::loopback(port), events_tx);

    let (page_index, x, y, text) = smol::block_on(async {
        // The session reports the document after merging the initial frame.
        let content_hash = loop {
            match next_event(&events_rx).await? {
                SessionEvent::Document { pages } => {
                    if let Some(page) = pages.first() {
                        break page.content_hash;
                    }
                }
                SessionEvent::Error(error) => bail!("session error: {error}"),
                SessionEvent::Disconnected => {
                    bail!("session disconnected before the document arrived")
                }
                _ => {}
            }
        };

        // Rasterize page 1 so its text runs are available for hit testing.
        session.request_pages(1.0, vec![(0, content_hash)]);
        let accessibility: PageAccessibility = loop {
            match next_event(&events_rx).await? {
                SessionEvent::Rendered {
                    index: 0,
                    accessibility,
                    ..
                } => break accessibility,
                SessionEvent::Error(error) => bail!("session error: {error}"),
                SessionEvent::Disconnected => bail!("session disconnected before page 1 rendered"),
                _ => {}
            }
        };

        // Skip the zero-width `\n` content hints that separate blocks; they
        // carry no source span, so clicking one cannot resolve to the file.
        let run = accessibility
            .text_runs()
            .iter()
            .find(|run| !run.text().trim().is_empty())
            .context("the fixture produced no selectable text runs")?;
        let bounds = run.bounds();
        Ok((
            0usize,
            (bounds.x0 + bounds.x1) / 2.0,
            (bounds.y0 + bounds.y1) / 2.0,
            run.text().to_string(),
        ))
    })?;

    eprintln!(
        "clicking page {} at ({x:.1}, {y:.1}) pt, on {:?}",
        page_index + 1,
        text
    );
    // This is exactly what `TypstPreviewView::handle_mouse_up` calls.
    session.request_source_jump(page_index, x, y);

    let request = server.wait_for_request("window/showDocument", Duration::from_secs(10))?;
    let params = &request["params"];
    eprintln!("window/showDocument params: {params}");
    server.respond(
        request["id"].clone(),
        serde_json::json!({ "success": true }),
    );

    let shown_uri = params["uri"].as_str().unwrap_or_default();
    assert!(
        shown_uri.ends_with("preview.typ"),
        "expected the fixture uri, got {shown_uri:?}"
    );
    let selection = &params["selection"];
    assert!(
        selection.get("start").is_some() && selection.get("end").is_some(),
        "expected a selection range, got {selection}"
    );

    drop(session);
    server.notify(
        "workspace/executeCommand",
        serde_json::json!({
            "command": "tinymist.doKillPreview",
            "arguments": [task_id],
        }),
    );
    server.shutdown();
    println!("ok: click resolved to {shown_uri} at {selection}");
    Ok(())
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
            smol::Timer::after(Duration::from_secs(15)).await;
            bail!("timed out waiting for a session event")
        },
    )
    .await
}
