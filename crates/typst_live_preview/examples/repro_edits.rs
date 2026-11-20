//! Applies a sequence of unsaved edits and renders every page after each one.
//!
//! Used to reproduce preview failures that only show up after the document
//! changes shape (for example swapping an imported package).
//!
//! ```sh
//! cargo run -p typst_live_preview --example repro_edits -- \
//!     /path/to/tinymist file.typ 0pt 10pt
//! ```
//!
//! Arguments after the file are `FIND REPLACE` pairs. With no pairs it runs
//! the zero-font regression: `0pt` -> `10pt`.

#![allow(clippy::disallowed_methods)]

mod common;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use common::{REQUEST_TIMEOUT, start_preview};
use serde_json::json;
use smol::channel::{Receiver, TryRecvError};
use typst_live_preview::{SessionEvent, SessionHandle, connection::PreviewConnectionConfig};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let tinymist = args.next().unwrap_or_else(|| "tinymist".to_string());
    let fixture = args
        .next()
        .map(PathBuf::from)
        .context("usage: repro_edits TINYMIST FILE [FIND REPLACE]...")?;

    let mut edits: Vec<(String, String)> = Vec::new();
    while let Some(find) = args.next() {
        let replace = args.next().context("every FIND needs a REPLACE argument")?;
        anyhow::ensure!(!find.contains('\n'), "FIND must stay on one line: {find:?}");
        edits.push((find, replace));
    }
    if edits.is_empty() {
        edits.push(("0pt".to_string(), "10pt".to_string()));
    }

    let source = std::fs::read_to_string(&fixture)?;
    let fixture = fixture.canonicalize()?;
    let uri = format!("file://{}", fixture.display());
    let root = std::env::var("PREVIEW_ROOT").unwrap_or_else(|_| {
        fixture
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/"))
            .display()
            .to_string()
    });
    let entry = fixture.display().to_string();

    let (mut server, port) = start_preview(&tinymist, &root, &uri, &source, &entry, "repro")?;
    eprintln!("data plane on {port}");

    let scale = std::env::var("PREVIEW_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.0);

    let (events_tx, events_rx) = smol::channel::unbounded();
    let session = SessionHandle::spawn(PreviewConnectionConfig::loopback(port), events_tx);

    render_round(&events_rx, &session, "initial", scale)?;

    let mut source = source;
    let mut version = 1;
    for (find, replace) in edits {
        let (line, character) = position_of(&source, &find)?;
        eprintln!("editing line {line}, char {character}: {find:?} -> {replace:?}");
        version += 1;
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": line, "character": character },
                        "end": {
                            "line": line,
                            "character": character + find.chars().count() as u32,
                        },
                    },
                    "text": replace,
                }],
            }),
        );
        let label = format!("after {find:?} -> {replace:?}");
        render_round(&events_rx, &session, &label, scale)?;
        source = source.replacen(&find, &replace, 1);
    }

    session.shutdown();
    server.shutdown();
    println!("ok: every round rendered");
    Ok(())
}

fn render_round(
    events: &Receiver<SessionEvent>,
    session: &SessionHandle,
    label: &str,
    scale: f64,
) -> Result<()> {
    eprintln!("--- {label}: waiting for the document");
    let pages = loop {
        match recv_event(events, REQUEST_TIMEOUT)? {
            SessionEvent::Document { pages } => break pages,
            SessionEvent::Error(error) => eprintln!("{label}: session error: {error}"),
            SessionEvent::Disconnected => bail!("{label}: session disconnected"),
            other => eprintln!("{label}: {other:?}", other = DebugEvent(&other)),
        }
    };
    let requested = pages
        .iter()
        .map(|page| (page.index, page.content_hash))
        .collect::<Vec<_>>();
    if requested.is_empty() {
        bail!("{label}: document has no pages");
    }

    eprintln!(
        "--- {label}: requesting {} page(s) at {scale:.2}x",
        requested.len()
    );
    session.request_pages(scale, requested.clone());

    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut done = HashSet::new();
    while done.len() < requested.len() {
        if Instant::now() >= deadline {
            bail!(
                "{label}: only {} of {} page(s) rendered within {REQUEST_TIMEOUT:?}",
                done.len(),
                requested.len()
            );
        }
        match recv_event(events, Duration::from_secs(5)) {
            Ok(SessionEvent::Rendered { index, .. }) => {
                eprintln!("--- {label}: rendered page {index}");
                done.insert(index);
            }
            Ok(SessionEvent::Skipped { index }) => {
                eprintln!("--- {label}: skipped page {index} (document moved on)");
                done.insert(index);
            }
            Ok(SessionEvent::Error(error)) => {
                eprintln!("--- {label}: RENDER ERROR: {error}");
                return Ok(());
            }
            Ok(SessionEvent::RenderFailed { index, error, .. }) => {
                eprintln!("--- {label}: RENDER FAILED on page {index}: {error}");
                return Ok(());
            }
            Ok(SessionEvent::Disconnected) => {
                bail!("{label}: session disconnected while rendering")
            }
            Ok(other) => eprintln!("{label}: {other:?}", other = DebugEvent(&other)),
            Err(error) => eprintln!("{label}: {error}"),
        }
    }
    Ok(())
}

fn recv_event(events: &Receiver<SessionEvent>, timeout: Duration) -> Result<SessionEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        match events.try_recv() {
            Ok(event) => return Ok(event),
            Err(TryRecvError::Empty) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("timed out waiting for a session event");
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(TryRecvError::Closed) => bail!("session event channel closed"),
        }
    }
}

struct DebugEvent<'a>(&'a SessionEvent);

impl std::fmt::Debug for DebugEvent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            SessionEvent::Connected => write!(f, "Connected"),
            SessionEvent::Document { pages } => write!(f, "Document({} pages)", pages.len()),
            SessionEvent::Rendered { index, .. } => write!(f, "Rendered({index})"),
            SessionEvent::Skipped { index } => write!(f, "Skipped({index})"),
            SessionEvent::RenderFailed { index, error, .. } => {
                write!(f, "RenderFailed({index}: {error})")
            }
            SessionEvent::ScrollTo { page, .. } => write!(f, "ScrollTo({page})"),
            SessionEvent::WordCount(count) => write!(f, "WordCount({count:?})"),
            SessionEvent::Outline(_) => write!(f, "Outline"),
            SessionEvent::Error(error) => write!(f, "Error({error})"),
            SessionEvent::Disconnected => write!(f, "Disconnected"),
        }
    }
}

fn position_of(source: &str, needle: &str) -> Result<(u32, u32)> {
    let offset = source
        .find(needle)
        .with_context(|| format!("{needle:?} is not in the source"))?;
    let before = &source[..offset];
    let line = before.matches('\n').count();
    let character = before.rsplit('\n').next().unwrap_or("").chars().count();
    Ok((line as u32, character as u32))
}
