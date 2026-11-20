//! Preview session: protocol frames in, rasterized pages out.
//!
//! Two layers live here:
//!
//! * [`PreviewSession`] owns the two incremental clients that must stay in
//!   lockstep, [`IncrDocClient`] (the IR) and [`IncrVelloDocClient`] (the Vello
//!   scenes built from it). A `new,` frame must reset both before merging, or
//!   a reconnecting viewer renders stale pages.
//! * [`SessionHandle`] runs a session on a dedicated thread together with the
//!   WebSocket connection and the GPU rasterizer. The UI only exchanges page
//!   metadata, render requests, and finished pixels with it, so no Vello or IR
//!   type ever crosses into the GPUI thread.
//!
//! Rendering the IR and rasterizing it are separate steps on purpose: a frame
//! rebuilds only the pages whose content hash changed, while rasterization
//! happens on demand for the pages the viewport actually shows.

use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::RenderImage;
use reflexo::{
    hash::Fingerprint,
    vector::{
        incr::IncrDocClient,
        ir::{Module, Page, VecItem},
        stream::BytesModuleStream,
    },
};
use smol::channel::Sender;
use tokio::sync::mpsc::UnboundedSender;
use vello::{kurbo::Size, peniko::Color};

use crate::{
    PageAccessibility,
    connection::{self, PreviewConnectionConfig, PreviewEvent},
    incr::{IncrVelloDocClient, RenderedPage},
    protocol::{
        PreviewControl, PreviewUpdate, parse_control_frame, preview_update_from_bytes,
        source_point_message,
    },
    raster::SceneRasterizer,
    word_count::{WordCounter, WordsCount},
};

/// The geometry and content identity of one page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageMeta {
    /// Zero-based page index.
    pub index: usize,
    /// The page size in Typst page points.
    pub size: Size,
    /// The IR content hash the page was built from.
    pub content_hash: Fingerprint,
}

/// Something the session thread reports to the preview pane.
#[derive(Debug)]
pub enum SessionEvent {
    /// The preview WebSocket connected.
    Connected,
    /// The document changed. Contains every page, not a delta.
    Document { pages: Vec<PageMeta> },
    /// One page finished rasterizing.
    Rendered {
        /// The page index the image belongs to.
        index: usize,
        /// The content hash the image was rendered from.
        content_hash: Fingerprint,
        /// The device-pixel scale the image was rendered at.
        scale: f64,
        /// The finished page image.
        image: Arc<RenderImage>,
        /// Text runs and links in page coordinates, used for selection and
        /// click-through.
        accessibility: PageAccessibility,
    },
    /// A requested page was not rasterized because the document had already
    /// moved on. It only tells the pane that the request is no longer in
    /// flight.
    Skipped { index: usize },
    /// A requested page could not be rasterized. The view clears the request
    /// and remembers the failure so it does not retry the same content in a
    /// loop; a later edit renders normally.
    RenderFailed {
        /// The page index that failed.
        index: usize,
        /// The content hash the failed request was for.
        content_hash: Fingerprint,
        /// The device-pixel scale the request was for.
        scale: f64,
        /// A human-readable reason.
        error: String,
    },
    /// Tinymist asked the viewer to bring a page point into view (editor to
    /// preview synchronization, or another preview client).
    ScrollTo {
        /// Zero-based page index.
        page: usize,
        x: f32,
        y: f32,
    },
    /// Rendered word, character, space, and CJK counts for the document.
    WordCount(crate::word_count::WordsCount),
    /// A document outline forwarded by the host in a collaborative preview.
    Outline(serde_json::Value),
    /// The connection or renderer failed.
    Error(String),
    /// The preview stream ended.
    Disconnected,
}

#[derive(Clone)]
pub(crate) enum SessionInput {
    Event(PreviewEvent),
    Render {
        scale: f64,
        /// Pages to render, with the content hash the view wants.
        pages: Vec<(usize, Fingerprint)>,
    },
    /// An outline pushed by a collaborative preview's host.
    Outline(serde_json::Value),
    /// The host stopped the collaborative preview stream.
    Disconnect,
}

