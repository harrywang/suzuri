//! End-to-end benchmark and stress test for the live preview.
//!
//! It generates the test documents itself, so it is repeatable without any
//! packages or network access:
//!
//! - `big.svg`: a 2.4 MB SVG with 30,000 circles on a 6000x4000 canvas.
//! - `bench-<N>.typ`: N pages of text, with the SVG rendered every sixth page.
//!
//! Two modes cover the two halves of the pipeline:
//!
//! - `micro` connects to Tinymist directly, merges the full frame, measures
//!   frame ingest, scene construction, rasterization at several scales, the
//!   oversized-target guard, and one incremental edit.
//! - `e2e` uses the same [`SessionHandle`] as the pane and measures the
//!   request-to-`Rendered` chain for the first page, zoom, scroll, and typing.
//!
//! ```sh
//! cargo run --release -p typst_live_preview --example benchmark -- --pages 10
//! cargo run --release -p typst_live_preview --example benchmark -- --pages 100 --mode e2e
//! ```
//!
//! `micro` is the place to compare Vello/GPU behavior; `e2e` includes the
//! session thread, channels, and the safety caps.

#![allow(clippy::disallowed_methods)]

mod common;

use std::{
    fmt::Write as _,
    fs,
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
use smol::channel::{Receiver, TryRecvError};
use typst_live_preview::{
    PreviewSession, SceneRasterizer, SessionEvent, SessionHandle,
    connection::{self, PreviewConnectionConfig, PreviewEvent},
    protocol::preview_update_from_bytes,
};
use vello::peniko::Color;

/// The user-facing budget for a single client-side chain.
const BUDGET: Duration = Duration::from_millis(30);
/// Rasterization repetitions per scale.
const RASTER_REPS: usize = 5;
/// Zoom/scroll request repetitions.
const REQUEST_REPS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Micro,
    E2e,
    Both,
}

struct Args {
    tinymist: String,
    pages: usize,
    edits: usize,
    mode: Mode,
    doc: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let doc = match args.doc {
        Some(doc) => doc,
        None => generate_docs(&bench_dir()?, args.pages)?,
    };
    println!(
        "document: {} ({} pages requested)",
        doc.display(),
        args.pages
    );

    if args.mode != Mode::E2e {
        run_micro(&args.tinymist, &doc, args.edits)?;
    }
    if args.mode != Mode::Micro {
        run_e2e(&args.tinymist, &doc, args.edits)?;
    }
    Ok(())
}

// -- Micro: merge, scene build, raster ------------------------------------

