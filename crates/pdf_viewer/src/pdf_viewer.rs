//! PDF viewer built on hayro, rendering pages in workspace tabs with zoom,
//! continuous scroll, and glyph-accurate text selection.
//!
//! Originally written by David Turnbull (@dsturnbull) as a contribution to
//! Zed (zed-industries/zed#51040, closed there as out of scope for a code
//! editor); adopted into Suzuri, where a research editor makes it a core
//! surface. hayro text-extraction support comes from the same author's
//! hayro fork.

mod pdf_item;
mod pdf_renderer;
mod selection;
mod toolbar;

use hayro::vello_cpu::color::palette::css::WHITE;

pub use pdf_item::{PdfItem, is_pdf_path};
pub use toolbar::PdfViewToolbarControls;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use editor::{Editor, EditorEvent, actions::SelectAll};
use file_icons::FileIcons;
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, Font,
    IntoElement, MouseButton, ParentElement, Pixels, Point, Render, RenderImage, ScrollDelta,
    ScrollHandle, ScrollWheelEvent, SharedString, Styled, Subscription, Task, TextAlign,
    TextStyleRefinement, Window, actions, div, img, point, px,
};
use project::Project;
use ui::WithScrollbar;
use ui::prelude::*;
use workspace::{
    Pane, ToolbarItemLocation, WorkspaceId,
    invalid_item_view::InvalidItemView,
    item::{HighlightedText, Item, ProjectItem, TabContentParams},
};

use pdf_renderer::{PageDimensions, PageTextLayout, PdfMetadata};

const PAGE_GAP_PX: f32 = 20.0;
/// Grey canvas showing on each side of a page at zoom 1.0, so a page reads
/// as a sheet of paper on a desk rather than filling the pane edge to edge.
const PAGE_CANVAS_MARGIN_PX: f32 = 24.0;
const RENDER_BUFFER_PAGES: usize = 2;
const SCREEN_SCALE_FACTOR: f32 = 2.0;
// Metal texture atlas tiles cannot exceed 16384px in either dimension.
const MAX_GPU_TILE_PX: f32 = 16384.0;

const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 20.0;
const ZOOM_STEP: f32 = 1.1;
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;
const SCROLL_ZOOM_SENSITIVITY: f32 = 0.01;

const RENDER_DEBOUNCE: Duration = Duration::from_millis(200);

actions!(
    pdf_viewer,
    [
        /// Zoom in the PDF.
        ZoomIn,
        /// Zoom out the PDF.
        ZoomOut,
        /// Reset zoom to 100%.
        ResetZoom,
        /// Fit the PDF to the view width.
        FitToView,
        /// Zoom to actual size (100%).
        ZoomToActualSize,
        /// Copy all document text to the clipboard.
        CopyDocumentText,
        /// Type a page number to jump to that page.
        GoToPage,
    ]
);

pub enum PdfViewEvent {
    TitleChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPosition {
    page: usize,
    glyph_index: usize,
}

pub struct PdfViewer {
    pdf_item: Entity<PdfItem>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    metadata: Option<PdfMetadata>,
    /// Only ever mutated through `place_rendered_page` and
    /// `take_rendered_pages`, so no page's atlas tile can be orphaned.
    rendered_pages: RenderedPages,
    pages_in_flight: HashSet<usize>,
    cancel_token: Arc<AtomicBool>,
    /// Loading metadata gets a field of its own rather than sharing
    /// `render_task`. `request_visible_pages` runs from `render`, so a reload
    /// notifying the window would otherwise overwrite the metadata task it had
    /// just spawned and cancel it, stranding the view on the replaced
    /// document's page count.
    metadata_task: Task<()>,
    render_task: Task<()>,
    render_debounce: Task<()>,
    render_error: Option<SharedString>,
    zoom_level: f32,
    render_scale: f32,
    /// The window's device-pixel ratio, captured each frame in `render`.
    display_scale: f32,
    pan_x: Pixels,
    text_layouts: HashMap<usize, PageTextLayout>,
    text_extraction_task: Task<()>,
    selection_start: Option<TextPosition>,
    selection_end: Option<TextPosition>,
    is_selecting: bool,
    /// The page number in the status bar, editable to jump to a page.
    page_input: Entity<Editor>,
    /// The page last written into `page_input`. `None` makes the next frame
    /// write the current page, which is how a half-typed number is discarded.
    shown_page: Option<usize>,
    _page_input_subscription: Subscription,
}

/// The rendered page cache, keyed by page index, holding the scale each page
/// was rasterised at alongside its image.
type RenderedPages = HashMap<usize, (f32, Arc<RenderImage>)>;

/// Places a freshly rendered page, returning the image it displaced so the
/// caller can free that image's sprite-atlas tile with `App::drop_image`.
///
/// A `RenderImage`'s GPU tile is keyed by a process-unique id and lives until an
/// explicit `drop_image`; dropping the `Arc` frees only the CPU bitmap, and the
/// Metal atlas has no eviction. A page tile fills a whole atlas texture, so a
/// page left behind holds that texture for the window's lifetime: about 10.5 MiB
/// for a Letter page in a 730 px split on a Retina display, and about 28 MiB at
/// a 1200 px pane, because `render_scale` grows with the pane.
fn place_rendered_page(
    rendered_pages: &mut RenderedPages,
    page_index: usize,
    scale: f32,
    image: Arc<RenderImage>,
) -> Option<Arc<RenderImage>> {
    rendered_pages
        .insert(page_index, (scale, image))
        .map(|(_scale, displaced)| displaced)
}

/// Empties the rendered page cache, handing back every image so the caller can
/// free the tiles. Used when the document is replaced on disk and when the tab
/// is released.
fn take_rendered_pages(rendered_pages: &mut RenderedPages) -> Vec<Arc<RenderImage>> {
    rendered_pages
        .drain()
        .map(|(_index, (_scale, image))| image)
        .collect()
}

impl PdfViewer {
    /// Registers the two things every `PdfViewer` needs, wherever it is built.
    ///
    /// `clone_on_split` builds its view from a struct literal rather than going
    /// through `new`, so anything registered only in `new` silently misses the
    /// split: before this was factored out, a split pane never reloaded when the
    /// file changed and never freed a tile when it closed.
    fn register_subscriptions(pdf_item: &Entity<PdfItem>, cx: &mut Context<Self>) {
        // Live-preview loop: the item reloads itself when the file changes
        // on disk (e.g. a Typst/LaTeX recompile); rebuild rendering state
        // but keep zoom and scroll so the document doesn't jump.
        cx.subscribe(
            pdf_item,
            |this, _, event: &pdf_item::PdfItemEvent, cx| match event {
                pdf_item::PdfItemEvent::Reloaded => this.reload_document(cx),
            },
        )
        .detach();
        // Free the sprite-atlas tiles of every page still displayed when the
        // tab closes. Dropping the view frees the CPU bitmaps, but a
        // RenderImage's GPU tile lives until drop_image, so without this every
        // page ever shown leaks for the window's lifetime.
        //
        // `on_release`, not `on_release_in`: release callbacks run from
        // `release_dropped_entities` with an `&mut App` and no window in hand,
        // so the window is still in `App.windows` and `None` reaches it.
        // `on_release_in` would run inside `update_window_id`, which takes the
        // window out of the map, and `None` would then free nothing.
        cx.on_release(|this, cx| {
            // The render thread is detached (its JoinHandle is discarded) and
            // holds the whole document as an Arc<[u8]>, so without this a closed
            // tab leaves it rasterising a page nobody will see. It notices when
            // the token flips or when its send fails, whichever comes first.
            this.cancel_token.store(true, Ordering::Relaxed);
            for image in take_rendered_pages(&mut this.rendered_pages) {
                cx.drop_image(image, None);
            }
        })
        .detach();
    }