/// A handle to a running preview session thread.
///
/// Dropping the handle stops the thread, the WebSocket connection, and the
/// GPU work. The editor is expected to keep exactly one handle per pane.
pub struct SessionHandle {
    inputs: std_mpsc::Sender<SessionInput>,
    /// Text messages for the preview server, such as click-to-source requests.
    /// Collaborative sessions talk to the host through the project client
    /// instead.
    outbound: Option<UnboundedSender<String>>,
    /// Stops the WebSocket thread, which otherwise keeps reconnecting.
    connection_stop: Option<Arc<AtomicBool>>,
    closed: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionHandle {
    /// Starts a session that connects to `config` and reports to `events`.
    pub fn spawn(config: PreviewConnectionConfig, events: Sender<SessionEvent>) -> Self {
        let (inputs, input_rx) = std_mpsc::channel();
        let (outbound, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
        let frames_tx = inputs.clone();
        let closed = Arc::new(AtomicBool::new(false));

        let stop_connection = Arc::new(AtomicBool::new(false));

        // The WebSocket client needs a Tokio runtime; it gets its own thread so
        // the session loop can stay a plain blocking loop.
        let (net_tx, net_rx) = std_mpsc::channel();
        let connection_stop = stop_connection.clone();
        std::thread::Builder::new()
            .name("typst-live-preview-ws".into())
            .spawn(move || {
                if let Err(error) = connection::run(config, net_tx, outbound_rx, connection_stop) {
                    log::warn!("typst live preview connection stopped: {error}");
                }
            })
            .ok();

        // Frames and viewport requests share one queue, so a large compile
        // cannot starve the pages the user is looking at.
        std::thread::Builder::new()
            .name("typst-live-preview-frames".into())
            .spawn(move || {
                for event in net_rx {
                    if frames_tx.send(SessionInput::Event(event)).is_err() {
                        break;
                    }
                }
            })
            .ok();

        let thread_closed = closed.clone();
        let resync = outbound.clone();
        let thread = spawn_session_thread(input_rx, events, thread_closed, Some(resync));

        Self {
            inputs,
            outbound: Some(outbound),
            connection_stop: Some(stop_connection),
            closed,
            thread: Some(thread),
        }
    }

    /// Starts a session whose events are pushed by the caller.
    ///
    /// Collaborative previews use this: the host's frames arrive through the
    /// project client, so there is no WebSocket on this side. The caller keeps
    /// `inputs` and forwards frames, outlines, and disconnects into it.
    pub(crate) fn spawn_with_inputs(
        inputs: std_mpsc::Sender<SessionInput>,
        input_rx: std_mpsc::Receiver<SessionInput>,
        events: Sender<SessionEvent>,
    ) -> Self {
        let closed = Arc::new(AtomicBool::new(false));
        let thread_closed = closed.clone();
        let thread = spawn_session_thread(input_rx, events, thread_closed, None);

        Self {
            inputs,
            outbound: None,
            connection_stop: None,
            closed,
            thread: Some(thread),
        }
    }

    /// Stops the session and waits for its thread to release its GPU resources.
    ///
    /// Dropping a handle only signals the thread to stop; this is for callers
    /// that need the render thread to finish before the process exits.
    pub fn shutdown(mut self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(stop) = &self.connection_stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }

    /// Asks the session to rasterize `pages` at `scale` device pixels per point.
    ///
    /// Each page carries the content hash the view is currently showing. A
    /// request that lost a race with a newer document is dropped instead of
    /// spending GPU time on a frame nobody will display.
    ///
    /// Requests are best-effort: if the session has shut down, the viewport
    /// will be torn down anyway.
    pub fn request_pages(&self, scale: f64, pages: Vec<(usize, Fingerprint)>) {
        if pages.is_empty() {
            return;
        }
        self.inputs.send(SessionInput::Render { scale, pages }).ok();
    }

    /// Asks Tinymist to resolve a click on a page to its source location.
    ///
    /// The point is in Typst points from the page's top-left. The server
    /// answers with an LSP `window/showDocument` request, which the editor
    /// turns into "open the file and select the range".
    pub fn request_source_jump(&self, page_index: usize, x: f64, y: f64) {
        if let Some(outbound) = &self.outbound {
            outbound.send(source_point_message(page_index, x, y)).ok();
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(stop) = &self.connection_stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Walks the IR in paint order and feeds rendered text into `counter`.
///
/// `stack` only guards against cycles; shared groups are counted at every
/// occurrence, just as Tinymist's text export visits them.
fn count_ir_item(
    module: &Module,
    id: Fingerprint,
    counter: &mut WordCounter,
    stack: &mut HashSet<Fingerprint>,
    depth: usize,
) {
    // Frames can nest deeply, but a cycle or absurdly deep group should not
    // crash the session thread.
    if depth > 512 {
        return;
    }

    let Some(item) = module.get_item(&id) else {
        return;
    };

    match item {
        VecItem::Text(text) => counter.push_str(text.content.content.as_ref()),
        VecItem::ContentHint(character) => counter.push(*character),
        VecItem::Group(group) => {
            if !stack.insert(id) {
                return;
            }
            for (_, child) in group.0.iter() {
                count_ir_item(module, *child, counter, stack, depth + 1);
            }
            stack.remove(&id);
        }
        VecItem::Item(transformed) => {
            if !stack.insert(id) {
                return;
            }
            count_ir_item(module, transformed.1, counter, stack, depth + 1);
            stack.remove(&id);
        }
        VecItem::Labelled(labelled) => {
            if !stack.insert(id) {
                return;
            }
            count_ir_item(module, labelled.1, counter, stack, depth + 1);
            stack.remove(&id);
        }
        VecItem::None
        | VecItem::Image(_)
        | VecItem::Link(_)
        | VecItem::Path(_)
        | VecItem::Color32(_)
        | VecItem::Gradient(_)
        | VecItem::Pattern(_)
        | VecItem::ColorTransform(_)
        | VecItem::SizedRawHtml(_)
        | VecItem::Html(_) => {}
    }
}

fn spawn_session_thread(
    inputs: std_mpsc::Receiver<SessionInput>,
    events: Sender<SessionEvent>,
    closed: Arc<AtomicBool>,
    resync: Option<UnboundedSender<String>>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("typst-live-preview".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            // A panic anywhere in the session (Vello asserts on degenerate
            // geometry, for instance) must not leave the pane waiting forever
            // for a `Disconnected` that never comes. Report it and close the
            // stream.
            let panic_events = events.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_session(inputs, events, closed, resync)
            }));
            if let Err(payload) = result {
                let message = panic_message(payload.as_ref());
                log::error!("typst live preview: session thread panicked: {message}");
                panic_events
                    .send_blocking(SessionEvent::Error(format!(
                        "The Typst preview renderer stopped: {message}"
                    )))
                    .ok();
                panic_events.send_blocking(SessionEvent::Disconnected).ok();
            }
        })
        .expect("failed to spawn the Typst live preview thread")
}

/// The message carried by a caught panic.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Tells the pane that every page of a failed batch is no longer in flight.
fn report_failed_batch(
    events: &Sender<SessionEvent>,
    requested: &[(usize, Fingerprint)],
    scale: f64,
    error: &str,
) {
    for (index, content_hash) in requested {
        events
            .send_blocking(SessionEvent::RenderFailed {
                index: *index,
                content_hash: *content_hash,
                scale,
                error: error.to_string(),
            })
            .ok();
    }
}

fn run_session(
    inputs: std_mpsc::Receiver<SessionInput>,
    events: Sender<SessionEvent>,
    closed: Arc<AtomicBool>,
    resync: Option<UnboundedSender<String>>,
) {
    let mut session = PreviewSession::new();
    let mut rasterizer = SceneRasterizer::new();
    let mut rendered: Vec<RenderedPage> = Vec::new();
    let mut last_metas: Vec<PageMeta> = Vec::new();
    let mut last_word_count = None;
    // Scenes are only rebuilt when the document changed. A zoom or a scroll
    // requests the same content at another scale, and rebuilding every scene
    // (and recounting the words) would delay the first page it is waiting for.
    let mut scenes_dirty = true;
    let mut deferred: VecDeque<SessionInput> = VecDeque::new();

    loop {
        let input = match deferred.pop_front() {
            Some(input) => input,
            None => match inputs.recv_timeout(Duration::from_millis(100)) {
                Ok(input) => input,
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    if closed.load(Ordering::Relaxed) {
                        break;
                    }
                    continue;
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };

        match input {
            SessionInput::Event(PreviewEvent::Connected) => {
                events.send_blocking(SessionEvent::Connected).ok();
                // Get the GPU device and pipelines ready while Tinymist is
                // still compiling the first document.
                let warm_up_at = Instant::now();
                match rasterizer.warm_up() {
                    Ok(()) => log::debug!(
                        "typst live preview: rasterizer ready in {:?}",
                        warm_up_at.elapsed()
                    ),
                    Err(error) => log::warn!(
                        "typst live preview: cannot prepare the rasterizer yet: {error:#}"
                    ),
                }
            }
            SessionInput::Event(PreviewEvent::ConnectFailed(error)) => {
                events
                    .send_blocking(SessionEvent::Error(format!(
                        "Cannot reach the Typst preview server: {error}"
                    )))
                    .ok();
            }
            SessionInput::Event(PreviewEvent::Text(_)) => {}
            SessionInput::Outline(outline) => {
                events.send_blocking(SessionEvent::Outline(outline)).ok();
            }
            SessionInput::Disconnect => {
                events.send_blocking(SessionEvent::Disconnected).ok();
                break;
            }
            SessionInput::Event(PreviewEvent::Binary(bytes)) => {
                let mut merged = 0;
                let mut next = Some(bytes);
                while let Some(bytes) = next.take() {
                    if let Some(control) = parse_control_frame(&bytes) {
                        // Control frames share the data plane with vector
                        // updates. They do not change the document, so they
                        // neither reset nor count toward the coalescing bound.
                        let point = match control {
                            PreviewControl::Viewport(point) => point,
                            PreviewControl::Jumps(points) => points[0],
                        };
                        events
                            .send_blocking(SessionEvent::ScrollTo {
                                page: point.page,
                                x: point.x,
                                y: point.y,
                            })
                            .ok();
                    } else if let Some(update) = preview_update_from_bytes(&bytes) {
                        if update.reset_before_merge {
                            // The session reset its document; scenes from the
                            // previous connection are no longer valid.
                            rendered.clear();
                        }
                        session.merge(update);
                        merged += 1;
                    } else {
                        log::debug!("ignoring an unknown preview frame ({} bytes)", bytes.len());
                    }

                    if merged >= MAX_COALESCED_FRAMES {
                        break;
                    }
                    next = match inputs.try_recv() {
                        Ok(SessionInput::Event(PreviewEvent::Binary(bytes))) => Some(bytes),
                        Ok(other) => {
                            deferred.push_back(other);
                            None
                        }
                        Err(_) => None,
                    };
                }

                if merged == 0 {
                    continue;
                }
                scenes_dirty = true;

                // The pane only needs the page list here. Building scenes is
                // deferred until the viewport actually asks for pixels, so a
                // burst of keystrokes does not rebuild a page per frame.
                let metas = session.page_metas();
                if metas != last_metas {
                    last_metas = metas.clone();
                    events
                        .send_blocking(SessionEvent::Document { pages: metas })
                        .ok();
                }
            }
            SessionInput::Render { scale, pages } => {
                if scenes_dirty {
                    // Scenes may have moved on since the last request; rebuild
                    // only the pages whose content hash changed.
                    let scenes_at = Instant::now();
                    let scenes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        session.render_pages()
                    }));
                    match scenes {
                        Ok(Ok(scenes)) => {
                            rendered = scenes;
                            scenes_dirty = false;
                            let word_count = session.words_count();
                            if last_word_count != Some(word_count) {
                                last_word_count = Some(word_count);
                                events
                                    .send_blocking(SessionEvent::WordCount(word_count))
                                    .ok();
                            }
                        }
                        Ok(Err(error)) => {
                            let error = format!("{error:#}");
                            report_failed_batch(&events, &pages, scale, &error);
                            continue;
                        }
                        Err(payload) => {
                            let error = format!(
                                "building the preview scene panicked: {}",
                                panic_message(payload.as_ref())
                            );
                            log::error!("typst live preview: {error}");

                            // The incremental IR is no longer trustworthy: a
                            // missing item or font index is what usually makes
                            // the renderer panic. Drop the client state and ask
                            // the server for a fresh full document, otherwise
                            // every later frame merges onto the broken state.
                            session = PreviewSession::new();
                            rendered.clear();
                            last_metas.clear();
                            if let Some(resync) = &resync {
                                resync.send("current".to_string()).ok();
                            }

                            report_failed_batch(&events, &pages, scale, &error);
                            continue;
                        }
                    }
                    log::debug!(
                        "typst live preview: rebuilt scenes in {:?}",
                        scenes_at.elapsed()
                    );
                }

                let background = session.background_color().unwrap_or(Color::WHITE);
                for (index, expected_hash) in pages {
                    let Some(page) = rendered.get(index) else {
                        continue;
                    };
                    // The document moved on while this request sat in the
                    // queue; the view will ask for the new content itself.
                    if page.content_hash != expected_hash {
                        events.send_blocking(SessionEvent::Skipped { index }).ok();
                        continue;
                    }
                    let raster_at = Instant::now();
                    let rasterized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        rasterizer.render_page(&page.scene, page.size, scale, background)
                    }));
                    match rasterized {
                        Ok(Ok(image)) => {
                            let raster_time = raster_at.elapsed();
                            let (width, height) = (image.width, image.height);
                            let convert_at = Instant::now();
                            let render_image = image.into_render_image();
                            log::debug!(
                                "typst live preview: page {index} {width}x{height} rasterized at \
                                 {scale:.2}x in {raster_time:?} (+{:?} to image)",
                                convert_at.elapsed()
                            );
                            events
                                .send_blocking(SessionEvent::Rendered {
                                    index,
                                    content_hash: page.content_hash,
                                    scale,
                                    image: render_image,
                                    accessibility: page.accessibility.clone(),
                                })
                                .ok();
                        }
                        Ok(Err(error)) => {
                            let error = format!("{error:#}");
                            log::error!(
                                "typst live preview: page {index} failed to rasterize: {error}"
                            );
                            // A failed texture allocation or a poll timeout can
                            // leave the device unusable. Start the next page
                            // from a fresh device instead of retrying a broken
                            // one until Suzuri restarts.
                            rasterizer = SceneRasterizer::new();
                            events
                                .send_blocking(SessionEvent::RenderFailed {
                                    index,
                                    content_hash: page.content_hash,
                                    scale,
                                    error,
                                })
                                .ok();
                        }
                        Err(payload) => {
                            // A panic in Vello can leave its wgpu device (and
                            // the renderer that was taken out for this call)
                            // unusable. Recreate the rasterizer so the next
                            // request starts from a fresh device instead of
                            // failing until Suzuri restarts. The session and IR
                            // stay alive: one bad page must not take the
                            // preview down.
                            let error = format!(
                                "the Vello renderer panicked: {}",
                                panic_message(payload.as_ref())
                            );
                            log::error!("typst live preview: {error}");
                            rasterizer = SceneRasterizer::new();
                            events
                                .send_blocking(SessionEvent::RenderFailed {
                                    index,
                                    content_hash: page.content_hash,
                                    scale,
                                    error,
                                })
                                .ok();
                        }
                    }
                }
            }
        }

        if closed.load(Ordering::Relaxed) {
            break;
        }
    }

    events.send_blocking(SessionEvent::Disconnected).ok();
}