fn run_micro(tinymist: &str, doc: &Path, edits: usize) -> Result<()> {
    println!("\n=== micro (raw frames + direct rasterizer) ===");
    let source = fs::read_to_string(doc).with_context(|| format!("reading {}", doc.display()))?;
    let doc = doc.canonicalize()?;
    let uri = format!("file://{}", doc.display());
    let root = doc.parent().unwrap_or(Path::new("/")).display().to_string();
    let entry = doc.display().to_string();
    let task_id = format!("preview-bench-micro-{}", std::process::id());
    let (mut server, port) = start_preview(tinymist, &root, &uri, &source, &entry, &task_id)?;
    println!("data plane: 127.0.0.1:{port}");

    let (tx, rx) = mpsc::channel();
    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let connection_stop = stop.clone();
    std::thread::spawn(move || {
        if let Err(error) = connection::run(
            PreviewConnectionConfig::loopback(port),
            tx,
            outbound_rx,
            connection_stop,
        ) {
            eprintln!("benchmark connection stopped: {error}");
        }
    });

    let full = wait_for_full_frame(&rx)?;
    let full_kind = preview_update_from_bytes(&full)
        .map(|update| if update.reset_before_merge { "new" } else { "diff" })
        .unwrap_or("?");
    println!(
        "full frame: {:.1} MB ({full_kind})",
        full.len() as f64 / 1e6
    );

    let ingest_at = Instant::now();
    let update =
        preview_update_from_bytes(&full).context("the full frame is not a preview frame")?;
    let mut session = PreviewSession::new();
    session.merge(update);
    let metas = session.page_metas();
    let ingest = ingest_at.elapsed();
    println!(
        "merge + page metadata: {} pages, {}",
        metas.len(),
        pretty(ingest)
    );
    report_one("frame merge + metadata", ingest, true);

    let build_at = Instant::now();
    let pages = session.render_pages().context("building the scenes")?;
    let build = build_at.elapsed();
    println!("scene build: {} pages, {}", pages.len(), pretty(build));
    report_one("full scene build", build, true);
    println!("rss after scene build: {}", rss());

    let mut rasterizer = SceneRasterizer::new();
    // Pay the device/pipeline warm-up before timing any page.
    let _ = rasterizer
        .render_page(&pages[0].scene, pages[0].size, 1.0, Color::WHITE)
        .context("warming up the rasterizer")?;

    for scale in [1.0, 2.0, 4.0] {
        for index in sample_pages(pages.len()) {
            let page = &pages[index];
            let mut raster = Vec::with_capacity(RASTER_REPS);
            let mut convert = Vec::with_capacity(RASTER_REPS);
            let mut dims = (0, 0);
            for _ in 0..RASTER_REPS {
                let raster_at = Instant::now();
                let rendered = rasterizer
                    .render_page(&page.scene, page.size, scale, Color::WHITE)
                    .with_context(|| format!("rasterizing page {index} at {scale}x"))?;
                raster.push(raster_at.elapsed());
                dims = (rendered.width, rendered.height);
                let convert_at = Instant::now();
                let image = rendered.into_render_image();
                convert.push(convert_at.elapsed());
                drop(image);
            }
            println!(
                "raster page {index} @{scale:.1}x: {dims:?}, bins {}, raster {}, image {}",
                bins(dims.0, dims.1),
                summary(&raster),
                summary(&convert)
            );
            report("  raster", &raster, true);
            report("  convert", &convert, true);
        }
    }

    // The safety backstop must reject an oversized target without touching
    // the GPU. The view never asks for this, but a future caller might.
    let guard_at = Instant::now();
    let guard = rasterizer.render_page(&pages[0].scene, pages[0].size, 100.0, Color::WHITE);
    anyhow::ensure!(guard.is_err(), "the oversized target was not rejected");
    println!(
        "oversized target: rejected in {}, error: {}",
        pretty(guard_at.elapsed()),
        guard
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_default()
    );

    if edits > 0 {
        let (line, character) = end_position(&source);
        drain_frames(&rx);
        let mut merges = Vec::with_capacity(edits);
        let mut scenes = Vec::with_capacity(edits);
        for edit in 0..edits {
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
                        "text": format!("bench {edit} "),
                    }],
                }),
            );
            let diff = wait_for_diff_frame(&rx, sent_at)?;
            let compile = sent_at.elapsed();
            let client_at = Instant::now();
            let update = preview_update_from_bytes(&diff).context("invalid diff frame")?;
            session.merge(update);
            let metas = session.page_metas();
            let merge = client_at.elapsed();
            let scene_at = Instant::now();
            session
                .render_pages()
                .context("rebuilding after the edit")?;
            let scene = scene_at.elapsed();
            println!(
                "edit {edit:>2}: frame {} ({} pages) | merge {} | scene {} | rss {}",
                pretty(compile),
                metas.len(),
                pretty(merge),
                pretty(scene),
                rss()
            );
            merges.push(merge);
            scenes.push(scene);
        }
        report("  merge after edit", &merges, true);
        report("  incremental scene build", &scenes, true);
        println!("rss after {edits} edits: {}", rss());
    }

    stop.store(true, Ordering::Relaxed);
    server.notify(
        "workspace/executeCommand",
        json!({ "command": "tinymist.doKillPreview", "arguments": [task_id] }),
    );
    server.shutdown();
    Ok(())
}

// -- E2E: the pane's actual session thread ---------------------------------