    fn new_page_input(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<Editor>, Subscription) {
        let page_input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text_style_refinement(TextStyleRefinement {
                color: Some(cx.theme().colors().text),
                text_align: Some(TextAlign::Center),
                ..Default::default()
            });
            editor
        });
        let subscription = cx.subscribe_in(
            &page_input,
            window,
            |this, _, event: &EditorEvent, _window, cx| {
                if let EditorEvent::Blurred = event {
                    this.shown_page = None;
                    cx.notify();
                }
            },
        );
        (page_input, subscription)
    }

    pub fn new(
        pdf_item: Entity<PdfItem>,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::register_subscriptions(&pdf_item, cx);
        let (page_input, page_input_subscription) = Self::new_page_input(window, cx);
        let mut this = Self {
            pdf_item,
            project,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            metadata: None,
            rendered_pages: HashMap::new(),
            pages_in_flight: HashSet::new(),
            cancel_token: Arc::new(AtomicBool::new(false)),
            metadata_task: Task::ready(()),
            render_task: Task::ready(()),
            render_debounce: Task::ready(()),
            render_error: None,
            zoom_level: 1.0,
            render_scale: SCREEN_SCALE_FACTOR,
            display_scale: SCREEN_SCALE_FACTOR,
            pan_x: px(0.0),
            text_layouts: HashMap::new(),
            text_extraction_task: Task::ready(()),
            selection_start: None,
            selection_end: None,
            is_selecting: false,
            page_input,
            shown_page: None,
            _page_input_subscription: page_input_subscription,
        };
        this.load_metadata(cx);
        this
    }

    fn reload_document(&mut self, cx: &mut Context<Self>) {
        self.cancel_token.store(true, Ordering::Relaxed);
        self.cancel_token = Arc::new(AtomicBool::new(false));
        self.pages_in_flight.clear();
        // Every page is about to be re-rendered into a new RenderImage, so the
        // outgoing ones must give their atlas tiles back. A typeset preview
        // recompiles on each pause in typing and rewrites the same PDF, so this
        // path runs constantly and dropping the images alone would orphan a
        // texture per page per recompile.
        for image in take_rendered_pages(&mut self.rendered_pages) {
            cx.drop_image(image, None);
        }
        // Stopping the extractor and clearing its output have to happen
        // together. `extract_all_text` skips its work when `text_layouts`
        // already holds a layout per page, and the extraction started for the
        // *previous* PDF keeps streaming into the map across the await for the
        // new metadata. Clearing alone lets that producer refill the map with
        // the old document's glyph positions, after which the new extraction is
        // skipped as already done and selection hit-tests against coordinates
        // that no longer match what is on screen. Dropping the task closes the
        // channel, which is also how the extractor thread learns to stop.
        self.text_extraction_task = Task::ready(());
        self.text_layouts.clear();
        self.selection_start = None;
        self.selection_end = None;
        self.load_metadata(cx);
        cx.notify();
    }

    fn load_metadata(&mut self, cx: &mut Context<Self>) {
        let pdf_bytes = self.pdf_item.read(cx).pdf_bytes().clone();

        let background_task =
            cx.background_spawn(async move { pdf_renderer::parse_metadata(&pdf_bytes) });

        self.metadata_task = cx.spawn(async move |this, cx| {
            let result = background_task.await;
            this.update(cx, |this, cx| match result {
                Ok(metadata) => {
                    log::debug!(
                        "pdf_viewer: loaded metadata — {} pages",
                        metadata.page_count
                    );
                    // Read the count before the move: PdfMetadata owns a Vec of
                    // page dimensions, so it is not Copy.
                    let page_count = metadata.page_count;
                    this.metadata = Some(metadata);
                    // A recompile can shorten the document. Pages past the new
                    // end would otherwise sit in the cache forever, holding a
                    // texture each, because nothing re-renders or clears them.
                    let dropped: Vec<_> = this
                        .rendered_pages
                        .keys()
                        .copied()
                        .filter(|index| *index >= page_count)
                        .collect();
                    for index in dropped {
                        if let Some((_scale, image)) = this.rendered_pages.remove(&index) {
                            cx.drop_image(image, None);
                        }
                    }
                    this.render_error = None;
                    log::debug!("pdf_viewer: kicking off text extraction");
                    this.extract_all_text(cx);
                    cx.notify();
                }
                Err(error) => {
                    log::error!("pdf_viewer: failed to load metadata: {error:#}");
                    this.render_error = Some(format!("{error:#}").into());
                }
            })
            .ok();
        });
    }

    fn page_count(&self) -> usize {
        self.metadata
            .as_ref()
            .map_or(0, |metadata| metadata.page_count)
    }

    fn page_dimensions(&self) -> &[PageDimensions] {
        self.metadata
            .as_ref()
            .map_or(&[], |metadata| &metadata.page_dimensions)
    }

    /// The desk the pages sit on: the theme's editor background, shifted a
    /// step darker in light themes and a step lighter in dark ones. Derived
    /// rather than a named token because no theme token means "canvas behind
    /// paper" — the closest, `element_background`, is a button colour that
    /// some themes tint — and deriving keeps the theme's own hue while
    /// guaranteeing contrast against a white page in every theme.
    fn canvas_color(cx: &App) -> gpui::Hsla {
        let mut canvas = cx.theme().colors().editor_background;
        if canvas.l > 0.5 {
            canvas.l = (canvas.l - 0.07).max(0.0);
        } else {
            canvas.l = (canvas.l + 0.05).min(1.0);
        }
        canvas
    }

    /// The width pages are fit to at zoom 1.0: the viewport minus the canvas
    /// gutter on each side. Every consumer of the viewport width for page
    /// geometry must go through this — layout, hit-testing, rasterisation
    /// scale, and pan clamping all assume the same fit width, and one of them
    /// using the raw viewport width would silently offset the others.
    fn page_fit_width(viewport_width: f32) -> f32 {
        (viewport_width - 2.0 * PAGE_CANVAS_MARGIN_PX).max(1.0)
    }

    fn display_height(dim: &PageDimensions, container_width: f32, zoom: f32) -> f32 {
        if dim.width <= 0.0 {
            return dim.height * zoom;
        }
        let fit_scale = container_width / dim.width;
        dim.height * fit_scale * zoom
    }

    fn total_content_height(&self, container_width: f32) -> f32 {
        let dimensions = self.page_dimensions();
        let pages_height: f32 = dimensions
            .iter()
            .map(|dim| Self::display_height(dim, container_width, self.zoom_level))
            .sum();
        let gaps = dimensions.len().saturating_sub(1) as f32 * PAGE_GAP_PX;
        pages_height + gaps
    }

    /// Clamp horizontal pan so content edges never retreat past the viewport
    /// edges, matching macOS Preview behaviour. When content fits within the
    /// viewport pan is forced to zero (centered). When content overflows, pan
    /// is bounded so neither edge can slide past its corresponding viewport
    /// edge.
    fn clamp_pan(&mut self) {
        let viewport_width: f32 = self.scroll_handle.bounds().size.width.into();
        if viewport_width <= 0.0 {
            return;
        }
        // At any zoom, every page has display_width = fit_width * zoom
        // (fit-to-view normalizes all pages to the fit width at zoom 1.0).
        let content_width = Self::page_fit_width(viewport_width) * self.zoom_level;
        let overflow = (content_width - viewport_width).max(0.0);
        let half_overflow = overflow / 2.0;
        self.pan_x = self.pan_x.clamp(px(-half_overflow), px(half_overflow));
    }

    fn visible_page_range(&self, viewport_height: f32, viewport_width: f32) -> Range<usize> {
        let dimensions = self.page_dimensions();
        if dimensions.is_empty() {
            return 0..0;
        }
        let viewport_width = Self::page_fit_width(viewport_width);

        let scroll_y: f32 = self.scroll_handle.offset().y.into();
        // The pages column starts one canvas gutter below the content origin.
        let scroll_offset = (scroll_y.abs() - PAGE_CANVAS_MARGIN_PX).max(0.0);

        let mut y = 0.0_f32;
        let mut first_visible = None;
        let mut last_visible = 0;

        for (index, dim) in dimensions.iter().enumerate() {
            let page_height = Self::display_height(dim, viewport_width, self.zoom_level);
            let page_top = y;
            let page_bottom = y + page_height;

            if page_bottom > scroll_offset && first_visible.is_none() {
                first_visible = Some(index);
            }
            if page_top < scroll_offset + viewport_height {
                last_visible = index;
            }
            if page_top > scroll_offset + viewport_height {
                break;
            }

            y = page_bottom + PAGE_GAP_PX;
        }

        let first = first_visible.unwrap_or(0);
        let last = (last_visible + 1).min(dimensions.len());
        first..last
    }

    /// How far below the top of the pages column page `index` starts.
    fn page_top(&self, index: usize, container_width: f32) -> f32 {
        self.page_dimensions()
            .iter()
            .take(index)
            .map(|dim| Self::display_height(dim, container_width, self.zoom_level) + PAGE_GAP_PX)
            .sum()
    }

    /// The page shown as current: the first one reaching below the canvas
    /// gutter at the top of the viewport. That is the line `scroll_to_page`
    /// puts a page's top on, so jumping to a page always shows its number.
    fn current_page(&self, viewport_width: f32) -> usize {
        let page_count = self.page_count();
        if page_count == 0 {
            return 0;
        }
        let offset = -self.scroll_handle.offset().y;
        let max_offset = self.scroll_handle.max_offset().y;
        // The last pages cannot be scrolled to the top of the viewport, so
        // the end of the document counts as the last page; otherwise jumping
        // to it would show the number of a page above it.
        if max_offset > px(0.0) && offset >= max_offset - px(1.0) {
            return page_count - 1;
        }

        let container_width = Self::page_fit_width(viewport_width);
        let probe: f32 = offset.into();
        let mut page_bottom = 0.0;
        for (index, dim) in self.page_dimensions().iter().enumerate() {
            page_bottom += Self::display_height(dim, container_width, self.zoom_level);
            if page_bottom > probe {
                return index;
            }
            page_bottom += PAGE_GAP_PX;
        }
        page_count - 1
    }

    /// Scrolls so page `index` sits one canvas gutter below the top of the
    /// viewport, as the first page does at the start of the document.
    fn scroll_to_page(&mut self, index: usize, cx: &mut Context<Self>) {
        let viewport_width: f32 = self.scroll_handle.bounds().size.width.into();
        let top = self.page_top(index, Self::page_fit_width(viewport_width));
        let offset = self.scroll_handle.offset();
        self.scroll_handle.set_offset(point(offset.x, px(-top)));
        cx.notify();
    }

    fn go_to_page(&mut self, _: &GoToPage, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_count() == 0 {
            return;
        }
        let focus_handle = self.page_input.focus_handle(cx);
        window.focus(&focus_handle, cx);
        self.page_input.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
    }

    fn confirm_page(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let page_count = self.page_count();
        let typed = self.page_input.read(cx).text(cx);
        if let Ok(number) = typed.trim().parse::<usize>()
            && page_count > 0
        {
            self.scroll_to_page(number.clamp(1, page_count) - 1, cx);
        }
        self.leave_page_input(window, cx);
    }

    fn cancel_page(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_page_input(window, cx);
    }

    fn leave_page_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shown_page = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn buffered_range(&self, visible: Range<usize>) -> Range<usize> {
        let start = visible.start.saturating_sub(RENDER_BUFFER_PAGES);
        let end = (visible.end + RENDER_BUFFER_PAGES).min(self.page_count());
        start..end
    }

    fn schedule_render_after_zoom(&mut self, cx: &mut Context<Self>) {
        self.render_debounce = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RENDER_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                this.update_render_scale_if_needed();
                this.request_visible_pages(cx);
            })
            .ok();
        });
    }

    fn update_render_scale_if_needed(&mut self) {
        // Pages are laid out fit-to-viewport-width at zoom 1.0, so the
        // rasterization scale must include that fit factor and the
        // display's device-pixel ratio — rendering at a fixed multiple of
        // the PDF's point size leaves wide viewports upscaling a too-small
        // bitmap into a blur.
        let viewport_width: f32 = self.scroll_handle.bounds().size.width.into();
        let fit_width = Self::page_fit_width(viewport_width);
        let max_width = self
            .page_dimensions()
            .iter()
            .map(|d| d.width)
            .fold(0.0_f32, f32::max);
        let fit_scale = if viewport_width > 0.0 && max_width > 0.0 {
            fit_width / max_width
        } else {
            1.0
        };
        let ideal_scale = fit_scale * self.zoom_level * self.display_scale;
        // Cap so the largest page dimension never exceeds the Metal atlas limit.
        let max_dim = self
            .page_dimensions()
            .iter()
            .map(|d| d.width.max(d.height))
            .fold(0.0_f32, f32::max);
        let needed_scale = if max_dim > 0.0 {
            ideal_scale.min(MAX_GPU_TILE_PX / max_dim)
        } else {
            ideal_scale
        };
        let scale_too_low = needed_scale > self.render_scale;
        let scale_wasteful = needed_scale < self.render_scale * 0.25;
        if scale_too_low || scale_wasteful {
            log::debug!(
                "pdf_viewer: render scale change {:.1} -> {:.1} (zoom={:.2})",
                self.render_scale,
                needed_scale,
                self.zoom_level
            );
            self.render_scale = needed_scale;
            self.cancel_token.store(true, Ordering::Relaxed);
            self.cancel_token = Arc::new(AtomicBool::new(false));
            self.pages_in_flight.clear();
        }
    }

    fn request_visible_pages(&mut self, cx: &mut Context<Self>) {
        if self.metadata.is_none() {
            return;
        }
        // Covers first layout, window resize, and display changes; the
        // hysteresis inside keeps this cheap when nothing moved.
        self.update_render_scale_if_needed();

        let bounds = self.scroll_handle.bounds();
        let viewport_height = {
            let h: f32 = bounds.size.height.into();
            if h > 0.0 { h } else { 800.0 }
        };
        let viewport_width = {
            let w: f32 = bounds.size.width.into();
            if w > 0.0 { w } else { 600.0 }
        };

        let visible = self.visible_page_range(viewport_height, viewport_width);
        let target_range = self.buffered_range(visible.clone());

        let current_scale = self.render_scale;
        let needs_render = |index: &usize| -> bool {
            !self.pages_in_flight.contains(index)
                && self
                    .rendered_pages
                    .get(index)
                    .map_or(true, |(scale, _)| *scale != current_scale)
        };

        let mut needed: Vec<usize> = visible.clone().filter(&needs_render).collect();
        let buffer_pages: Vec<usize> = target_range
            .filter(|index| !visible.contains(index) && needs_render(index))
            .collect();
        needed.extend(buffer_pages);

        if needed.is_empty() {
            return;
        }

        for &index in &needed {
            self.pages_in_flight.insert(index);
        }

        let needed_display: Vec<usize> = needed.iter().map(|i| i + 1).collect();
        log::debug!(
            "pdf_viewer: rendering pages {:?} at scale {:.1}",
            needed_display,
            self.render_scale
        );

        let pdf_bytes = self.pdf_item.read(cx).pdf_bytes().clone();
        let render_scale = self.render_scale;
        let cancel_token = self.cancel_token.clone();

        let (sender, receiver) = smol::channel::unbounded::<(usize, Arc<RenderImage>)>();

        if let Err(error) = std::thread::Builder::new()
            .name("pdf-renderer".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let pdf = match pdf_renderer::open_pdf(&pdf_bytes) {
                    Ok(pdf) => pdf,
                    Err(error) => {
                        log::error!("pdf_viewer: failed to open PDF for rendering: {error:#}");
                        return;
                    }
                };
                // `needed` was computed from `self.metadata`, but the bytes are
                // read at spawn time. Across a reload the two can disagree for
                // a moment — the request may name pages past the end of the
                // document now on disk. Skip those rather than treating them as
                // failures: the metadata task is already reloading, and its
                // completion re-renders with matching indices.
                let page_count = pdf.pages().len();
                for page_index in needed {
                    if cancel_token.load(Ordering::Relaxed) {
                        log::debug!("pdf_viewer: render cancelled");
                        break;
                    }
                    if page_index >= page_count {
                        log::debug!(
                            "pdf_viewer: skipping page {} — document now has {} pages",
                            page_index + 1,
                            page_count
                        );
                        continue;
                    }
                    log::debug!("pdf_viewer: rendering page {}...", page_index + 1);
                    match pdf_renderer::render_single_page(&pdf, page_index, render_scale, WHITE) {
                        Ok(rendered) => {
                            log::debug!(
                                "pdf_viewer: page {} rendered ({}x{})",
                                page_index + 1,
                                rendered.page_width,
                                rendered.page_height
                            );
                            if sender.send_blocking((page_index, rendered.image)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            log::error!(
                                "pdf_viewer: failed to render page {}: {error:#}",
                                page_index + 1
                            );
                            break;
                        }
                    }
                }
            })
        {
            log::error!("pdf_viewer: failed to spawn render thread: {error:#}");
        }

        self.render_task = cx.spawn(async move |this, cx| {
            while let Ok((page_index, image)) = receiver.recv().await {
                let scale = render_scale;
                this.update(cx, |this, cx| {
                    this.pages_in_flight.remove(&page_index);
                    // Re-rendering a page at a new scale displaces the previous
                    // image, whose atlas tile is only freed explicitly.
                    if let Some(displaced) =
                        place_rendered_page(&mut this.rendered_pages, page_index, scale, image)
                    {
                        cx.drop_image(displaced, None);
                    }
                    cx.notify();
                })
                .ok();
            }
            this.update(cx, |this, _cx| {
                this.pages_in_flight.clear();
            })
            .ok();
        });
    }

    fn render_page_element(
        &self,
        index: usize,
        dimensions: &PageDimensions,
        container_width: f32,
        cx: &App,
    ) -> AnyElement {
        let fit_scale = if dimensions.width > 0.0 {
            container_width / dimensions.width
        } else {
            1.0
        };
        let scale = fit_scale * self.zoom_level;
        let display_width = px(dimensions.width * scale);
        let display_height = px(dimensions.height * scale);

        let page_content = if let Some((_scale, image)) = self.rendered_pages.get(&index) {
            img(image.clone())
                .id(("pdf-page", index))
                .w(display_width)
                .h(display_height)
                .into_any_element()
        } else {
            div()
                .id(("pdf-page-placeholder", index))
                .w(display_width)
                .h(display_height)
                // Rendered pages arrive white (hayro rasterises onto WHITE),
                // so the placeholder is white too, not a theme colour: it is
                // standing in for a sheet of paper, and on the grey canvas it
                // should read as one.
                .bg(gpui::white())
                .flex()
                .justify_center()
                .items_center()
                .child(Label::new(format!("Page {}", index + 1)).color(Color::Muted))
                .into_any_element()
        };

        let highlights = self.selection_highlights_for_page(index, fit_scale);

        div()
            .relative()
            .w(display_width)
            .h(display_height)
            // Word/Preview-style page framing: the scroll container paints a
            // grey canvas, so each white page reads as a sheet of paper and
            // the gaps between pages read as breaks. The shadow lifts the
            // page off the canvas; the hairline crisps its edge.
            .shadow_md()
            .child(page_content)
            .children(highlights)
            // The outline is an overlay rather than a border on this div so
            // the page box keeps the exact size the rendered bitmap and the
            // selection highlights are positioned against.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .border_1()
                    .border_color(cx.theme().colors().border),
            )
            .into_any_element()
    }

    // -- Zoom --

    fn set_zoom(
        &mut self,
        new_zoom: f32,
        zoom_center: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let old_zoom = self.zoom_level;
        self.zoom_level = new_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (self.zoom_level - old_zoom).abs() > f32::EPSILON {
            if old_zoom > 0.0 {
                let ratio = self.zoom_level / old_zoom;
                self.pan_x *= ratio;
            }
            self.clamp_pan();

            if let Some(cursor) = zoom_center {
                let bounds = self.scroll_handle.bounds();
                let offset = self.scroll_handle.offset();

                let cursor_in_container =
                    point(cursor.x - bounds.origin.x, cursor.y - bounds.origin.y);

                let content_x = cursor_in_container.x - offset.x;
                let content_y = cursor_in_container.y - offset.y;

                let zoom_ratio = self.zoom_level / old_zoom;
                let new_content_x = content_x * zoom_ratio;
                let new_content_y = content_y * zoom_ratio;

                let new_offset = point(
                    cursor_in_container.x - new_content_x,
                    cursor_in_container.y - new_content_y,
                );
                self.scroll_handle.set_offset(new_offset);
            }

            cx.notify();
            self.schedule_render_after_zoom(cx);
        }
    }

    fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom_level * ZOOM_STEP, None, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom_level / ZOOM_STEP, None, cx);
    }

    fn reset_zoom(&mut self, _: &ResetZoom, _window: &mut Window, cx: &mut Context<Self>) {
        self.pan_x = px(0.0);
        self.set_zoom(1.0, None, cx);
    }

    fn fit_to_view(&mut self, _: &FitToView, _window: &mut Window, cx: &mut Context<Self>) {
        self.pan_x = px(0.0);
        self.set_zoom(1.0, None, cx);
    }

    fn zoom_to_actual_size(
        &mut self,
        _: &ZoomToActualSize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_zoom(1.0, None, cx);
    }

    // Handle horizontal scroll manually here because GPUI's ScrollHandler
    // only deals with vertical scrolling — horizontal delta from the
    // trackpad / scroll wheel is not forwarded through the scroll container.
    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.control || event.modifiers.platform {
            let delta: f32 = match event.delta {
                ScrollDelta::Pixels(pixels) => pixels.y.into(),
                ScrollDelta::Lines(lines) => lines.y * SCROLL_LINE_MULTIPLIER,
            };
            let zoom_factor = if delta > 0.0 {
                1.0 + delta.abs() * SCROLL_ZOOM_SENSITIVITY
            } else {
                1.0 / (1.0 + delta.abs() * SCROLL_ZOOM_SENSITIVITY)
            };
            self.set_zoom(self.zoom_level * zoom_factor, Some(event.position), cx);
        } else {
            let delta_x = match event.delta {
                ScrollDelta::Pixels(pixels) => pixels.x,
                ScrollDelta::Lines(lines) => px(lines.x * SCROLL_LINE_MULTIPLIER),
            };
            if delta_x != px(0.0) {
                self.pan_x += delta_x;
                self.clamp_pan();
            }
            self.request_visible_pages(cx);
            cx.notify();
        }
    }
}