/// The preview server emits one frame per compilation. When frames pile up
/// behind a slow render, merging a batch before rebuilding scenes avoids
/// rebuilding an intermediate page for every keystroke.
const MAX_COALESCED_FRAMES: usize = 16;

/// A snapshot of the state a [`PreviewSession`] retains.
///
/// This is diagnostic: the counts should stay bounded by the current document
/// and its page count, not grow with the number of frames merged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreviewSessionStats {
    /// Vector items retained by the incremental IR module.
    pub module_items: usize,
    /// Fonts retained by the IR module.
    pub module_fonts: usize,
    /// Glyph slots allocated across all fonts.
    pub module_glyphs: usize,
    /// Glyphs accumulated by `IncrDocClient` itself.
    pub pending_glyphs: usize,
    /// Source-mapping nodes retained for click-to-source.
    pub source_mapping_nodes: usize,
    /// Cached vector pages.
    pub cached_pages: usize,
    /// Cached flushed Vello scenes.
    pub flushed_pages: usize,
}

/// The client-side state of one Typst live preview.
#[derive(Default)]
pub struct PreviewSession {
    doc: IncrDocClient,
    vello: IncrVelloDocClient,
}

impl PreviewSession {
    /// Creates a session with no document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops all state. The next frame must be a full `new,` document.
    pub fn reset(&mut self) {
        self.doc = IncrDocClient::default();
        self.vello.reset();
    }