fn run_e2e(tinymist: &str, doc: &Path, edits: usize) -> Result<()> {
    println!("\n=== e2e (SessionHandle, like the preview pane) ===");
    let source = fs::read_to_string(doc).with_context(|| format!("reading {}", doc.display()))?;
    let doc = doc.canonicalize()?;
    let uri = format!("file://{}", doc.display());
    let root = doc.parent().unwrap_or(Path::new("/")).display().to_string();
    let entry = doc.display().to_string();
    let task_id = format!("preview-bench-e2e-{}", std::process::id());
    let started = Instant::now();
    let (mut server, port) = start_preview(tinymist, &root, &uri, &source, &entry, &task_id)?;

    let (events_tx, events_rx) = smol::channel::unbounded();
    let session = SessionHandle::spawn(PreviewConnectionConfig::loopback(port), events_tx);

    let pages = wait_for_document(&events_rx)?;
    let document_at = started.elapsed();
    println!(
        "startup: Document with {} pages in {}",
        pages.len(),
        pretty(document_at)
    );
    if pages.is_empty() {
        bail!("the benchmark document produced no pages");
    }
    let mut current = pages;

    let index = 0;
    let scale = 2.0;
    let first = request_page(&session, &events_rx, &current, index, scale)?;
    println!("first render page {index} @{scale}x: {}", pretty(first));
    report_one("first render (scene build + raster)", first, true);

    for _ in 0..REQUEST_REPS {
        let zoom = request_page(&session, &events_rx, &current, index, 4.0)?;
        println!("zoom page {index} @4x (warm): {}", pretty(zoom));
        report_one("zoom request @4x (warm)", zoom, true);
    }

    let scroll_index = current.len() / 2;
    for _ in 0..REQUEST_REPS {
        let scroll = request_page(&session, &events_rx, &current, scroll_index, scale)?;
        println!(
            "scroll to page {scroll_index} @{scale}x: {}",
            pretty(scroll)
        );
        report_one("scroll request", scroll, true);
    }

    // Typing: measure both the compile (external) and the client's chain.
    let (line, character) = end_position(&source);
    let mut compile = Vec::with_capacity(edits);
    let mut client = Vec::with_capacity(edits);
    let mut total = Vec::with_capacity(edits);
    for edit in 0..edits {
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
                    "text": format!("bench {edit} "),
                }],
            }),
        );
        let changed = wait_for_changed_document(&events_rx, &current)?;
        let document_at = sent_at.elapsed();
        let target = changed
            .iter()
            .zip(&current)
            .find(|(page, old)| page.content_hash != old.content_hash)
            .map(|(page, _)| page.index)
            .unwrap_or(0);
        let rendered = request_page(&session, &events_rx, &changed, target, 1.0)?;
        compile.push(document_at);
        client.push(rendered);
        total.push(sent_at.elapsed());
        println!(
            "edit {edit:>2}: compile {} | client {} | total {} (page {target})",
            pretty(document_at),
            pretty(rendered),
            pretty(sent_at.elapsed())
        );
        current = changed;
    }
    report("  keystroke -> Document (external)", &compile, false);
    report("  Document -> pixels", &client, true);
    report("  keystroke -> pixels", &total, false);

    // The view clamps long before this, but the session must survive a direct
    // oversized request and report an error instead of hanging the GPU.
    let oversized_at = Instant::now();
    let oversized = request_page_result(&session, &events_rx, &current, 0, 100.0)?;
    anyhow::ensure!(
        oversized.is_err(),
        "an oversized render request must fail, not render"
    );
    println!(
        "oversized request: rejected in {}",
        pretty(oversized_at.elapsed())
    );

    println!("rss at end: {}", rss());
    session.shutdown();
    server.notify(
        "workspace/executeCommand",
        json!({ "command": "tinymist.doKillPreview", "arguments": [task_id] }),
    );
    server.shutdown();
    Ok(())
}

fn request_page(
    session: &SessionHandle,
    events: &Receiver<SessionEvent>,
    pages: &[typst_live_preview::PageMeta],
    index: usize,
    scale: f64,
) -> Result<Duration> {
    let started = Instant::now();
    let result = request_page_result(session, events, pages, index, scale)?;
    result.map_err(|error| anyhow::anyhow!("page {index} @{scale}x failed: {error}"))?;
    Ok(started.elapsed())
}

fn request_page_result(
    session: &SessionHandle,
    events: &Receiver<SessionEvent>,
    pages: &[typst_live_preview::PageMeta],
    index: usize,
    scale: f64,
) -> Result<Result<(), String>> {
    let page = pages.get(index).context("no such page")?;
    session.request_pages(scale, vec![(index, page.content_hash)]);
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        match recv_event(events, deadline)? {
            SessionEvent::Rendered {
                index: rendered, ..
            } if rendered == index => {
                return Ok(Ok(()));
            }
            SessionEvent::RenderFailed {
                index: failed,
                error,
                ..
            } if failed == index => return Ok(Err(error)),
            SessionEvent::Skipped { index: skipped } if skipped == index => {
                bail!("page {index} was skipped")
            }
            SessionEvent::Disconnected => bail!("session disconnected"),
            _ => {}
        }
    }
}

