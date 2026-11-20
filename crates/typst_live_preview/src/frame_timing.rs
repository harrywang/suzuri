//! Opt-in frame-cost instrumentation for the preview pane.
//!
//! Zed requires that a frame cost no more than 8ms
//! (`docs/src/development/ui-checklist.md`: "Frames must take no more than 8ms
//! (120fps)"). `TypstPreviewView::render` is the only place the preview spends
//! UI-thread time proportional to the document, so it is what needs measuring.
//!
//! The probe is off unless `SUZURI_TYPST_FRAME_TIMING` is set, and it
//! accumulates into fixed-size histograms rather than keeping samples, so
//! switching it on cannot itself distort the numbers:
//!
//! ```sh
//! SUZURI_TYPST_FRAME_TIMING=1 suzuri
//! ```
//!
//! Each report covers the interval since the previous one, so the lines follow
//! what the window is doing (scrolling, zooming, idle) instead of averaging a
//! whole session together.

use std::time::{Duration, Instant};

/// The per-frame budget from the UI checklist (120fps).
pub(crate) const FRAME_BUDGET: Duration = Duration::from_millis(8);

const ENV_VAR: &str = "SUZURI_TYPST_FRAME_TIMING";
/// How often to log. Shorter intervals make the log unreadable; longer ones
/// blend distinct interactions together.
const REPORT_INTERVAL: Duration = Duration::from_secs(2);
/// Percentile resolution.
const BUCKET_MICROS: u64 = 50;
/// 512 buckets of 50µs covers 0–25.6ms, past any frame that still draws.
const BUCKETS: usize = 512;

/// Timestamps for one `render` call.
///
/// Building one takes three `Instant::now` calls, which on Linux are vDSO
/// reads costing tens of nanoseconds each — far below the noise floor of an
/// 8ms budget. Measuring unconditionally keeps `render` free of branches on
/// whether the probe is on.
pub(crate) struct FrameSpans {
    start: Instant,
    pages_started: Option<Instant>,
    pages_done: Option<Instant>,
    selection_started: Option<Instant>,
    selection_done: Option<Instant>,
}

impl FrameSpans {
    pub(crate) fn start() -> Self {
        Self {
            start: Instant::now(),
            pages_started: None,
            pages_done: None,
            selection_started: None,
            selection_done: None,
        }
    }

    /// Marks the start of the per-page element construction in `render`.
    pub(crate) fn pages_started(&mut self) {
        self.pages_started = Some(Instant::now());
    }

    /// Marks the end of the per-page element construction in `render`.
    pub(crate) fn pages_done(&mut self) {
        self.pages_done = Some(Instant::now());
    }

    /// Marks the start of the selection word count.
    pub(crate) fn selection_started(&mut self) {
        self.selection_started = Some(Instant::now());
    }

    /// Marks the end of the selection word count.
    pub(crate) fn selection_done(&mut self) {
        self.selection_done = Some(Instant::now());
    }

    fn page_span(&self) -> Option<Duration> {
        Some(self.pages_done? - self.pages_started?)
    }

    fn selection_span(&self) -> Option<Duration> {
        Some(self.selection_done? - self.selection_started?)
    }

    fn total(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Collects frame spans and logs percentiles every [`REPORT_INTERVAL`].
pub(crate) struct FrameTiming {
    render: Histogram,
    pages: Histogram,
    selection: Histogram,
    frames: u32,
    pages_in_document: usize,
    since: Instant,
}

impl FrameTiming {
    /// Returns a probe when `SUZURI_TYPST_FRAME_TIMING` enables one.
    pub(crate) fn from_env() -> Option<Self> {
        let value = std::env::var(ENV_VAR).ok()?;
        let enabled = !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off"
        );
        enabled.then(Self::new)
    }

    fn new() -> Self {
        Self {
            render: Histogram::default(),
            pages: Histogram::default(),
            selection: Histogram::default(),
            frames: 0,
            pages_in_document: 0,
            since: Instant::now(),
        }
    }

    /// Records one frame and reports once the interval has elapsed.
    pub(crate) fn record(&mut self, spans: &FrameSpans, pages_in_document: usize) {
        self.render.record(spans.total());
        if let Some(span) = spans.page_span() {
            self.pages.record(span);
        }
        if let Some(span) = spans.selection_span() {
            self.selection.record(span);
        }
        self.frames += 1;
        self.pages_in_document = pages_in_document;

        if self.since.elapsed() >= REPORT_INTERVAL {
            self.report();
            self.reset();
        }
    }

    fn report(&self) {
        let seconds = self.since.elapsed().as_secs_f64();
        log::info!(
            "typst live preview frame timing: {} frames in {:.1}s ({} pages) | render {} | pages {} | selection {}",
            self.frames,
            seconds,
            self.pages_in_document,
            self.render.summary(),
            self.pages.summary(),
            self.selection.summary(),
        );
    }

    fn reset(&mut self) {
        self.render.reset();
        self.pages.reset();
        self.selection.reset();
        self.frames = 0;
        self.since = Instant::now();
    }
}

/// A fixed-memory duration histogram.
struct Histogram {
    buckets: [u32; BUCKETS],
    /// Samples past the last bucket.
    overflow: u32,
    count: u32,
    over_budget: u32,
    max: Duration,
    sum: Duration,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            // Arrays past 32 elements do not implement `Default`.
            buckets: [0; BUCKETS],
            overflow: 0,
            count: 0,
            over_budget: 0,
            max: Duration::ZERO,
            sum: Duration::ZERO,
        }
    }
}