    /// Merges one data-plane frame.
    ///
    /// A full-current frame resets the session first. Rendering is separate so
    /// callers can coalesce several diffs into one render pass.
    pub fn merge(&mut self, update: PreviewUpdate<'_>) {
        if update.reset_before_merge {
            self.reset();
        }

        let delta = BytesModuleStream::from_slice(update.payload).checkout_owned();
        self.doc.merge_delta(delta);

        // `IncrDocClient::glyphs` is write-only in this version; the renderer
        // reads outlines from `doc.module.fonts`. It appends every glyph in
        // every delta and is never pruned, so a long editing session grows it
        // without bound. Drop the unused copy instead of carrying it forever.
        self.doc.glyphs = Vec::new();
    }

    /// Renders every page of the current document.
    ///
    /// Pages whose content hash did not change reuse their cached Vello scene.
    pub fn render_pages(&mut self) -> Result<Vec<RenderedPage>> {
        Ok(self.vello.render_pages_with_accessibility(&mut self.doc)?)
    }

    /// The current page sizes and content hashes, without building scenes.
    ///
    /// This is what lets the pane lay pages out on every keystroke while the
    /// expensive scene construction waits for the viewport to settle.
    pub fn page_metas(&mut self) -> Vec<PageMeta> {
        self.vello
            .page_metas(&mut self.doc)
            .into_iter()
            .enumerate()
            .map(|(index, (size, content_hash))| PageMeta {
                index,
                size,
                content_hash,
            })
            .collect()
    }