// -- Generated documents --------------------------------------------------

fn bench_dir() -> Result<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target/bench-docs");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn generate_docs(dir: &Path, pages: usize) -> Result<PathBuf> {
    let svg_path = dir.join("big.svg");
    if !svg_path.exists() {
        let mut svg = String::with_capacity(3_000_000);
        svg.push_str(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"6000\" height=\"4000\" \
             viewBox=\"0 0 6000 4000\">\n<rect width=\"6000\" height=\"4000\" fill=\"#101820\"/>\n",
        );
        for i in 0..30_000u64 {
            let x = (i * 7919) % 5900;
            let y = (i * 6271) % 3900;
            let radius = 2 + (i % 13);
            let hue = (i * 47) % 360;
            writeln!(
                svg,
                "<circle cx=\"{x}\" cy=\"{y}\" r=\"{radius}\" fill=\"hsl({hue},70%,55%)\" \
                 fill-opacity=\"0.35\"/>"
            )?;
        }
        svg.push_str("</svg>\n");
        fs::write(&svg_path, svg).context("writing big.svg")?;
    }

    let doc = dir.join(format!("bench-{pages}.typ"));
    let mut source = String::new();
    source.push_str("#set page(width: 210mm, height: 297mm, margin: 2cm)\n");
    source.push_str("#set text(size: 10pt)\n#set par(justify: true)\n");
    writeln!(source, "#for i in range({pages}) [")?;
    source.push_str("  #if i > 0 { pagebreak() }\n");
    source.push_str("  = Page #(i + 1)\n  #lorem(120)\n");
    source.push_str("  #if calc.rem(i, 6) == 0 [\n");
    source.push_str(
        "    #figure(image(\"big.svg\", width: 100%), caption: [Big vector figure #(i + 1)])\n",
    );
    source.push_str("  ]\n]\n");
    fs::write(&doc, source).context("writing the benchmark document")?;
    Ok(doc)
}

// -- Event helpers --------------------------------------------------------

fn wait_for_full_frame(rx: &mpsc::Receiver<PreviewEvent>) -> Result<Vec<u8>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for the first preview document frame");
        }
        match rx.recv_timeout(remaining) {
            // The initial document can arrive either as a full `new,` frame or
            // as a `diff-v1,` frame against an empty session. The render actor
            // races the `current` request, and when a compile wins it streams
            // the document as a diff; merging that into empty state yields the
            // same document, so accept whichever arrives first.
            Ok(PreviewEvent::Binary(bytes)) => {
                if preview_update_from_bytes(&bytes).is_some() {
                    return Ok(bytes);
                }
            }
            Ok(PreviewEvent::Connected) => eprintln!("data plane connected"),
            Ok(PreviewEvent::ConnectFailed(error)) => {
                eprintln!("data plane connection failed: {error}")
            }
            Ok(_) => {}
            Err(error) => bail!("waiting for the first Preview document frame: {error}"),
        }
    }
}

fn wait_for_diff_frame(rx: &mpsc::Receiver<PreviewEvent>, sent_at: Instant) -> Result<Vec<u8>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "timed out waiting for the diff frame (sent {:?})",
                sent_at.elapsed()
            );
        }
        match rx.recv_timeout(remaining) {
            Ok(PreviewEvent::Binary(bytes)) => {
                if preview_update_from_bytes(&bytes)
                    .is_some_and(|update| !update.reset_before_merge)
                {
                    return Ok(bytes);
                }
            }
            Ok(_) => {}
            Err(error) => bail!("waiting for the diff frame: {error}"),
        }
    }
}

fn drain_frames(rx: &mpsc::Receiver<PreviewEvent>) {
    while rx.try_recv().is_ok() {}
}

fn wait_for_document(events: &Receiver<SessionEvent>) -> Result<Vec<typst_live_preview::PageMeta>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        match recv_event(events, deadline)? {
            SessionEvent::Document { pages } => return Ok(pages),
            SessionEvent::Error(error) => bail!("preview error: {error}"),
            SessionEvent::Disconnected => bail!("preview disconnected"),
            _ => {}
        }
    }
}

