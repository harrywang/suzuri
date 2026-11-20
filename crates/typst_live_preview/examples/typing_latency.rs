//! Measures Tinymist's keystroke-to-frame latency.
//!
//! The preview pane's end-to-end latency is
//!
//! ```text
//! keystroke -> LSP didChange -> tinymist compiles -> diff frame on the data plane
//!   -> merge -> rasterize -> paint
//! ```
//!
//! This harness measures the first half, which the client cannot control, by
//! sending one in-memory edit at a time and timing the next preview frame. It
//! then runs the client's merge, scene rebuild, rasterization, and image
//! conversion on that frame, all timed from the keystroke.
//!
//! ```sh
//! cargo run -p typst_live_preview --example typing_latency -- \
//!     /path/to/tinymist [fixture.typ] [edits] [interval-ms]
//! ```

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

use anyhow::{Context as _, Result};
use common::{REQUEST_TIMEOUT, end_position, start_preview};
use serde_json::json;
use typst_live_preview::{
    PreviewSession, SceneRasterizer,
    connection::{self, PreviewConnectionConfig, PreviewEvent},
    protocol::preview_update_from_bytes,
};
use vello::peniko::Color;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let tinymist = args.next().unwrap_or_else(|| "tinymist".to_string());
    let fixture = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview.typ")
    });
    let edits: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    let interval_ms: u64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);

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

    let task_id = "typst-typing-latency";
    let entry = fixture.display().to_string();
    let (mut server, port) = start_preview(&tinymist, &root, &uri, &source, &entry, task_id)?;

    let (events_tx, events_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let connection_stop = stop.clone();
    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let _ = connection::run(
            PreviewConnectionConfig::loopback(port),
            events_tx,
            outbound_rx,
            connection_stop,
        );
    });

    // Merge the initial full frame before any diff: the incremental IR cannot
    // apply a delta to an empty document.
    let mut session = PreviewSession::new();
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        match events_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(PreviewEvent::Binary(frame)) => {
                let Some(update) = preview_update_from_bytes(&frame) else {
                    continue;
                };
                if update.reset_before_merge {
                    eprintln!("connected: initial frame {} bytes", frame.len());
                    session.merge(update);
                    let _ = session.page_metas();
                    break;
                }
            }
            Ok(_) => {}
            Err(error) => anyhow::bail!("waiting for the initial frame: {error}"),
        }
    }
    // Let tinymist settle after the initial render.
    std::thread::sleep(Duration::from_millis(500));

    let (line, character) = end_position(&source);
    let mut latencies = Vec::with_capacity(edits);
    let mut rasterizer = SceneRasterizer::new();
    // Matches a fit-to-width page on a 2x display, the common HiDPI case.
    let render_scale = 2.0;

    for edit in 0..edits {
        // Append one character per edit so the document keeps changing.
        let text = format!("{edit} ");
        let sent_at = Instant::now();
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": edit as i64 + 2 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": line, "character": character },
                        "end": { "line": line, "character": character },
                    },
                    "text": text,
                }],
            }),
        );

        // Collect the whole train of frames this edit causes, feeding them
        // through the same merge + metadata path the pane uses. A later frame
        // restarts the pane's settle timer, so its arrival time matters, not
        // just the first frame.
        let first = collect_frames(&events_rx, &mut session, sent_at, edit < 3);
        let Some(first_frame_at) = first else {
            println!("edit {edit:>2}: no diff frame");
            continue;
        };
        latencies.push(first_frame_at);

        // Now run everything the session thread does after the pane decides to
        // render, still timed from the keystroke.
        let scenes_at = Instant::now();
        let pages = session.render_pages().context("rendering the pages")?;
        let scenes_time = scenes_at.elapsed();
        let Some(page) = pages.first() else {
            println!("edit {edit:>2}: no pages");
            continue;
        };
        let raster_at = Instant::now();
        let rendered = rasterizer
            .render_page(&page.scene, page.size, render_scale, Color::WHITE)
            .context("rasterizing")?;
        let raster_time = raster_at.elapsed();
        let convert_at = Instant::now();
        let image = rendered.into_render_image();
        let convert_time = convert_at.elapsed();
        drop(image);

        println!(
            "edit {edit:>2}: frame {:>6.1} ms | scenes {:>6.1} ms | raster {:>6.1} ms | convert {:>6.1} ms | total {:>6.1} ms",
            first_frame_at.as_secs_f64() * 1e3,
            scenes_time.as_secs_f64() * 1e3,
            raster_time.as_secs_f64() * 1e3,
            convert_time.as_secs_f64() * 1e3,
            sent_at.elapsed().as_secs_f64() * 1e3,
        );
        if edit % 10 == 0 {
            println!(
                "        session {:?}, rss {:?} MiB",
                session.stats(),
                resident_mib()
            );
        }

        std::thread::sleep(Duration::from_millis(interval_ms));
    }

    if !latencies.is_empty() {
        latencies.sort_unstable();
        let at = |percentile: f64| {
            let index = ((latencies.len() - 1) as f64 * percentile).round() as usize;
            latencies[index].as_secs_f64() * 1e3
        };
        println!(
            "\nkeystroke -> diff frame: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms ({} edits)",
            at(0.50),
            at(0.95),
            at(1.0),
            latencies.len()
        );
    }

    stop.store(true, Ordering::Relaxed);
    server.notify(
        "workspace/executeCommand",
        json!({
            "command": "tinymist.doKillPreview",
            "arguments": [task_id],
        }),
    );
    server.shutdown();
    Ok(())
}

/// Collects frames until the stream has been quiet for a while, merging each
/// one into `session`. Returns the time to the first `diff-v1` frame.
fn collect_frames(
    events: &mpsc::Receiver<PreviewEvent>,
    session: &mut PreviewSession,
    sent_at: Instant,
    trace: bool,
) -> Option<Duration> {
    const QUIET: Duration = Duration::from_millis(400);
    let mut first = None;
    let mut last_metas = None;
    let mut frame_count = 0;

    loop {
        let received = events.recv_timeout(QUIET);
        let Ok(PreviewEvent::Binary(frame)) = received else {
            if let Err(mpsc::RecvTimeoutError::Timeout) = received {
                break;
            }
            continue;
        };

        let Some(update) = preview_update_from_bytes(&frame) else {
            continue;
        };
        let kind = if update.reset_before_merge {
            "new"
        } else {
            "diff"
        };
        frame_count += 1;
        session.merge(update);
        let metas = session.page_metas();
        let changed = last_metas.as_ref() != Some(&metas);
        last_metas = Some(metas.clone());

        let elapsed = sent_at.elapsed();
        if kind == "diff" && first.is_none() {
            first = Some(elapsed);
        }
        if trace {
            println!(
                "        +{:>7.1} ms: {kind} frame #{frame_count} ({} bytes), metachange={changed}, pages={}",
                elapsed.as_secs_f64() * 1e3,
                frame.len(),
                metas.len(),
            );
        }
    }

    first
}

fn resident_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<f64>().ok()?;
    Some(kib / 1024.0)
}