    /// The current page geometry, or an empty slice before the first render.
    pub fn pages(&self) -> &[Page] {
        self.doc
            .layout
            .as_ref()
            .and_then(|layout| layout.pages_meta())
            .unwrap_or(&[])
    }

    /// The number of pages in the current document.
    pub fn page_count(&self) -> usize {
        self.pages().len()
    }

    /// The background color the document asks for, if any.
    pub fn background_color(&self) -> Option<Color> {
        self.vello.background_color()
    }

    /// A snapshot of the memory-bearing state retained by this session.
    pub fn stats(&self) -> PreviewSessionStats {
        let (cached_pages, flushed_pages) = self.vello.vec2vello.cached_page_counts();
        PreviewSessionStats {
            module_items: self.doc.doc.module.items.len(),
            module_fonts: self.doc.doc.module.fonts.len(),
            module_glyphs: self
                .doc
                .doc
                .module
                .fonts
                .iter()
                .map(|font| font.glyphs.len())
                .sum(),
            pending_glyphs: self.doc.glyphs.len(),
            source_mapping_nodes: self.doc.source_mapping_data.len(),
            cached_pages,
            flushed_pages,
        }
    }

    /// Counts words in the rendered document, matching Tinymist's counter.
    ///
    /// This walks the IR's text items and content hints, so it includes text
    /// from every page and from included files, but no source markup.
    pub fn words_count(&self) -> WordsCount {
        let Some(layout) = self.doc.layout.as_ref() else {
            return WordsCount::default();
        };
        let module = &self.doc.doc.module;
        let Some(pages) = layout.pages(module) else {
            return WordsCount::default();
        };

        let mut counter = WordCounter::new();
        let mut stack = HashSet::new();
        for page in pages.pages() {
            count_ir_item(module, page.content, &mut counter, &mut stack, 0);
        }
        counter.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::mpsc, time::Duration};