fn wait_for_changed_document(
    events: &Receiver<SessionEvent>,
    previous: &[typst_live_preview::PageMeta],
) -> Result<Vec<typst_live_preview::PageMeta>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    loop {
        match recv_event(events, deadline)? {
            SessionEvent::Document { pages } => {
                let changed = pages
                    .iter()
                    .zip(previous)
                    .filter(|(page, old)| page.content_hash != old.content_hash)
                    .map(|(page, _)| page.index)
                    .collect::<Vec<_>>();
                if !changed.is_empty() || pages.len() != previous.len() {
                    return Ok(pages);
                }
            }
            SessionEvent::Error(error) => bail!("preview error: {error}"),
            SessionEvent::Disconnected => bail!("preview disconnected"),
            _ => {}
        }
    }
}

fn recv_event(events: &Receiver<SessionEvent>, deadline: Instant) -> Result<SessionEvent> {
    loop {
        match events.try_recv() {
            Ok(event) => return Ok(event),
            Err(TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for a preview event");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TryRecvError::Closed) => bail!("the preview session closed"),
        }
    }
}

// -- Measurement helpers --------------------------------------------------

fn sample_pages(count: usize) -> Vec<usize> {
    let mut pages = vec![0];
    if count > 1 {
        pages.push(count / 2);
    }
    if count > 2 {
        pages.push(count - 1);
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}

fn bins(width: u32, height: u32) -> u32 {
    width.div_ceil(256) * height.div_ceil(256)
}

fn report_one(label: &str, sample: Duration, budgeted: bool) {
    report(label, &[sample], budgeted);
}

fn report(label: &str, samples: &[Duration], budgeted: bool) {
    if samples.is_empty() {
        return;
    }
    println!(
        "{label:<34} p50 {:>8}   p95 {:>8}   max {:>8}   {}",
        pretty(percentile(samples, 0.50)),
        pretty(percentile(samples, 0.95)),
        pretty(percentile(samples, 1.0)),
        if budgeted {
            if samples.iter().all(|sample| *sample <= BUDGET) {
                "PASS <=30ms"
            } else {
                "FAIL >30ms"
            }
        } else {
            "external"
        }
    );
}

fn summary(samples: &[Duration]) -> String {
    format!(
        "p50 {} / max {}",
        pretty(percentile(samples, 0.50)),
        pretty(percentile(samples, 1.0))
    )
}

fn percentile(samples: &[Duration], percentile: f64) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted[index]
}

fn pretty(duration: Duration) -> String {
    format!("{:>6.1} ms", duration.as_secs_f64() * 1e3)
}

fn rss() -> String {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return "unknown".to_string();
    };
    let Some(line) = status.lines().find(|line| line.starts_with("VmRSS:")) else {
        return "unknown".to_string();
    };
    let kib = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    format!("{:.0} MiB", kib / 1024.0)
}

// -- Arguments ------------------------------------------------------------

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut tinymist = std::env::var("TINYMIST").unwrap_or_else(|_| "tinymist".to_string());
    let mut pages = 10;
    let mut edits = 5;
    let mut mode = Mode::Both;
    let mut doc = None;

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--tinymist" => tinymist = next_value(&mut args, "--tinymist")?,
            "--pages" => pages = next_value(&mut args, "--pages")?.parse()?,
            "--edits" => edits = next_value(&mut args, "--edits")?.parse()?,
            "--mode" => {
                mode = match next_value(&mut args, "--mode")?.as_str() {
                    "micro" => Mode::Micro,
                    "e2e" => Mode::E2e,
                    "both" => Mode::Both,
                    other => bail!("--mode must be micro, e2e, or both (got {other:?})"),
                }
            }
            "--doc" => doc = Some(PathBuf::from(next_value(&mut args, "--doc")?)),
            "--help" | "-h" => {
                println!(
                    "benchmark [--tinymist PATH] [--pages N] [--edits N] \
                     [--mode micro|e2e|both] [--doc FILE]"
                );
                std::process::exit(0);
            }
            other => bail!("unexpected argument {other:?} (try --help)"),
        }
    }

    anyhow::ensure!(pages > 0, "--pages must be positive");
    Ok(Args {
        tinymist,
        pages,
        edits,
        mode,
        doc,
    })
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next().with_context(|| format!("{flag} needs a value"))
}