impl EventEmitter<PdfViewEvent> for PdfViewer {}
impl EventEmitter<()> for PdfViewer {}

impl Focusable for PdfViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PdfViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.display_scale = window.scale_factor();
        self.request_visible_pages(cx);

        let content = if let Some(error) = &self.render_error {
            v_flex()
                .p_4()
                .gap_2()
                .child(Label::new("Failed to render PDF").color(Color::Error))
                .child(Label::new(error.clone()).size(LabelSize::Small))
                .into_any_element()
        } else if self.metadata.is_none() {
            v_flex()
                .p_4()
                .child(Label::new("Loading..."))
                .into_any_element()
        } else {
            let dimensions = self.page_dimensions().to_vec();
            let bounds = self.scroll_handle.bounds();
            let container_width: f32 = bounds.size.width.into();
            let container_width = if container_width > 0.0 {
                Self::page_fit_width(container_width)
            } else {
                dimensions.iter().map(|d| d.width).fold(0.0_f32, f32::max)
            };

            // When all pages fit inside the viewport, centre them vertically
            // with equal padding so they float in the middle. When content
            // exceeds the viewport, no padding is added and GPUI's scroll
            // container naturally bounds scrolling to the content height —
            // matching macOS Preview's behaviour.
            let total_content_h = self.total_content_height(container_width);
            let viewport_h: f32 = {
                let h: f32 = self.scroll_handle.bounds().size.height.into();
                if h > 0.0 { h } else { 800.0 }
            };

            // Centering happens inside the space left after the canvas
            // gutters; the hit-test in selection.rs mirrors this exact
            // computation, so the two must change together.
            let viewport_avail = viewport_h - 2.0 * PAGE_CANVAS_MARGIN_PX;
            let centering_pad = if total_content_h < viewport_avail {
                (viewport_avail - total_content_h) / 2.0
            } else {
                0.0
            };
            let vertical_gutter = PAGE_CANVAS_MARGIN_PX + centering_pad;

            let mut pages_column = v_flex().gap(px(PAGE_GAP_PX)).items_center();
            pages_column = pages_column.child(div().h(px(vertical_gutter)));
            for (index, dim) in dimensions.iter().enumerate() {
                pages_column =
                    pages_column.child(self.render_page_element(index, dim, container_width, cx));
            }
            pages_column = pages_column.child(div().h(px(vertical_gutter)));

            if self.pan_x != px(0.0) {
                div()
                    .relative()
                    .left(self.pan_x)
                    .child(pages_column)
                    .into_any_element()
            } else {
                pages_column.into_any_element()
            }
        };

        let vp_w: f32 = self.scroll_handle.bounds().size.width.into();
        let vp_w = if vp_w > 0.0 { vp_w } else { 600.0 };

        let zoom_percent = (self.zoom_level * 100.0).round() as u32;
        let page_count = self.page_count();
        // The box follows scrolling only while nobody is typing in it.
        if page_count > 0 && !self.page_input.focus_handle(cx).is_focused(window) {
            let current_page = self.current_page(vp_w);
            if self.shown_page != Some(current_page) {
                self.shown_page = Some(current_page);
                // Deferred past this frame: an editor whose text is replaced
                // while the window is drawing laid out an empty line, so after
                // the first page every number the box was given came out blank.
                cx.defer_in(window, move |this, window, cx| {
                    this.page_input.update(cx, |editor, cx| {
                        editor.set_text((current_page + 1).to_string(), window, cx);
                    });
                });
            }
        }
        let page_digits = page_count.max(1).to_string().len() as f32;
        let page_box_width = px(page_digits * 9.0 + 30.0);

        v_flex()
            .id("PdfViewer")
            .key_context("PdfViewer")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::reset_zoom))
            .on_action(cx.listener(Self::fit_to_view))
            .on_action(cx.listener(Self::zoom_to_actual_size))
            .on_action(cx.listener(Self::copy_document_text))
            .on_action(cx.listener(Self::go_to_page))
            // Take the space the pane leaves rather than asking for the
            // parent's full height: measured in a running window, a
            // `size_full` root put this view's bounds 45px below the pane's
            // content mask — exactly the height of the toolbar rendered above
            // it. That strip is clipped, and because it sits at the end of the
            // scroll range, the canvas margin below the last page could never
            // be scrolled into view: the last page ran off the bottom of the
            // pane while every other page break showed its gap.
            .w_full()
            .flex_1()
            .min_h_0()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .gap_1()
                    .when(page_count == 0, |row| {
                        row.child(Label::new("Loading...").size(LabelSize::Small))
                    })
                    .when(page_count > 0, |row| {
                        row.child(Label::new("Page").size(LabelSize::Small))
                            .child(
                                div()
                                    .id("pdf-page-input")
                                    // Enter and Escape reach the box as menu
                                    // actions; listening here rather than on
                                    // the root keeps them from firing while the
                                    // pages have focus.
                                    .on_action(cx.listener(Self::confirm_page))
                                    .on_action(cx.listener(Self::cancel_page))
                                    .w(page_box_width)
                                    .px_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .bg(cx.theme().colors().editor_background)
                                    .text_ui_sm(cx)
                                    .child(self.page_input.clone()),
                            )
                            .child(
                                Label::new(format!("of {page_count}  ·  {zoom_percent}%"))
                                    .size(LabelSize::Small),
                            )
                    }),
            )
            // The scrollbar hangs off a wrapper, not the scroll container: a
            // child of the container is painted at the scroll offset, so the
            // thumb scrolled away with the first page and its track's hit area
            // drifted, which made any drag leap to the last page.
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("pdf-scroll-container")
                            .flex_1()
                            // The canvas behind the pages. Deliberately darker
                            // than `editor_background`: white pages on a white
                            // canvas have no visible edges, so breaks and
                            // margins disappear — Word and Preview grey the
                            // desk for the same reason.
                            .bg(Self::canvas_color(cx))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .on_scroll_wheel(cx.listener(Self::handle_scroll_wheel))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
                            .on_mouse_move(cx.listener(Self::handle_mouse_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
                            .cursor(CursorStyle::IBeam)
                            .child(content),
                    )
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
    }
}