    use super::*;
    use crate::connection::PreviewEvent;

    const EVENT_TIMEOUT: Duration = Duration::from_secs(15);

    fn recv_event(events: &smol::channel::Receiver<SessionEvent>) -> SessionEvent {
        // `smol::Timer::after` is disallowed by the workspace lints (it makes
        // tests non-deterministic), so the timeout lives on a std channel.
        let events = events.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(smol::block_on(events.recv()).ok()).ok();
        });
        rx.recv_timeout(EVENT_TIMEOUT)
            .expect("timed out waiting for a session event")
            .expect("the preview session stopped")
    }

    /// The guest half of a collaborative preview: frames arrive as relayed
    /// bytes instead of over a WebSocket and must still render a page.
    #[test]
    fn relayed_frames_render_through_a_session_with_external_inputs() {
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/preview-new.bin");
        let frame = std::fs::read(&fixture).expect("the recorded frame");

        let (inputs, input_rx) = mpsc::channel();
        let (events, events_rx) = smol::channel::unbounded();
        let session = SessionHandle::spawn_with_inputs(inputs.clone(), input_rx, events);

        inputs
            .send(SessionInput::Event(PreviewEvent::Connected))
            .unwrap();
        inputs
            .send(SessionInput::Event(PreviewEvent::Binary(frame)))
            .unwrap();

        assert!(matches!(recv_event(&events_rx), SessionEvent::Connected));
        let pages = match recv_event(&events_rx) {
            SessionEvent::Document { pages } => pages,
            other => panic!("expected page metadata, got {other:?}"),
        };
        assert_eq!(pages.len(), 2, "the fixture has two pages");

        inputs
            .send(SessionInput::Render {
                scale: 1.0,
                pages: vec![(0, pages[0].content_hash)],
            })
            .unwrap();

        loop {
            match recv_event(&events_rx) {
                SessionEvent::Rendered {
                    index,
                    scale,
                    accessibility,
                    ..
                } => {
                    assert_eq!(index, 0);
                    assert_eq!(scale, 1.0);
                    assert!(
                        !accessibility.text_runs().is_empty(),
                        "the rendered page should carry selectable text"
                    );
                    break;
                }
                SessionEvent::Error(error) => panic!("rendering failed: {error}"),
                _ => {}
            }
        }

        // A second request at another scale must rasterize from the cached
        // scenes: that is the path a zoom takes, where nothing changed except
        // the scale.
        inputs
            .send(SessionInput::Render {
                scale: 2.0,
                pages: vec![(0, pages[0].content_hash)],
            })
            .unwrap();
        loop {
            match recv_event(&events_rx) {
                SessionEvent::Rendered { index, scale, .. } => {
                    assert_eq!(index, 0);
                    assert_eq!(scale, 2.0);
                    break;
                }
                SessionEvent::Error(error) => panic!("rendering failed: {error}"),
                _ => {}
            }
        }

        session.shutdown();
    }
}