impl Histogram {
    fn record(&mut self, duration: Duration) {
        let bucket = (duration.as_micros() / u128::from(BUCKET_MICROS)) as usize;
        if bucket < BUCKETS {
            self.buckets[bucket] += 1;
        } else {
            self.overflow += 1;
        }
        self.count += 1;
        if duration > FRAME_BUDGET {
            self.over_budget += 1;
        }
        self.max = self.max.max(duration);
        self.sum += duration;
    }

    /// The lower bound of the bucket the `p` quantile falls in. Buckets are
    /// 50µs wide, which is well inside the resolution a frame budget needs.
    fn percentile(&self, p: f64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let target = ((self.count as f64) * p).ceil().max(1.0) as u32;
        let mut seen = 0u32;
        for (index, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                return Duration::from_micros(index as u64 * BUCKET_MICROS);
            }
        }
        self.max
    }

    fn summary(&self) -> String {
        if self.count == 0 {
            return "idle".to_owned();
        }
        let over_percent = self.over_budget as f64 / self.count as f64 * 100.0;
        format!(
            "p50 {:.2}ms p95 {:.2}ms p99 {:.2}ms max {:.2}ms, {} over {}ms ({:.1}%)",
            millis(self.percentile(0.5)),
            millis(self.percentile(0.95)),
            millis(self.percentile(0.99)),
            millis(self.max),
            self.over_budget,
            FRAME_BUDGET.as_millis(),
            over_percent,
        )
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_histogram_reports_idle() {
        assert_eq!(Histogram::default().summary(), "idle");
    }

    #[test]
    fn percentiles_track_recorded_durations() {
        let mut histogram = Histogram::default();
        // 100 samples: 1..=100ms is past the buckets, so use 0..2ms.
        for sample in 0..100 {
            histogram.record(Duration::from_micros(sample * 20));
        }

        assert_eq!(histogram.count, 100);
        // Buckets are 50µs wide, so a percentile reports the lower bound of the
        // bucket its sample falls in — never below, and at most one bucket off.
        let p50 = histogram.percentile(0.5);
        assert!(
            p50 >= Duration::from_micros(950) && p50 <= Duration::from_micros(1000),
            "p50 was {p50:?}"
        );
        let p95 = histogram.percentile(0.95);
        assert!(
            p95 >= Duration::from_micros(1850) && p95 <= Duration::from_micros(1900),
            "p95 was {p95:?}"
        );
        assert_eq!(histogram.max, Duration::from_micros(1980));
    }

    #[test]
    fn samples_past_the_buckets_land_in_overflow() {
        let mut histogram = Histogram::default();
        histogram.record(Duration::from_micros(BUCKETS as u64 * BUCKET_MICROS + 1));

        assert_eq!(histogram.count, 1);
        assert_eq!(histogram.overflow, 1);
        assert_eq!(histogram.over_budget, 1);
    }

    #[test]
    fn over_budget_counts_only_frames_past_eight_milliseconds() {
        let mut histogram = Histogram::default();
        histogram.record(Duration::from_micros(7_900));
        histogram.record(Duration::from_micros(8_100));

        assert_eq!(histogram.count, 2);
        assert_eq!(histogram.over_budget, 1);
    }

    #[test]
    fn the_probe_stays_off_without_the_variable() {
        // Only meaningful when the harness itself does not set the variable.
        if std::env::var(ENV_VAR).is_err() {
            assert!(FrameTiming::from_env().is_none());
        }
    }
}
