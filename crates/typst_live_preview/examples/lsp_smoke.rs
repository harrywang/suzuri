//! End-to-end smoke test against a real `tinymist lsp` process.
//!
//! It exercises exactly the contract the preview pane relies on:
//!
//! 1. `tinymist.focusMain` points the server at the document.
//! 2. `tinymist.doStartBrowsingPreview` returns a data-plane port.
//! 3. The data plane sends a full `new,` frame on connect.
//! 4. An *unsaved* `textDocument/didChange` produces a `diff-v1,` frame.
//!
//! Run it with a Tinymist binary (the default is `tinymist` on `PATH`):
//!
//! ```sh
//! cargo run -p typst_live_preview --example lsp_smoke -- /path/to/tinymist
//! ```
//!
//! It does not modify the fixture on disk; the edit exists only in the LSP's
//! in-memory source, which is the point.

mod common;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use common::{REQUEST_TIMEOUT, end_position, start_preview};
use serde_json::json;
use typst_live_preview::{
    connection::{self, PreviewConnectionConfig, PreviewEvent},
    protocol::{PreviewDataFrame, split_preview_data_frame},
};

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

    let task_id = "typst-live-preview-smoke";
    let entry = fixture.display().to_string();
    let (mut server, port) = start_preview(&tinymist, &root, &uri, &source, &entry, task_id)?;
    eprintln!("preview data plane on 127.0.0.1:{port}");

    // Receive frames exactly like the pane does.
    let (events_tx, events_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let connection_stop = stop.clone();
    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        if let Err(error) = connection::run(
            PreviewConnectionConfig::loopback(port),
            events_tx,
            outbound_rx,
            connection_stop,
        ) {
            eprintln!("connection stopped: {error}");
        }
    });

    let full_frame = wait_for_frame(&events_rx, REQUEST_TIMEOUT, |frame| {
        matches!(
            split_preview_data_frame(frame),
            Some(PreviewDataFrame::FullCurrent(_))
        )
    })?;
    eprintln!("received a full new, frame ({} bytes)", full_frame.len());

    // Insert at the end of the document without saving it.
    let (line, character) = end_position(&source);
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": line, "character": character },
                    "end": { "line": line, "character": character },
                },
                "text": "#text[Live edit from the LSP smoke test.]\n",
            }],
        }),
    );

    let diff_frame = wait_for_frame(&events_rx, REQUEST_TIMEOUT, |frame| {
        matches!(
            split_preview_data_frame(frame),
            Some(PreviewDataFrame::DiffV1(_))
        )
    })?;
    let PreviewDataFrame::DiffV1(payload) =
        split_preview_data_frame(&diff_frame).expect("checked by the predicate")
    else {
        unreachable!()
    };
    eprintln!(
        "received a diff-v1, frame after an unsaved edit ({} bytes, {} payload)",
        diff_frame.len(),
        payload.len()
    );

    stop.store(true, Ordering::Relaxed);
    let kill = server.request(
        4,
        "workspace/executeCommand",
        json!({
            "command": "tinymist.doKillPreview",
            "arguments": [task_id],
        }),
    )?;
    if kill.get("error").is_some() {
        eprintln!("killPreview returned an error: {kill}");
    }
    server.shutdown();

    println!(
        "ok: new={} bytes, diff={} bytes",
        full_frame.len(),
        diff_frame.len()
    );
    Ok(())
}

fn wait_for_frame(
    events: &mpsc::Receiver<PreviewEvent>,
    timeout: Duration,
    predicate: impl Fn(&[u8]) -> bool,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for a preview frame");
        }
        match events.recv_timeout(remaining) {
            Ok(PreviewEvent::Binary(frame)) if predicate(&frame) => return Ok(frame),
            Ok(PreviewEvent::Binary(_)) => {}
            Ok(PreviewEvent::Connected) => eprintln!("data plane connected"),
            Ok(PreviewEvent::ConnectFailed(error)) => eprintln!("connect failed: {error}"),
            Ok(PreviewEvent::Text(text)) => eprintln!("text: {text}"),
            Err(error) => bail!("waiting for a preview frame: {error}"),
        }
    }
}