impl Item for PdfViewer {
    type Event = PdfViewEvent;

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        match event {
            PdfViewEvent::TitleChanged => {
                f(workspace::item::ItemEvent::UpdateTab);
                f(workspace::item::ItemEvent::UpdateBreadcrumbs);
            }
        }
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.pdf_item.read(cx).file_name().to_string().into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, cx: &App) -> Option<Icon> {
        let path = self.pdf_item.read(cx).abs_path();
        FileIcons::get_icon(path, cx).map(Icon::from_path)
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(
            self.pdf_item
                .read(cx)
                .abs_path()
                .display()
                .to_string()
                .into(),
        )
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        f(self.pdf_item.entity_id(), self.pdf_item.read(cx))
    }

    fn breadcrumb_location(&self, _cx: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        let text = self.pdf_item.read(cx).file_name().to_string();
        Some((
            vec![HighlightedText {
                text: text.into(),
                highlights: Vec::new(),
            }],
            None,
        ))
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        Task::ready(Some(cx.new(|cx| {
            let pdf_item = self.pdf_item.clone();
            Self::register_subscriptions(&pdf_item, cx);
            let (page_input, page_input_subscription) = Self::new_page_input(window, cx);
            Self {
                pdf_item,
                project: self.project.clone(),
                focus_handle: cx.focus_handle(),
                scroll_handle: ScrollHandle::new(),
                metadata: self.metadata.clone(),
                // A page's image is owned by exactly one view, so the split
                // starts empty and renders its own visible pages.
                //
                // Sharing the `Arc`s would be unsafe, not merely wasteful. A
                // pane renders as a cached view, and a cached view that is not
                // dirty replays its scene verbatim, tile ids included, without
                // calling `paint_image`. `cx.notify()` marks only a view and its
                // ancestors dirty, and a split is a sibling, so it never
                // repaints. Meanwhile a two-way split only halves `fit_scale`,
                // which the re-render hysteresis ignores, so the split would sit
                // on shared images indefinitely. The primary's free empties the
                // texture slot, and the split's next replay reads that slot back
                // as `None`.
                rendered_pages: RenderedPages::new(),
                pages_in_flight: HashSet::new(),
                cancel_token: Arc::new(AtomicBool::new(false)),
                metadata_task: Task::ready(()),
                render_task: Task::ready(()),
                render_debounce: Task::ready(()),
                render_error: self.render_error.clone(),
                zoom_level: self.zoom_level,
                render_scale: self.render_scale,
                display_scale: self.display_scale,
                pan_x: self.pan_x,
                text_layouts: self.text_layouts.clone(),
                text_extraction_task: Task::ready(()),
                selection_start: None,
                selection_end: None,
                is_selecting: false,
                page_input,
                shown_page: None,
                _page_input_subscription: page_input_subscription,
            }
        })))
    }

    fn buffer_kind(&self, _cx: &App) -> workspace::item::ItemBufferKind {
        workspace::item::ItemBufferKind::Singleton
    }
}

