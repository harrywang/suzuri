//! Measures the latency of the live-preview input path.
//!
//! Everything the preview does in response to a keystroke is measured here so a
//! regression shows up as a number instead of a hitch:
//!
//! 1. parse + merge one `diff-v1,` frame and compute page metadata — this is
//!    what the session thread must keep under a millisecond,
//! 2. rebuild the Vello scenes for the settled document — the debounced path,
//! 3. rasterize a page on the GPU and convert it for GPUI.
//!
//! The frames are the recorded ones from `tests/fixtures`, so the numbers are
//! reproducible without a running Tinymist:
//!
//! ```sh
//! cargo run --release -p typst_live_preview --example input_latency
//! ```
//!
//! Always benchmark `--release`; debug builds of the IR decoder are an order of
//! magnitude slower and say nothing about shipped latency.
//!
//! GPU numbers are only comparable across runs on the same machine: a VM or
//! sandbox without `/dev/dri` falls back to a software Vulkan device, which is
//! one to two orders of magnitude slower than a real GPU. The adapters Vello
//! can see are printed with the timings.

use std::{fs, path::PathBuf, time::Duration, time::Instant};

use anyhow::{Context as _, Result};
use typst_live_preview::{PreviewSession, SceneRasterizer, protocol::preview_update_from_bytes};
use vello::peniko::Color;

/// How many simulated keystrokes to measure.
const KEYSTROKES: usize = 500;
/// Scene rebuilding is the expensive half, so it gets fewer samples.
const RENDERS: usize = 25;
/// GPU rasterization samples, after the device is warm.
const RASTERS: usize = 25;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn report(label: &str, mut samples: Vec<Duration>, pages: usize) {
    if samples.is_empty() {
        return;
    }
    samples.sort_unstable();
    let at = |percentile: f64| {
        let index = ((samples.len() - 1) as f64 * percentile).round() as usize;
        samples[index]
    };
    let pages = if pages == 1 {
        "1 page".to_string()
    } else {
        format!("{pages} pages")
    };
    println!(
        "{label:<34} p50 {:>9.1} us   p95 {:>9.1} us   max {:>9.1} us   ({pages})",
        at(0.50).as_secs_f64() * 1e6,
        at(0.95).as_secs_f64() * 1e6,
        at(1.0).as_secs_f64() * 1e6,
    );
}

fn main() -> Result<()> {
    let new_frame = fs::read(fixture("preview-new.bin")).context("reading preview-new.bin")?;
    let diff_frame = fs::read(fixture("preview-diff.bin")).context("reading preview-diff.bin")?;

    let mut session = PreviewSession::new();

    // -- Cold open: full document --
    let parse_new = Instant::now();
    let update = preview_update_from_bytes(&new_frame).context("preview-new.bin is not a frame")?;
    let parsed = parse_new.elapsed();
    let merge_new = Instant::now();
    session.merge(update);
    let merged = merge_new.elapsed();
    let metas_at = Instant::now();
    let metas = session.page_metas();
    let metas_time = metas_at.elapsed();
    println!(
        "cold open: parse {:.1} us, merge {:.1} us, metas {:.1} us ({} pages)",
        parsed.as_secs_f64() * 1e6,
        merged.as_secs_f64() * 1e6,
        metas_time.as_secs_f64() * 1e6,
        metas.len()
    );
    let page_count = metas.len();

    // -- Typing: parse + merge + metas for every diff frame --
    let mut keystroke_samples = Vec::with_capacity(KEYSTROKES);
    let mut parse_samples = Vec::with_capacity(KEYSTROKES);
    let mut merge_samples = Vec::with_capacity(KEYSTROKES);
    let mut metas_samples = Vec::with_capacity(KEYSTROKES);
    for _ in 0..KEYSTROKES {
        let parse_at = Instant::now();
        let update =
            preview_update_from_bytes(&diff_frame).context("preview-diff.bin is not a frame")?;
        parse_samples.push(parse_at.elapsed());

        let merge_at = Instant::now();
        session.merge(update);
        merge_samples.push(merge_at.elapsed());

        let metas_at = Instant::now();
        let metas = session.page_metas();
        metas_samples.push(metas_at.elapsed());

        keystroke_samples.push(parse_at.elapsed());
        assert_eq!(
            metas.len(),
            page_count,
            "replaying the fixture diff must not change the page count"
        );
    }
    println!("\nper keystroke (parse + merge + page metadata):");
    report("  total", keystroke_samples, page_count);
    report("  parse frame", parse_samples, page_count);
    report("  merge into IR", merge_samples, page_count);
    report("  page metadata", metas_samples, page_count);

    // -- Settled: rebuild Vello scenes (only pages whose hash changed) --
    // The same diff twice is idempotent, so a diff always follows a full frame:
    // the `new,` frame rebuilds every scene, the diff rebuilds just one page.
    let mut full_render_samples = Vec::with_capacity(RENDERS);
    let mut diff_render_samples = Vec::with_capacity(RENDERS);
    for _ in 0..RENDERS {
        let new_update = preview_update_from_bytes(&new_frame).context("preview-new.bin")?;
        session.merge(new_update);
        let render_at = Instant::now();
        session.render_pages().context("rendering the pages")?;
        full_render_samples.push(render_at.elapsed());

        let diff_update = preview_update_from_bytes(&diff_frame).context("preview-diff.bin")?;
        session.merge(diff_update);
        let render_at = Instant::now();
        session.render_pages().context("rendering the pages")?;
        diff_render_samples.push(render_at.elapsed());
    }
    println!("\nsettled document (scene rebuild):");
    report("  all pages after new,", full_render_samples, page_count);
    report(
        "  one changed page after diff",
        diff_render_samples,
        page_count,
    );

    // -- GPU: rasterize and convert one page --
    let pages = session.render_pages().context("rendering the pages")?;
    let page = pages.first().context("the fixture has no pages")?;
    let mut rasterizer = SceneRasterizer::new();

    println!("\nGPU adapters visible to Vello:");
    let instance = vello::wgpu::Instance::new(&vello::wgpu::InstanceDescriptor::default());
    for adapter in instance.enumerate_adapters(vello::wgpu::Backends::all()) {
        let info = adapter.get_info();
        println!(
            "  {} ({:?}, {:?})",
            info.name, info.device_type, info.backend
        );
    }
    let scale = std::env::var("PREVIEW_SCALE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(2.0);
    println!(
        "\npage 0 raster target at {scale}x: {}x{} px",
        (page.size.width * scale).round() as u32,
        (page.size.height * scale).round() as u32
    );

    let warmup_at = Instant::now();
    match rasterizer.render_page(&page.scene, page.size, scale, Color::WHITE) {
        Ok(_) => println!(
            "  device init + first page: {:.1} ms",
            warmup_at.elapsed().as_secs_f64() * 1e3
        ),
        Err(error) => {
            println!("\nskipping GPU timings: {error:#}");
            return Ok(());
        }
    }

    let mut raster_samples = Vec::with_capacity(RASTERS);
    let mut convert_samples = Vec::with_capacity(RASTERS);
    for _ in 0..RASTERS {
        let raster_at = Instant::now();
        let rendered = rasterizer
            .render_page(&page.scene, page.size, scale, Color::WHITE)
            .context("rasterizing page 0")?;
        raster_samples.push(raster_at.elapsed());

        let convert_at = Instant::now();
        let image = rendered.into_render_image();
        convert_samples.push(convert_at.elapsed());
        drop(image);
    }
    report("  rasterize", raster_samples, 1);
    report("  RGBA -> GPUI image", convert_samples, 1);

    Ok(())
}