impl ProjectItem for PdfViewer {
    type Item = PdfItem;

    fn for_project_item(
        project: Entity<Project>,
        _pane: Option<&Pane>,
        item: Entity<Self::Item>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self
    where
        Self: Sized,
    {
        Self::new(item, project, window, cx)
    }

    fn for_broken_project_item(
        abs_path: &Path,
        is_local: bool,
        error: &anyhow::Error,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InvalidItemView>
    where
        Self: Sized,
    {
        Some(InvalidItemView::new(abs_path, is_local, error, window, cx))
    }
}

pub fn init(cx: &mut App) {
    workspace::register_project_item::<PdfViewer>(cx);
}

#[cfg(test)]
mod rendered_page_tests {
    //! Pins the sprite-atlas leak fix. Every image these helpers displace must
    //! come back to the caller, because a `RenderImage`'s GPU tile outlives its
    //! `Arc` and the Metal atlas never evicts. An image dropped without being
    //! returned here holds a texture for the window's lifetime.
    use super::*;
    use image::Frame;
    use smallvec::SmallVec;

    fn test_image() -> Arc<RenderImage> {
        // A 1x1 BGRA pixel. Only the process-unique id matters here.
        let buffer = image::ImageBuffer::from_raw(1, 1, vec![0u8; 4]).expect("buffer");
        Arc::new(RenderImage::new(SmallVec::from_elem(Frame::new(buffer), 1)))
    }

    #[test]
    fn first_render_of_a_page_displaces_nothing() {
        let mut pages = RenderedPages::default();
        let displaced = place_rendered_page(&mut pages, 0, 2.0, test_image());
        assert!(displaced.is_none(), "a fresh page slot displaces nothing");
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn re_rendering_a_page_returns_its_previous_image() {
        let mut pages = RenderedPages::default();
        let old = test_image();
        let old_id = old.id;
        place_rendered_page(&mut pages, 3, 2.0, old);

        let new = test_image();
        let new_id = new.id;
        let displaced = place_rendered_page(&mut pages, 3, 4.0, new)
            .expect("re-rendering a page must return the image it replaced");

        assert_eq!(
            displaced.id, old_id,
            "the previous image must come back so its tile can be freed"
        );
        assert_eq!(pages[&3].1.id, new_id);
        assert_eq!(pages[&3].0, 4.0, "the new scale must be recorded");
    }

    #[test]
    fn taking_the_cache_returns_every_page_and_empties_it() {
        let mut pages = RenderedPages::default();
        let mut expected: Vec<_> = (0..5)
            .map(|index| {
                let image = test_image();
                let id = image.id;
                place_rendered_page(&mut pages, index, 2.0, image);
                id
            })
            .collect();

        let mut taken: Vec<_> = take_rendered_pages(&mut pages)
            .iter()
            .map(|image| image.id)
            .collect();

        taken.sort();
        expected.sort();
        assert_eq!(taken, expected, "every cached page must be handed back");
        assert!(
            pages.is_empty(),
            "the cache must be empty so no page is freed twice"
        );
    }

    #[test]
    fn taking_an_empty_cache_returns_nothing() {
        let mut pages = RenderedPages::default();
        assert!(take_rendered_pages(&mut pages).is_empty());
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use project::{ProjectItem as _, ProjectPath};
    use serde_json::json;

    /// A document of `page_count` blank Letter pages, with a cross-reference
    /// table whose offsets are real.
    fn blank_pdf(page_count: usize) -> String {
        let kids = (0..page_count)
            .map(|index| format!("{} 0 R", index + 3))
            .collect::<Vec<_>>()
            .join(" ");
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"),
        ];
        objects.extend(
            (0..page_count)
                .map(|_| "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string()),
        );

        let mut pdf = "%PDF-1.7\n".to_string();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref_offset = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf
    }

    async fn open_viewer(
        cx: &mut TestAppContext,
    ) -> (Entity<PdfViewer>, &mut VisualTestContext, tempfile::TempDir) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            let client = client::Client::new(
                Arc::new(clock::FakeSystemClock::new()),
                http_client::FakeHttpClient::with_404_response(),
                cx,
            );
            Project::init(&client, cx);
        });

        // The item reads the bytes with `std::fs`, not the project's `Fs`, so
        // the document has to exist on disk at the path the worktree reports.
        let dir = tempfile::tempdir().expect("temp dir");
        let pdf = blank_pdf(10);
        std::fs::write(dir.path().join("doc.pdf"), &pdf).expect("write the PDF");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(dir.path(), json!({ "doc.pdf": pdf })).await;
        let project = Project::test(fs, [dir.path()], cx).await;

        let project_path = project.read_with(cx, |project, cx| {
            let worktree = project.worktrees(cx).next().expect("a worktree");
            ProjectPath {
                worktree_id: worktree.read(cx).id(),
                path: util::rel_path::rel_path("doc.pdf").into(),
            }
        });
        let item = cx
            .update(|cx| PdfItem::try_open(&project, &project_path, cx))
            .expect("the viewer claims a .pdf")
            .await
            .expect("the PDF loads");

        let (viewer, cx) =
            cx.add_window_view(|window, cx| PdfViewer::new(item, project, window, cx));
        cx.run_until_parked();
        viewer.update_in(cx, |viewer, window, cx| {
            window.focus(&viewer.focus_handle, cx);
            window.refresh();
        });
        cx.run_until_parked();
        (viewer, cx, dir)
    }

    /// The scrollbar used to be a child of the scroll container, so it was
    /// painted at the scroll offset: away from the first page its thumb was
    /// off screen, and a press where the thumb should be landed on a track
    /// that had drifted with the content, sending the view to the last page.
    #[gpui::test]
    async fn dragging_the_thumb_mid_document_scrolls_by_the_drag(cx: &mut TestAppContext) {
        let (viewer, cx, _dir) = open_viewer(cx).await;

        let (bounds, max_offset) = viewer.read_with(cx, |viewer, _| {
            (
                viewer.scroll_handle.bounds(),
                viewer.scroll_handle.max_offset().y,
            )
        });
        assert!(
            max_offset > px(0.0),
            "ten pages must overflow the window, max offset was {max_offset:?}"
        );

        let start_offset = -max_offset / 2.0;
        viewer.update_in(cx, |viewer, window, _| {
            viewer
                .scroll_handle
                .set_offset(point(px(0.0), start_offset));
            window.refresh();
        });
        cx.run_until_parked();

        // Mirrors `ScrollbarState::thumb_ranges` for a regular-style vertical
        // track: 4px of padding around a 6px thumb on the right edge.
        let viewport = bounds.size.height;
        let track_top = bounds.top() + px(4.0);
        let track_length = viewport - px(8.0);
        let thumb_fraction =
            (viewport * (viewport / (viewport + max_offset))).max(px(25.0)) / viewport;
        let thumb_length = track_length * thumb_fraction;
        let thumb_center = point(
            bounds.right() - px(7.0),
            track_top + (track_length - thumb_length) * 0.5 + thumb_length / 2.0,
        );

        let drag = px(20.0);
        let dragged_to = point(thumb_center.x, thumb_center.y + drag);
        cx.simulate_mouse_move(thumb_center, None, Modifiers::none());
        cx.simulate_mouse_down(thumb_center, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(dragged_to, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(dragged_to, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let end_offset = viewer.read_with(cx, |viewer, _| viewer.scroll_handle.offset().y);
        let moved = start_offset - end_offset;
        let expected = drag * (max_offset / (track_length - thumb_length));
        assert!(
            moved > expected * 0.5 && moved < expected * 1.5,
            "a {drag:?} drag should scroll about {expected:?}, but scrolled {moved:?} \
             (from {start_offset:?} to {end_offset:?}, max {max_offset:?})"
        );
    }

    /// What the box displays, which is what the reader sees. The buffer can
    /// hold the right number while the display shows something else.
    fn shown_page_text(viewer: &Entity<PdfViewer>, cx: &mut VisualTestContext) -> String {
        let (buffer, display) = viewer.update(cx, |viewer, cx| {
            viewer
                .page_input
                .update(cx, |editor, cx| (editor.text(cx), editor.display_text(cx)))
        });
        assert_eq!(display, buffer, "the box must display the number it holds");
        display
    }

    fn scroll_offset(viewer: &Entity<PdfViewer>, cx: &mut VisualTestContext) -> Pixels {
        viewer.read_with(cx, |viewer, _| viewer.scroll_handle.offset().y)
    }

    fn page_top_offset(
        viewer: &Entity<PdfViewer>,
        index: usize,
        cx: &mut VisualTestContext,
    ) -> Pixels {
        viewer.read_with(cx, |viewer, _| {
            let width: f32 = viewer.scroll_handle.bounds().size.width.into();
            px(-viewer.page_top(index, PdfViewer::page_fit_width(width)))
        })
    }

    fn type_page(text: &str, cx: &mut VisualTestContext) {
        cx.dispatch_action(GoToPage);
        cx.simulate_input(text);
    }

    fn redraw(viewer: &Entity<PdfViewer>, cx: &mut VisualTestContext) {
        viewer.update_in(cx, |_, window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn typing_a_page_number_jumps_to_that_page(cx: &mut TestAppContext) {
        let (viewer, cx, _dir) = open_viewer(cx).await;
        assert_eq!(shown_page_text(&viewer, cx), "1");

        type_page("4", cx);
        cx.dispatch_action(menu::Confirm);
        redraw(&viewer, cx);

        assert_eq!(
            scroll_offset(&viewer, cx),
            page_top_offset(&viewer, 3, cx),
            "page 4's top should sit one canvas gutter below the top of the view"
        );
        assert_eq!(shown_page_text(&viewer, cx), "4");
        viewer.update_in(cx, |viewer, window, cx| {
            assert!(
                viewer.focus_handle(cx).is_focused(window),
                "confirming hands focus back to the pages"
            );
        });
    }

    #[gpui::test]
    async fn page_numbers_out_of_range_go_to_the_nearest_page(cx: &mut TestAppContext) {
        let (viewer, cx, _dir) = open_viewer(cx).await;

        type_page("99", cx);
        cx.dispatch_action(menu::Confirm);
        redraw(&viewer, cx);
        assert_eq!(scroll_offset(&viewer, cx), page_top_offset(&viewer, 9, cx));
        assert_eq!(shown_page_text(&viewer, cx), "10");

        type_page("0", cx);
        cx.dispatch_action(menu::Confirm);
        redraw(&viewer, cx);
        assert_eq!(scroll_offset(&viewer, cx), px(0.0));
        assert_eq!(shown_page_text(&viewer, cx), "1");
    }

    #[gpui::test]
    async fn jumping_to_a_last_page_that_cannot_reach_the_top_still_shows_it(
        cx: &mut TestAppContext,
    ) {
        let (viewer, cx, _dir) = open_viewer(cx).await;
        viewer.update(cx, |viewer, cx| {
            viewer.zoom_level = 0.2;
            cx.notify();
        });
        redraw(&viewer, cx);
        let max_offset = viewer.read_with(cx, |viewer, _| viewer.scroll_handle.max_offset().y);
        assert!(
            page_top_offset(&viewer, 9, cx) < -max_offset,
            "zoomed out, the last page's top must lie beyond the end of the scroll range"
        );

        type_page("10", cx);
        cx.dispatch_action(menu::Confirm);
        redraw(&viewer, cx);
        assert_eq!(scroll_offset(&viewer, cx), -max_offset);
        assert_eq!(
            shown_page_text(&viewer, cx),
            "10",
            "the end of the document counts as the last page, or the box would \
             answer a jump to page 10 with the number of a page above it"
        );
    }

    #[gpui::test]
    async fn escape_and_non_numbers_leave_the_view_where_it_was(cx: &mut TestAppContext) {
        let (viewer, cx, _dir) = open_viewer(cx).await;

        type_page("7", cx);
        cx.dispatch_action(menu::Cancel);
        redraw(&viewer, cx);
        assert_eq!(scroll_offset(&viewer, cx), px(0.0));
        assert_eq!(
            shown_page_text(&viewer, cx),
            "1",
            "an abandoned number is replaced by the current page"
        );

        type_page("seven", cx);
        cx.dispatch_action(menu::Confirm);
        redraw(&viewer, cx);
        assert_eq!(scroll_offset(&viewer, cx), px(0.0));
        assert_eq!(shown_page_text(&viewer, cx), "1");
    }

    #[gpui::test]
    async fn the_page_box_follows_scrolling(cx: &mut TestAppContext) {
        let (viewer, cx, _dir) = open_viewer(cx).await;

        let sixth_page = page_top_offset(&viewer, 5, cx);
        viewer.update(cx, |viewer, _| {
            viewer.scroll_handle.set_offset(point(px(0.0), sixth_page));
        });
        redraw(&viewer, cx);
        assert_eq!(shown_page_text(&viewer, cx), "6");

        // Page six stays current until its bottom edge passes the line.
        let seventh_page = page_top_offset(&viewer, 6, cx);
        viewer.update(cx, |viewer, _| {
            viewer
                .scroll_handle
                .set_offset(point(px(0.0), seventh_page + px(PAGE_GAP_PX + 1.0)));
        });
        redraw(&viewer, cx);
        assert_eq!(shown_page_text(&viewer, cx), "6");
    }
}
