//! The Typst live preview pane.
//!
//! The pane never touches Vello or the vector IR: [`crate::session`] runs the
//! connection and the GPU on its own thread and sends page metadata plus
//! finished RGBA images over a channel. This module lays those images out as a
//! vertical stack of pages and asks for the pages the viewport shows.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use editor::{Editor, EditorEvent, actions::SelectAll};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, PinchEvent, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollEvent,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, TextAlign,
    TextStyleRefinement, WeakEntity, Window, actions, canvas, div, img, point, px, size,
};
use language::{Buffer, BufferEvent};
use lsp::LanguageServerId;
use menu::{Cancel, Confirm};
use project::{LspStore, LspStoreEvent, Project};
use reflexo::hash::Fingerprint;
use serde_json::json;
use ui::prelude::*;
use ui::{ScrollAxes, Scrollbars, Tooltip, WithScrollbar};
use vello::kurbo;
use workspace::{
    OpenOptions, SaveIntent, SplitDirection, Toast, Workspace,
    item::{Item, ItemEvent},
    notifications::NotificationId,
};

use crate::collab::{RelayClient, RelayItem};
use crate::connection::PreviewConnectionConfig;
use crate::connection::PreviewEvent;
use crate::frame_timing::{FrameSpans, FrameTiming};
use crate::links::{LinkTarget, classify_link};
use crate::session::{PageMeta, SessionEvent, SessionHandle, SessionInput};
use crate::tunnel::PortForward;
use crate::word_count::{WordsCount, count_words};
use crate::{PageAccessibility, PageTextPosition, PageTextSelection};

const PAGE_GAP: f32 = 16.0;
/// Tinymist pushes the document outline as a server-specific notification.
pub(crate) const OUTLINE_METHOD: &str = "tinymist/documentOutline";
const CANVAS_PADDING: f32 = 24.0;
/// Keep a revealed link target at least this far inside the viewport.
const REVEAL_MARGIN: f32 = 24.0;
/// Pages to keep rendered on either side of the viewport.
const RENDER_BUFFER_PAGES: usize = 1;
/// Vello's coarse rasterizer supports at most 256 bins of 256x256 pixels.
///
/// Larger targets make its binning shaders write out of bounds. On some
/// drivers that hangs the GPU and takes the whole desktop with it, so page
/// rasters stay well inside this limit (linebender/vello#680).
const VELLO_BIN_SIZE: f64 = 256.0;
const VELLO_MAX_BINS: f64 = 256.0;
/// Largest page texture dimension. This also keeps the GPUI sprite atlas
/// happy, and a page that big would not be visible anyway.
const MAX_TILE_DIM: f64 = 4096.0;
/// Pixel budget for one page texture.
///
/// 8 MP is about 176 bins even for a square page, comfortably below Vello's
/// 256-bin limit. It bounds the render target, readback buffer, and image
/// copies to roughly 32 MB each by the time a page is on screen.
const MAX_TILE_PIXELS: f64 = 8_000_000.0;
/// Quiet period after the last document update before pages are re-rendered at
/// full resolution.
///
/// Updates are shown immediately, at [`DRAFT_SCALE_FACTOR`] of the display
/// resolution, so the preview tracks typing instead of waiting for a pause.
/// The sharp pass runs once typing stops.
const SETTLE_DELAY: Duration = Duration::from_millis(120);
/// Quiet period after the last pinch or wheel-zoom event before new pixels are
/// requested.
///
/// A gesture produces a zoom event every few milliseconds. Requesting a render
/// for each one queues a full set of page rasters behind each other, and the
/// final scale only starts after every intermediate scale has finished. The
/// old bitmap is scaled on screen while the gesture runs, so waiting for it to
/// pause is both faster and cheaper.
const ZOOM_RENDER_DELAY: Duration = Duration::from_millis(80);
/// Quiet period after cursor movement before the preview follows it.
const SYNC_DEBOUNCE: Duration = Duration::from_millis(200);
/// Display-scale factor for the renders shown while the document is changing.
///
/// Rasterization cost grows with the square of the scale, so this is a 4x
/// reduction in pixels at the cost of softness until the document settles.
const DRAFT_SCALE_FACTOR: f64 = 0.5;
/// A request older than this is assumed lost (for example because the session
/// skipped a stale page) so the pane cannot wedge waiting for it forever.
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 1.2;
const SCROLL_LINE_PX: f32 = 64.0;
/// Pixels per wheel "line" for horizontal panning.
const SCROLL_LINE_MULTIPLIER: f32 = 20.0;
const SCROLL_PAGE_MARGIN_PX: f32 = 48.0;
const SCROLL_ZOOM_SENSITIVITY: f32 = 0.01;

/// A text position in the document: page, text run, character.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TextPosition {
    page: usize,
    run: usize,
    character: usize,
}

#[derive(Clone, Copy)]
struct Selection {
    anchor: TextPosition,
    focus: TextPosition,
    /// True while the left button is held and the pointer is extending the selection.
    dragging: bool,
}

actions!(
    typst_live_preview,
    [
        /// Opens a live Typst preview, rendered from Tinymist's preview stream.
        OpenPreview,
        /// Copies the selected text to the clipboard.
        Copy,
        /// Zooms in.
        ZoomIn,
        /// Zooms out.
        ZoomOut,
        /// Returns to fit-to-width.
        ResetZoom,
        /// Fits the page width to the pane.
        FitWidth,
        /// Fits a whole page in the pane.
        FitPage,
        /// Renders pages at 100%.
        ActualSize,
        /// Jumps to a page number.
        GoToPage,
        /// Scrolls to the previous page.
        PreviousPage,
        /// Scrolls to the next page.
        NextPage,
        /// Scrolls the preview to the active editor's cursor.
        ScrollPreviewToCursor,
        /// Exports the previewed document as a PDF.
        ExportPdf,
        /// Toggles the document outline.
        ToggleOutline,
        /// Opens the find-in-preview bar.
        FindInPreview,
        /// Selects the next find result.
        FindNext,
        /// Selects the previous find result.
        FindPrevious,
        /// Closes the find-in-preview bar.
        CloseFind,
        /// Scrolls down by one line.
        ScrollLineDown,
        /// Scrolls up by one line.
        ScrollLineUp,
        /// Scrolls down by one viewport.
        ScrollPageDown,
        /// Scrolls up by one viewport.
        ScrollPageUp,
        /// Scrolls to the top of the document.
        ScrollToTop,
        /// Scrolls to the end of the document.
        ScrollToBottom,
        /// Scrolls left.
        ScrollLeft,
        /// Scrolls right.
        ScrollRight
    ]
);

/// How the page scale is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZoomMode {
    FitWidth,
    FitPage,
    ActualSize,
    Custom,
}

/// How a zoom change asks for new pixels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ZoomRender {
    /// Request a sharp render immediately (keyboard, toolbar, fit modes).
    Now,
    /// Coalesce the events of a continuous gesture into one render once it
    /// pauses (pinch and ctrl+wheel).
    OnSettle,
}

/// The zoom state restored for a file when its preview is reopened.
#[derive(Clone, Copy)]
struct SavedZoom {
    mode: ZoomMode,
    zoom: f32,
}

/// The preview whose document Tinymist currently treats as its main document.
///
/// Outline notifications carry no task or file id, so when several previews are
/// open only this one should apply them.
static PRIMARY_PREVIEW: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

pub(crate) fn set_primary_preview(path: &Path) {
    *PRIMARY_PREVIEW
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(path.to_path_buf());
}

fn is_primary_preview(path: &Path) -> bool {
    PRIMARY_PREVIEW
        .get()
        .and_then(|primary| primary.lock().ok().and_then(|primary| primary.clone()))
        .is_some_and(|primary| primary == path)
}

fn saved_zooms() -> &'static Mutex<HashMap<PathBuf, SavedZoom>> {
    static SAVED: OnceLock<Mutex<HashMap<PathBuf, SavedZoom>>> = OnceLock::new();
    SAVED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn zoom_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Emitted when the preview's tab title needs to be refreshed.
#[derive(Clone, Copy, Debug)]
pub enum TypstPreviewEvent {
    TitleChanged,
}

/// One heading in Tinymist's preview outline.
#[derive(Clone, Debug)]
struct OutlineEntry {
    title: String,
    depth: usize,
    /// Zero-based page, when Tinymist resolved the heading to a position.
    page: Option<usize>,
    x: f32,
    y: f32,
}

/// One text match for the find bar.
#[derive(Clone, Debug)]
struct FindMatch {
    page: usize,
    selection: PageTextSelection,
    rects: Vec<kurbo::Rect>,
}

fn parse_outline(value: &serde_json::Value) -> Vec<OutlineEntry> {
    fn walk(items: Option<&serde_json::Value>, depth: usize, entries: &mut Vec<OutlineEntry>) {
        let Some(items) = items.and_then(|items| items.as_array()) else {
            return;
        };
        for item in items {
            let title = item
                .get("title")
                .and_then(|title| title.as_str())
                .unwrap_or_default();
            let position = item.get("position");
            let page = position
                .and_then(|position| position.get("page_no"))
                .and_then(|page| page.as_u64())
                .and_then(|page| usize::try_from(page).ok())
                .map(|page| page.saturating_sub(1));
            let x = position
                .and_then(|position| position.get("x"))
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0) as f32;
            let y = position
                .and_then(|position| position.get("y"))
                .and_then(|y| y.as_f64())
                .unwrap_or(0.0) as f32;
            entries.push(OutlineEntry {
                title: title.to_owned(),
                depth,
                page,
                x,
                y,
            });
            walk(item.get("children"), depth + 1, entries);
        }
    }

    let mut entries = Vec::new();
    walk(value.get("items"), 0, &mut entries);
    entries
}

/// The LSP resources a preview needs in order to clean up after itself.
#[derive(Clone)]
pub struct PreviewOwner {
    lsp_store: WeakEntity<LspStore>,
    server_id: Option<LanguageServerId>,
    task_id: String,
    /// The SSH tunnel carrying the data plane, for previews on a remote host.
    forward: Option<Arc<PortForward>>,
    /// Set when Tinymist runs on a collaborative project's host.
    collab: Option<CollabPreviewOwner>,
}

/// The host-side stream this pane is subscribed to.
#[derive(Clone)]
pub(crate) struct CollabPreviewOwner {
    stream_id: String,
}

impl PreviewOwner {
    /// Creates an owner handle for a local preview task.
    pub fn new(
        lsp_store: WeakEntity<LspStore>,
        server_id: LanguageServerId,
        task_id: String,
    ) -> Self {
        Self {
            lsp_store,
            server_id: Some(server_id),
            task_id,
            forward: None,
            collab: None,
        }
    }

    /// Creates an owner handle for a preview relayed by the collab host.
    pub(crate) fn new_collab(
        lsp_store: WeakEntity<LspStore>,
        stream_id: String,
        server_id: LanguageServerId,
    ) -> Self {
        Self {
            lsp_store,
            server_id: Some(server_id),
            task_id: stream_id.clone(),
            forward: None,
            collab: Some(CollabPreviewOwner { stream_id }),
        }
    }

    /// Attaches the tunnel that carries a remote preview. The tunnel is stopped
    /// together with the preview task.
    pub(crate) fn with_forward(mut self, forward: Option<Arc<PortForward>>) -> Self {
        self.forward = forward;
        self
    }

    fn collab(&self) -> Option<&CollabPreviewOwner> {
        self.collab.as_ref()
    }

    /// Asks Tinymist to stop this preview task.
    pub(crate) fn kill(&self, cx: &mut App) {
        if let Some(forward) = &self.forward {
            forward.stop();
        }
        if let Some(collab) = &self.collab {
            let (Some(lsp_store), Some(server_id)) = (self.lsp_store.upgrade(), self.server_id)
            else {
                return;
            };
            let stream_id = collab.stream_id.clone();
            lsp_store
                .update(cx, |store, cx| {
                    store.execute_lsp_command(
                        server_id,
                        crate::collab::COMMAND_STOP.to_owned(),
                        vec![json!({ "stream_id": stream_id })],
                        cx,
                    )
                })
                .detach();
            return;
        }
        let Some(lsp_store) = self.lsp_store.upgrade() else {
            return;
        };
        let Some(server_id) = self.server_id else {
            return;
        };
        let task_id = self.task_id.clone();
        lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(
                    server_id,
                    "tinymist.doKillPreview".to_owned(),
                    vec![json!(task_id)],
                    cx,
                )
            })
            .detach();
    }
}

/// Where a preview pane's frames come from.
pub(crate) enum PreviewSource {
    /// A WebSocket on this machine.
    WebSocket { port: u16 },
    /// A relayed stream from the collab host.
    Collab(RelayClient),
}

struct PageImage {
    scale: f64,
    image: Arc<RenderImage>,
}

/// What changed between two document updates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentChange {
    /// Nothing that affects rendering.
    None,
    /// Only page content changed; the layout and old bitmaps still fit.
    Content,
    /// Page count or page sizes changed, so the pane must lay out again.
    Layout,
}

struct PreviewPage {
    index: usize,
    /// Page width in Typst points.
    width: f32,
    /// Page height in Typst points.
    height: f32,
    content_hash: Fingerprint,
    /// The most recent rasterization. It may belong to an older content hash;
    /// it stays on screen until its replacement arrives so updating does not
    /// flicker.
    image: Option<PageImage>,
    image_hash: Option<Fingerprint>,
    /// Text runs and links matching `image_hash`, used for selection and
    /// click-through.
    accessibility: Option<PageAccessibility>,
    /// `accessibility` summarised for assistive technology, built when the page
    /// is rasterized rather than in `render`.
    accessibility_label: SharedString,
    /// The rasterization in flight for this page, if any.
    in_flight: Option<InFlight>,
    /// A render of this content failed. Remembering it keeps a broken page
    /// (for example one the rasterizer panics on) from being retried on every
    /// update; an edit changes the hash and clears it.
    render_failure: Option<RenderFailure>,
}

#[derive(Clone)]
struct RenderFailure {
    content_hash: Fingerprint,
    /// The scale the failed request was for. A sharper request still retries
    /// once, so a transient failure at draft quality can recover.
    scale: f64,
    message: SharedString,
}

#[derive(Clone, Copy)]
struct InFlight {
    content_hash: Fingerprint,
    scale: f64,
    requested_at: Instant,
}

/// The word count of the active selection, valid for one selection and one
/// revision of the page text.
///
/// `render` rebuilds this on every frame otherwise, which is O(document) while
/// a select-all is active.
#[derive(Clone, Copy)]
struct CachedSelectionWords {
    selection: (TextPosition, TextPosition),
    text_generation: u64,
    words: WordsCount,
}

impl PreviewPage {
    /// Whether this page needs a new rasterization for `scale`.
    ///
    /// Only an *upgrade* counts: an image that is already crisper than the
    /// requested scale (a full-quality render surviving while the document is
    /// back in draft mode, or a zoom-out) is kept and downscaled on screen.
    /// Treating any difference as stale would make draft and full renders
    /// replace each other forever.
    fn needs_render(&self, scale: f64) -> bool {
        if self.render_failure.as_ref().is_some_and(|failure| {
            failure.content_hash == self.content_hash && scale <= failure.scale + 0.05
        }) {
            return false;
        }
        needs_render_for(
            self.image_hash,
            self.image.as_ref().map(|image| image.scale),
            self.content_hash,
            scale,
        )
    }
}

fn needs_render_for(
    image_hash: Option<Fingerprint>,
    image_scale: Option<f64>,
    content_hash: Fingerprint,
    scale: f64,
) -> bool {
    image_hash != Some(content_hash)
        || image_scale.is_none_or(|image_scale| image_scale + 0.05 < scale)
}

/// Orders the pages to render so the viewport sharpens before its buffer.
///
/// The session rasterizes its batch sequentially. Inside the batch the visible
/// pages must come first: when the document is zoomed in, only one page is on
/// screen, and rasterizing the prefetched page above it would delay the pixels
/// the user is waiting for.
fn order_render_pages(
    visible: std::ops::Range<usize>,
    start: usize,
    end: usize,
    mut needs_render: impl FnMut(usize) -> bool,
) -> Vec<usize> {
    let mut pages: Vec<usize> = (start..end).filter(|index| needs_render(*index)).collect();
    pages.sort_by_key(|index| {
        let distance = if *index < visible.start {
            visible.start - *index
        } else if *index >= visible.end {
            *index - visible.end + 1
        } else {
            0
        };
        (distance, *index)
    });
    pages
}

/// The largest scale at which a page can be rasterized safely.
///
/// Vello's binning only supports 256 bins of 256x256 pixels. Exceeding that
/// makes its shaders write out of bounds; on Intel and Apple GPUs it has hung
/// the whole graphics stack. The dimension and pixel budgets are stricter than
/// the bin limit for normal page shapes, but the bin term is kept explicit so
/// a future budget change cannot reintroduce the hang.
fn page_render_scale_cap(width: f32, height: f32) -> f64 {
    let width = f64::from(width.max(1.0));
    let height = f64::from(height.max(1.0));
    let area = width * height;
    let by_dim = MAX_TILE_DIM / width.max(height);
    let by_pixels = (MAX_TILE_PIXELS / area).sqrt();
    let by_bins = (VELLO_MAX_BINS * VELLO_BIN_SIZE * VELLO_BIN_SIZE / area).sqrt();
    by_dim.min(by_pixels).min(by_bins)
}

/// Applies the safe cap without letting a pathological page panic the UI.
///
/// `f64::clamp` panics when the minimum is greater than the maximum, which can
/// happen when a page is large enough that its safe scale is below 0.1. The
/// cap always wins: a tiny page is harmless, an over-sized Vello target is not.
fn clamp_render_scale(ideal: f64, cap: f64) -> f64 {
    let ideal = if ideal.is_finite() { ideal } else { 1.0 };
    ideal.max(0.1).min(cap)
}

/// A live Typst preview pane.
pub struct TypstPreviewView {
    focus_handle: FocusHandle,
    /// The previewed document on disk. Updated if the file is renamed.
    path: PathBuf,
    workspace: WeakEntity<Workspace>,
    project: Option<WeakEntity<Project>>,
    source_buffer: Option<WeakEntity<Buffer>>,
    /// The editor this preview was opened from, when it was a temporary
    /// preview tab. Such an editor is replaced by the pane when another file
    /// is previewed, so the preview follows the replacement.
    source_editor_id: Option<EntityId>,
    /// Set when the source editor was removed; the deferred check then looks
    /// for the editor that replaced it in the same pane.
    source_editor_removed: bool,
    /// Guards against starting more than one replacement preview while the
    /// first one is starting.
    retarget_pending: bool,
    owner: PreviewOwner,
    session: SessionHandle,
    pages: Vec<PreviewPage>,
    scroll_handle: ScrollHandle,
    zoom: f32,
    zoom_mode: ZoomMode,
    display_scale: f32,
    connected: bool,
    error: Option<SharedString>,
    /// `(errors, warnings)` reported for the previewed file.
    diagnostics: Option<(usize, usize)>,
    /// Rendered word count for the whole document, from the preview IR.
    word_count: Option<WordsCount>,
    /// A page point to bring into view once the page layout is known.
    pending_scroll: Option<(usize, f32, f32)>,
    /// The newest cursor position waiting for the follow debounce.
    pending_source_sync: Option<(PathBuf, u32, u32)>,
    _sync_task: Task<()>,
    /// The page number box in the pane's toolbar.
    page_input: Entity<Editor>,
    /// The page last written into `page_input`.
    shown_page: Option<usize>,
    /// Whether the next render should be at full display resolution.
    ///
    /// Cleared by every document update and set again once the document has
    /// been quiet for [`SETTLE_DELAY`].
    want_full: bool,
    /// A render was requested while another was still in flight.
    render_pending: bool,
    /// A pinch or wheel-zoom gesture is still running, so `request_render`
    /// waits for [`ZOOM_RENDER_DELAY`] instead of rendering every step.
    zoom_settling: bool,
    /// Bumped for every changed document; stale settle timers check it.
    document_generation: u64,
    selection: Option<Selection>,
    /// Word count for `selection`, valid for `text_generation`.
    cached_selection_words: Option<CachedSelectionWords>,
    /// Bumped whenever the page text changes: on a document update and whenever
    /// a rasterization delivers new text runs. Kept separate from
    /// `document_generation`, which also drives the settle timers.
    text_generation: u64,
    /// Opt-in frame-cost probe; see [`crate::frame_timing`].
    frame_timing: Option<FrameTiming>,
    /// The link under the pointer when the left button went down.
    pressed_link: Option<(usize, String)>,
    /// The link under the pointer now, used for the pointer cursor.
    hovered_link: Option<(usize, String)>,
    outline: Vec<OutlineEntry>,
    show_outline: bool,
    find_input: Entity<Editor>,
    find_open: bool,
    find_matches: Vec<FindMatch>,
    find_index: usize,
    /// Window-space bounds of each painted page, used for hit testing.
    page_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    _events: Task<()>,
    /// Polls the host relay; dropping it with the pane stops the stream.
    _collab_poll: Option<Task<()>>,
    _settle_task: Task<()>,
    /// Delays the render request of a continuous zoom gesture.
    _zoom_task: Task<()>,
    _page_input_subscription: Subscription,
    _find_input_subscription: Subscription,
    _buffer_subscription: Option<Subscription>,
    _editor_subscription: Option<Subscription>,
    _preview_tab_subscription: Option<Subscription>,
    _lsp_subscription: Option<Subscription>,
    _project_subscription: Option<Subscription>,
}

impl TypstPreviewView {
    /// Opens (or reveals) the preview for `path`, splitting the active pane.
    ///
    /// When `replace` is given, the preview view it names is swapped for the
    /// new one in place. That is how a preview opened from a temporary editor
    /// tab follows the pane when the tab opens another file.
    pub(crate) fn open(
        workspace: &mut Workspace,
        path: PathBuf,
        source_buffer: Option<Entity<Buffer>>,
        source_editor: Option<Entity<Editor>>,
        owner: PreviewOwner,
        source: PreviewSource,
        replace: Option<crate::start::PreviewPlacement>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let replacement = replace.as_ref().and_then(|placement| {
            placement
                .pane
                .read(cx)
                .items()
                .position(|item| item.item_id() == placement.view_id)
                .map(|index| (placement.pane.clone(), placement.view_id, index))
        });

        let existing = workspace.panes().iter().find_map(|pane| {
            let index = pane
                .read(cx)
                .items_of_type::<TypstPreviewView>()
                .find(|view| view.read(cx).path == path)
                .and_then(|view| pane.read(cx).index_for_item(&view))?;
            Some((pane.clone(), index))
        });
        if let Some((pane, index)) = existing {
            // A preview opened automatically must not steal focus from
            // the editor whose temporary tab just changed files.
            let focus_item = replace.is_none();
            pane.update(cx, |pane, cx| {
                pane.activate_item(index, true, focus_item, window, cx);
            });
            if let Some((old_pane, old_view_id, _)) = &replacement {
                old_pane.update(cx, |pane, cx| {
                    pane.close_item_by_id(*old_view_id, SaveIntent::Close, window, cx)
                        .detach();
                });
            }
            // The preview server started for this request is redundant.
            owner.kill(cx);
            cx.notify();
            return;
        }

        if replace.is_some() && replacement.is_none() {
            // The preview we were asked to replace is gone; do not open a
            // stray pane after the user already closed it.
            owner.kill(cx);
            return;
        }

        // Find the temporary editor's pane while we still have direct access
        // to the workspace. Reading the workspace entity here would panic: the
        // caller is already inside `workspace.update_in`.
        let preview_tab_pane = source_editor.as_ref().and_then(|editor| {
            let editor_id = editor.entity_id();
            workspace
                .panes()
                .iter()
                .find(|pane| pane.read(cx).is_active_preview_item(editor_id))
                .cloned()
        });

        let project = workspace.project().clone();
        let workspace_handle = cx.entity().downgrade();
        let view = cx.new(|cx| {
            Self::new(
                workspace_handle,
                project,
                path,
                source_buffer,
                source_editor,
                preview_tab_pane,
                owner,
                source,
                window,
                cx,
            )
        });

        if let Some((pane, old_view_id, index)) = replacement {
            pane.update(cx, |pane, cx| {
                let index = index.min(pane.items_len());
                pane.add_item_inner(Box::new(view), false, false, true, Some(index), window, cx);
                // Insert before closing: if the old preview was the pane's
                // only item, closing first would tear the pane down.
                pane.close_item_by_id(old_view_id, SaveIntent::Close, window, cx)
                    .detach();
            });
            cx.notify();
            return;
        }

        let pane = workspace.split_pane(
            workspace.active_pane().clone(),
            SplitDirection::Right,
            window,
            cx,
        );
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(view), true, true, None, window, cx)
        });
        cx.notify();
    }

    /// The file this preview is rendering. Follows renames of the source
    /// buffer.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Closes this preview when its temporary source tab was replaced by a
    /// file that cannot be previewed.
    fn close_self(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view_id = cx.entity_id();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let pane = {
            let workspace = workspace.read(cx);
            workspace
                .panes()
                .iter()
                .find(|pane| pane.read(cx).items().any(|item| item.item_id() == view_id))
                .cloned()
        };
        if let Some(pane) = pane {
            pane.update(cx, |pane, cx| {
                pane.close_item_by_id(view_id, SaveIntent::Close, window, cx)
                    .detach();
            });
        }
    }

    /// Replaces this preview with one for `editor`, in the same pane.
    fn retarget_to_editor(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view_id = cx.entity_id();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(pane) = ({
            let workspace = workspace.read(cx);
            workspace
                .panes()
                .iter()
                .find(|pane| pane.read(cx).items().any(|item| item.item_id() == view_id))
                .cloned()
        }) else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            crate::start::open_preview_for_editor_replacing(
                workspace,
                editor,
                crate::start::PreviewPlacement { pane, view_id },
                window,
                cx,
            );
        });
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

    fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        path: PathBuf,
        source_buffer: Option<Entity<Buffer>>,
        source_editor: Option<Entity<Editor>>,
        preview_tab_pane: Option<Entity<workspace::Pane>>,
        owner: PreviewOwner,
        source: PreviewSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let is_collab = matches!(&source, PreviewSource::Collab(_));
        let (page_input, page_input_subscription) = Self::new_page_input(window, cx);
        let find_input = cx.new(|cx| Editor::single_line(window, cx));
        let find_input_subscription = cx.subscribe_in(
            &find_input,
            window,
            |this, _, event: &EditorEvent, _window, cx| {
                if matches!(event, EditorEvent::Edited { .. }) {
                    this.refresh_find_matches(cx);
                    if !this.find_matches.is_empty() {
                        this.find_index = 0;
                        this.show_find_match(cx);
                    }
                }
            },
        );
        let (zoom_mode, zoom) = saved_zooms()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&zoom_key(&path))
            .map(|saved| (saved.mode, saved.zoom))
            .unwrap_or((ZoomMode::FitWidth, 1.0));

        let source_file = source_buffer.as_ref().map(|buffer| buffer.downgrade());
        let buffer_subscription = source_buffer.map(|buffer| {
            cx.subscribe(&buffer, |this, buffer, event: &BufferEvent, cx| {
                if matches!(event, BufferEvent::FileHandleChanged) {
                    if let Some(path) = buffer
                        .read(cx)
                        .file()
                        .and_then(|file| file.file_system_abs_path(cx))
                    {
                        this.path = path;
                    }
                    if let Some(project) =
                        this.project.as_ref().and_then(|project| project.upgrade())
                    {
                        this.diagnostics = diagnostic_counts(&project, &this.path, cx);
                    }
                    cx.emit(TypstPreviewEvent::TitleChanged);
                    cx.notify();
                }
            })
        });

        // A preview opened from a temporary editor tab must follow that tab
        // when it opens another file. Zed replaces the preview item itself, so
        // the original editor entity is removed and a new one is added to the
        // same pane. Listen on the source pane: reading the workspace from its
        // own update would panic.
        let source_editor_id = source_editor.as_ref().map(|editor| editor.entity_id());
        let preview_tab_subscription = preview_tab_pane.map(|pane| {
            cx.subscribe_in(
                &pane,
                window,
                move |this, _pane, event: &workspace::pane::Event, window, cx| {
                    match event {
                        workspace::pane::Event::RemovedItem { item }
                            if Some(item.item_id()) == this.source_editor_id =>
                        {
                            this.source_editor_removed = true;
                            // A replacement is added in the same update; if none
                            // arrives, the user simply closed the temporary tab
                            // and a later unrelated preview tab must not pull
                            // this pane along.
                            cx.defer_in(window, |this, _, _| {
                                this.source_editor_removed = false;
                            });
                        }
                        workspace::pane::Event::AddItem { item }
                            if this.source_editor_removed && !this.retarget_pending =>
                        {
                            let editor = item.downcast::<Editor>();
                            this.retarget_pending = true;
                            // This callback runs in the middle of the pane
                            // update. Closing a pane or updating the workspace
                            // has to wait until that update finishes.
                            cx.defer_in(window, move |this, window, cx| match editor {
                                Some(editor) if crate::start::is_typst_editor(&editor, cx) => {
                                    this.retarget_to_editor(editor, window, cx);
                                }
                                _ => this.close_self(window, cx),
                            });
                        }
                        _ => {}
                    }
                },
            )
        });

        let outline_server_id = owner.server_id;
        let lsp_subscription = outline_server_id.and_then(|outline_server_id| {
            owner.lsp_store.upgrade().map(|store| {
                store.update(cx, |store, cx| {
                    store.watch_custom_notification(outline_server_id, OUTLINE_METHOD, cx);
                });
                cx.subscribe(&store, move |this, _, event: &LspStoreEvent, cx| {
                    if let LspStoreEvent::CustomNotification {
                        server_id,
                        method,
                        params,
                    } = event
                        && *server_id == outline_server_id
                        && *method == OUTLINE_METHOD
                        && is_primary_preview(&this.path)
                    {
                        this.outline = parse_outline(params);
                        cx.notify();
                    }
                })
            })
        });

        let diagnostics = diagnostic_counts(&project, &path, cx);
        let project_subscription = Some(cx.subscribe(
            &project,
            move |this, project, event: &project::Event, cx| {
                if matches!(event, project::Event::DiagnosticsUpdated { .. }) {
                    this.diagnostics = diagnostic_counts(&project, &this.path, cx);
                    cx.notify();
                }
            },
        ));

        let editor_subscription = if is_collab {
            // Forward sync is deliberately off in shared projects: one
            // Tinymist task has one viewport, so a cursor move would scroll
            // everyone's preview.
            None
        } else {
            source_editor.as_ref().map(|editor| {
                cx.subscribe(editor, |this, editor, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::SelectionsChanged { local: true })
                        && let Some((path, line, character)) = editor.read_with(cx, |editor, cx| {
                            crate::start::cursor_source_point(editor, cx)
                        })
                    {
                        this.schedule_source_sync(path, line, character, cx);
                    }
                })
            })
        };

        let (events_tx, events_rx) = smol::channel::unbounded();
        let (session, collab_poll) = match source {
            PreviewSource::WebSocket { port } => (
                SessionHandle::spawn(PreviewConnectionConfig::loopback(port), events_tx),
                None,
            ),
            PreviewSource::Collab(mut relay) => {
                let (inputs, inputs_rx) = std::sync::mpsc::channel();
                let session =
                    SessionHandle::spawn_with_inputs(inputs.clone(), inputs_rx, events_tx);
                inputs
                    .send(SessionInput::Event(PreviewEvent::Connected))
                    .ok();
                let poll = cx.spawn_in(window, async move |this, mut cx| {
                    loop {
                        match relay.next(&mut cx).await {
                            Ok(Some(RelayItem::Frame(frame))) => {
                                if inputs
                                    .send(SessionInput::Event(PreviewEvent::Binary(frame)))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(Some(RelayItem::Outline(outline))) => {
                                if inputs.send(SessionInput::Outline(outline)).is_err() {
                                    break;
                                }
                            }
                            Ok(Some(RelayItem::Stopped(reason))) => {
                                if !reason.is_empty() {
                                    inputs
                                        .send(SessionInput::Event(PreviewEvent::ConnectFailed(
                                            reason,
                                        )))
                                        .ok();
                                }
                                inputs.send(SessionInput::Disconnect).ok();
                                break;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                inputs
                                    .send(SessionInput::Event(PreviewEvent::ConnectFailed(
                                        format!("{error:#}"),
                                    )))
                                    .ok();
                                inputs.send(SessionInput::Disconnect).ok();
                                break;
                            }
                        }
                        if this.update(cx, |_, _| {}).is_err() {
                            break;
                        }
                    }
                });
                (session, Some(poll))
            }
        };

        // `on_release` runs outside a window update, so `App::drop_image(None)`
        // can still reach the window. Use it for the final set of page images;
        // `on_removed` may run while the window is temporarily out of
        // `App.windows` and would not free the atlas tiles.
        cx.on_release(|view, cx| {
            for page in &view.pages {
                if let Some(image) = &page.image {
                    cx.drop_image(image.image.clone(), None);
                }
            }
        })
        .detach();
        let events = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events_rx.recv().await {
                if this
                    .update_in(cx, |view, window, cx| {
                        view.handle_session_event(event, window, cx);
                    })
                    .is_err()
                {
                    break;
                }
                // `recv` returns immediately while events are queued, so a
                // compile burst would otherwise keep this loop from ever
                // yielding. That runs on GPUI's foreground executor: input and
                // painting would stall until the burst drained. Yield after
                // every event so the editor stays responsive (and the preview
                // may drop intermediate frames, which is fine).
                smol::future::yield_now().await;
            }
        });

        Self {
            focus_handle: cx.focus_handle(),
            path,
            workspace,
            project: Some(project.downgrade()),
            source_buffer: source_file,
            source_editor_id,
            source_editor_removed: false,
            retarget_pending: false,
            owner,
            session,
            pages: Vec::new(),
            scroll_handle: ScrollHandle::new(),
            zoom,
            zoom_mode,
            display_scale: 1.0,
            connected: false,
            error: None,
            diagnostics,
            word_count: None,
            pending_scroll: None,
            pending_source_sync: None,
            _sync_task: Task::ready(()),
            page_input,
            shown_page: None,
            want_full: false,
            render_pending: false,
            zoom_settling: false,
            document_generation: 0,
            selection: None,
            cached_selection_words: None,
            text_generation: 0,
            pressed_link: None,
            hovered_link: None,
            outline: Vec::new(),
            show_outline: false,
            find_input,
            find_open: false,
            find_matches: Vec::new(),
            find_index: 0,
            page_bounds: Rc::default(),
            frame_timing: FrameTiming::from_env(),
            _events: events,
            _collab_poll: collab_poll,
            _settle_task: Task::ready(()),
            _zoom_task: Task::ready(()),
            _page_input_subscription: page_input_subscription,
            _find_input_subscription: find_input_subscription,
            _buffer_subscription: buffer_subscription,
            _editor_subscription: editor_subscription,
            _preview_tab_subscription: preview_tab_subscription,
            _lsp_subscription: lsp_subscription,
            _project_subscription: project_subscription,
        }
    }

    fn handle_session_event(
        &mut self,
        event: SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut notify = true;
        match event {
            SessionEvent::Connected => {
                log::info!("typst live preview: connected for {}", self.path.display());
                self.connected = true;
                self.error = None;
            }
            SessionEvent::Document { pages } => {
                let first_document = self.pages.is_empty();
                match self.apply_document(pages, window, cx) {
                    DocumentChange::None => notify = false,
                    DocumentChange::Content | DocumentChange::Layout => {
                        // A new document wins over a zoom gesture that has not
                        // requested its pixels yet.
                        self.zoom_settling = false;
                        self._zoom_task = Task::ready(());
                        // Draw the new content now at draft resolution; the
                        // settle timer replaces it with a sharp render once
                        // typing stops. The very first render has nothing to
                        // sharpen later, so it goes straight to full quality.
                        self.want_full = first_document;
                        self.schedule_settle(cx);
                        self.request_render();
                    }
                }
            }
            SessionEvent::Rendered {
                index,
                content_hash,
                scale,
                image,
                accessibility,
            } => {
                let (full_scale, draft_scale) = (self.render_scale(), self.draft_scale());
                if let Some(page) = self.pages.get_mut(index) {
                    // Every arrival completes the batch that produced it, even
                    // when the document moved on: otherwise the page would
                    // stay marked in flight and block further renders.
                    let requested = page.in_flight.take();
                    if page.content_hash == content_hash {
                        // A successful render clears both the remembered
                        // failure and the status-bar message it produced.
                        if let Some(failure) = page.render_failure.take()
                            && self.error.as_ref() == Some(&failure.message)
                        {
                            self.error = None;
                        }
                        page.accessibility_label = page_label(index, Some(&accessibility));
                        page.accessibility = Some(accessibility);
                        self.text_generation = self.text_generation.wrapping_add(1);
                        // A render that lost a zoom change or a newer request
                        // is stale even if the hash still matches.
                        let current = match requested {
                            Some(requested) => {
                                requested.content_hash == content_hash
                                    && (requested.scale - scale).abs() <= 0.05
                            }
                            None => {
                                (full_scale - scale).abs() <= 0.05
                                    || (draft_scale - scale).abs() <= 0.05
                            }
                        };
                        if current {
                            if let Some(previous) = page.image.replace(PageImage { scale, image }) {
                                cx.drop_image(previous.image, Some(window));
                            }
                            page.image_hash = Some(content_hash);
                            if let Some(requested) = requested {
                                log::debug!(
                                    "typst live preview: page {index} rendered at {scale:.2}x in {:?}",
                                    requested.requested_at.elapsed()
                                );
                            }
                        }
                    }
                }
                if self.find_open {
                    self.refresh_find_matches(cx);
                }
            }
            SessionEvent::Skipped { index } => {
                if let Some(page) = self.pages.get_mut(index) {
                    page.in_flight = None;
                }
            }
            SessionEvent::RenderFailed {
                index,
                content_hash,
                scale,
                error,
            } => {
                if let Some(page) = self.pages.get_mut(index) {
                    // The batch is over for this page. Remember the failure so
                    // `request_render` does not immediately ask for the same
                    // broken content again; the next edit changes the hash.
                    page.in_flight = None;
                    page.render_failure = Some(RenderFailure {
                        content_hash,
                        scale,
                        message: error.clone().into(),
                    });
                }
                self.error = Some(error.into());
            }
            SessionEvent::ScrollTo { page, x, y } => {
                self.scroll_to_page_point(page, x, y, cx);
            }
            SessionEvent::WordCount(count) => {
                self.word_count = Some(count);
            }
            SessionEvent::Error(error) => {
                self.error = Some(error.into());
            }
            SessionEvent::Disconnected => {
                self.connected = false;
            }
            SessionEvent::Outline(outline) => {
                if is_primary_preview(&self.path) {
                    self.outline = parse_outline(&outline);
                } else {
                    notify = false;
                }
            }
        }
        if notify {
            cx.notify();
        }

        // A render finished or the document moved on: keep the pipeline busy
        // with the newest content instead of queueing every intermediate frame.
        if !self.pages.iter().any(|page| page.in_flight.is_some()) && self.render_pending {
            self.render_pending = false;
            self.request_render();
        }
    }

    /// Waits for the document to settle, then asks for a sharp re-render.
    ///
    /// The render that runs on every update is deliberately soft (see
    /// [`SETTLE_DELAY`]). This is the only place that asks for display
    /// resolution, so a fast typist never pays for it per keystroke.
    fn schedule_settle(&mut self, cx: &mut Context<Self>) {
        let generation = self.document_generation;
        self._settle_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.document_generation == generation {
                    this.want_full = true;
                    this.request_render();
                    cx.notify();
                }
            })
            .ok();
        });
    }

    fn apply_document(
        &mut self,
        metas: Vec<PageMeta>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DocumentChange {
        let same_layout = self.pages.len() == metas.len()
            && self.pages.iter().zip(&metas).all(|(page, meta)| {
                page.index == meta.index
                    && page.width == meta.size.width as f32
                    && page.height == meta.size.height as f32
            });
        let same_content = same_layout
            && self
                .pages
                .iter()
                .zip(&metas)
                .all(|(page, meta)| page.content_hash == meta.content_hash);
        if same_content {
            return DocumentChange::None;
        }

        self.document_generation = self.document_generation.wrapping_add(1);
        self.text_generation = self.text_generation.wrapping_add(1);

        let prev = std::mem::take(&mut self.pages);
        let mut pages = Vec::with_capacity(metas.len());
        for meta in metas {
            // Page indices are dense and ordered, so the previous page for
            // this index sits at the same position.
            let old = prev.get(meta.index);
            pages.push(PreviewPage {
                index: meta.index,
                width: meta.size.width as f32,
                height: meta.size.height as f32,
                content_hash: meta.content_hash,
                image: old.and_then(|page| {
                    page.image.as_ref().map(|image| PageImage {
                        scale: image.scale,
                        image: image.image.clone(),
                    })
                }),
                image_hash: old.and_then(|page| page.image_hash),
                // Keep the previous hit-testing geometry until the new bitmap
                // arrives: it is still roughly correct and keeps an active
                // selection highlighted while typing.
                accessibility: old.and_then(|page| page.accessibility.clone()),
                // The summary follows the text it was built from, so a page
                // whose new runs have not arrived keeps the old one.
                accessibility_label: old
                    .map(|page| page.accessibility_label.clone())
                    .unwrap_or_else(|| page_label(meta.index, None)),
                // Keep the request in flight across content changes: the batch
                // is already running, and `render_pending` re-issues the newest
                // content when it finishes. Clearing it here would queue one
                // render per keystroke behind a slow rasterizer.
                in_flight: old.and_then(|page| page.in_flight),
                // A failure only describes the content it was rendered from.
                render_failure: old
                    .and_then(|page| page.render_failure.clone())
                    .filter(|failure| failure.content_hash == meta.content_hash),
            });
        }

        // Pages past the end of the new document will never be rendered again,
        // so their atlas tiles have to go explicitly.
        for page in prev.into_iter().skip(pages.len()) {
            if let Some(image) = page.image {
                cx.drop_image(image.image, Some(&mut *window));
            }
        }

        self.pages = pages;
        if same_layout {
            DocumentChange::Content
        } else {
            DocumentChange::Layout
        }
    }

    /// The scale that fits the widest page to the pane's width.
    fn width_fit_scale(&self) -> f32 {
        let viewport_width: f32 = self.scroll_handle.bounds().size.width.into();
        if viewport_width <= 0.0 || self.pages.is_empty() {
            return 1.0;
        }
        let content_width = (viewport_width - 2.0 * CANVAS_PADDING).max(1.0);
        let widest = self
            .pages
            .iter()
            .map(|page| page.width)
            .fold(0.0_f32, f32::max)
            .max(1.0);
        content_width / widest
    }

    /// The scale that fits a whole page in the pane.
    fn page_fit_scale(&self) -> f32 {
        let viewport = self.scroll_handle.bounds().size;
        let (viewport_width, viewport_height): (f32, f32) =
            (viewport.width.into(), viewport.height.into());
        if viewport_width <= 0.0 || viewport_height <= 0.0 || self.pages.is_empty() {
            return 1.0;
        }
        let widest = self
            .pages
            .iter()
            .map(|page| page.width)
            .fold(0.0_f32, f32::max)
            .max(1.0);
        let tallest = self
            .pages
            .iter()
            .map(|page| page.height)
            .fold(0.0_f32, f32::max)
            .max(1.0);
        let by_width = (viewport_width - 2.0 * CANVAS_PADDING).max(1.0) / widest;
        let by_height = (viewport_height - 2.0 * CANVAS_PADDING).max(1.0) / tallest;
        by_width.min(by_height)
    }

    /// The current page-to-pixel scale.
    fn page_scale(&self) -> f32 {
        match self.zoom_mode {
            ZoomMode::FitWidth => self.width_fit_scale(),
            ZoomMode::FitPage => self.page_fit_scale(),
            ZoomMode::ActualSize => 1.0,
            ZoomMode::Custom => self.zoom,
        }
    }

    /// The device-pixel scale pages must be rasterized at to look crisp.
    fn render_scale(&self) -> f64 {
        let ideal = f64::from(self.page_scale() * self.display_scale);
        let cap = self
            .pages
            .iter()
            .map(|page| page_render_scale_cap(page.width, page.height))
            .fold(f64::INFINITY, f64::min);
        clamp_render_scale(ideal, cap)
    }

    /// The softer scale used while the document is still changing.
    fn draft_scale(&self) -> f64 {
        let full = self.render_scale();
        (full * DRAFT_SCALE_FACTOR).max(0.1).min(full)
    }

    /// Asks the session to rasterize the pages that need the current quality.
    ///
    /// Only one batch is ever in flight: if a render is already running, the
    /// request is remembered and re-issued when the batch finishes, with the
    /// newest content. That keeps a fast typist from queueing a render per
    /// keystroke while still showing every settled state.
    ///
    /// Page order matters: the session rasterizes sequentially, so the pages
    /// under the viewport are requested before the prefetch buffer around them.
    fn request_render(&mut self) {
        if self.pages.is_empty() || self.zoom_settling {
            return;
        }
        let now = Instant::now();
        for page in &mut self.pages {
            if page.in_flight.as_ref().is_some_and(|request| {
                now.saturating_duration_since(request.requested_at) > IN_FLIGHT_TIMEOUT
            }) {
                page.in_flight = None;
            }
        }
        if self.pages.iter().any(|page| page.in_flight.is_some()) {
            self.render_pending = true;
            return;
        }

        let bounds = self.scroll_handle.bounds();
        let viewport_height: f32 = bounds.size.height.into();
        if viewport_height <= 0.0 {
            return;
        }

        let scale = if self.want_full {
            self.render_scale()
        } else {
            self.draft_scale()
        };
        let visible = self.visible_pages(viewport_height);
        let start = visible.start.saturating_sub(RENDER_BUFFER_PAGES);
        let end = (visible.end + RENDER_BUFFER_PAGES).min(self.pages.len());
        let requested_at = Instant::now();

        let needed: Vec<(usize, Fingerprint)> = order_render_pages(visible, start, end, |index| {
            self.pages[index].needs_render(scale)
        })
        .into_iter()
        .map(|index| (index, self.pages[index].content_hash))
        .collect();
        if needed.is_empty() {
            return;
        }

        log::debug!(
            "typst live preview: requesting {} page(s) at {scale:.2}x ({})",
            needed.len(),
            if self.want_full { "full" } else { "draft" },
        );
        for (index, hash) in &needed {
            self.pages[*index].in_flight = Some(InFlight {
                content_hash: *hash,
                scale,
                requested_at,
            });
        }

        self.session.request_pages(scale, needed);
    }

    fn visible_pages(&self, viewport_height: f32) -> std::ops::Range<usize> {
        let scroll_y: f32 = self.scroll_handle.offset().y.into();
        let scroll_offset = (scroll_y.abs() - CANVAS_PADDING).max(0.0);
        let scale = self.page_scale();
        let mut y = 0.0;
        let mut first = None;
        let mut last = 0;

        for (index, page) in self.pages.iter().enumerate() {
            let page_height = page.height * scale;
            let top = y;
            let bottom = y + page_height;

            if bottom > scroll_offset && first.is_none() {
                first = Some(index);
            }
            if top < scroll_offset + viewport_height {
                last = index;
            }
            if top > scroll_offset + viewport_height {
                break;
            }

            y = bottom + PAGE_GAP;
        }

        let first = first.unwrap_or(0);
        first..(last + 1).min(self.pages.len())
    }

    /// The page under the top of the viewport, used by the page box.
    fn current_page(&self) -> usize {
        let scroll_offset =
            (f32::from(self.scroll_handle.offset().y).abs() - CANVAS_PADDING).max(0.0);
        let scale = self.page_scale();
        let mut y = 0.0;
        for (index, page) in self.pages.iter().enumerate() {
            let bottom = y + page.height * scale;
            if bottom > scroll_offset {
                return index;
            }
            y = bottom + PAGE_GAP;
        }
        self.pages.len().saturating_sub(1)
    }

    fn render_page_element(&self, page: &PreviewPage, scale: f32, cx: &App) -> AnyElement {
        let display_width = px(page.width * scale);
        let display_height = px(page.height * scale);
        let index = page.index;
        // The pages are rasterized images, so assistive technology only gets a
        // text summary. Full per-run accessibility nodes would need a real
        // AccessKit tree mirroring the page IR. The summary is built when the
        // page is rasterized: this function runs for every page on every frame,
        // so walking the text runs here scaled with the document.
        let aria_label = page.accessibility_label.clone();

        let content = if let Some(image) = &page.image {
            img(image.image.clone())
                .id(("typst-live-page", index))
                .w(display_width)
                .h(display_height)
                .into_any_element()
        } else {
            div()
                .id(("typst-live-page-placeholder", index))
                .w(display_width)
                .h(display_height)
                // A page is a sheet of paper, not a theme surface.
                .bg(gpui::white())
                .flex()
                .justify_center()
                .items_center()
                .child(Label::new(format!("Page {}", index + 1)).color(Color::Muted))
                .into_any_element()
        };

        div()
            .id(("typst-live-page-container", index))
            .aria_label(aria_label)
            .debug_selector(|| format!("typst-live-page-{index}"))
            .relative()
            .w(display_width)
            .h(display_height)
            .shadow_md()
            .child(content)
            .children(self.selection_highlights_for_page(page, scale, cx))
            .children(self.find_highlights_for_page(page, scale, cx))
            .child({
                let page_bounds = self.page_bounds.clone();
                canvas(
                    move |bounds, _, _| {
                        page_bounds.borrow_mut().insert(index, bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .border_1()
                    .border_color(cx.theme().colors().border),
            )
            .into_any_element()
    }

    // -- Selection --

    /// The page under a window position and the point in page coordinates.
    ///
    /// The scale comes from the painted page bounds rather than the requested
    /// layout scale: layout rounds to device pixels, and recomputing the scale
    /// by summing page heights drifts deeper into the document.
    fn point_to_page_point(&self, window_position: Point<Pixels>) -> Option<(usize, kurbo::Point)> {
        let page_bounds = self.page_bounds.borrow();
        let (&page_index, bounds) = page_bounds
            .iter()
            .find(|(_, bounds)| bounds.contains(&window_position))?;
        let page = self.pages.get(page_index)?;
        let display_width: f32 = bounds.size.width.into();
        if page.width <= 0.0 || display_width <= 0.0 {
            return None;
        }

        let scale = display_width / page.width;
        let local = window_position - bounds.origin;
        Some((
            page_index,
            kurbo::Point::new(
                f64::from(f32::from(local.x)) / f64::from(scale),
                f64::from(f32::from(local.y)) / f64::from(scale),
            ),
        ))
    }

    fn point_to_text_position(&self, window_position: Point<Pixels>) -> Option<TextPosition> {
        let (page_index, point) = self.point_to_page_point(window_position)?;
        let page = self.pages.get(page_index)?;
        let position = page.accessibility.as_ref()?.hit_test_text(point)?;
        Some(TextPosition {
            page: page_index,
            run: position.run_index,
            character: position.character_index,
        })
    }

    fn link_at(&self, window_position: Point<Pixels>) -> Option<(usize, &str)> {
        let (page_index, point) = self.point_to_page_point(window_position)?;
        let href = self
            .pages
            .get(page_index)?
            .accessibility
            .as_ref()?
            .hit_test_link(point)?;
        Some((page_index, href))
    }

    fn normalized_selection(&self) -> Option<(TextPosition, TextPosition)> {
        let selection = self.selection?;
        if selection.anchor == selection.focus {
            return None;
        }
        Some(if selection.anchor <= selection.focus {
            (selection.anchor, selection.focus)
        } else {
            (selection.focus, selection.anchor)
        })
    }

    fn selected_text(&self) -> String {
        let Some((start, end)) = self.normalized_selection() else {
            return String::new();
        };
        selection_text(start, end, |page_index| {
            self.pages
                .get(page_index)
                .and_then(|page| page.accessibility.as_ref())
        })
    }

    /// The word count of the active selection, cached for the current selection
    /// and revision of the page text.
    ///
    /// `render` rebuilds the selected text on every frame otherwise, which is
    /// O(document) while a select-all is active.
    fn selection_word_count(&mut self) -> Option<WordsCount> {
        let selection = self.normalized_selection()?;
        let text_generation = self.text_generation;

        if let Some(cached) = self.cached_selection_words
            && cached.selection == selection
            && cached.text_generation == text_generation
        {
            return (cached.words.chars > 0).then_some(cached.words);
        }

        let words = count_words(&self.selected_text());
        self.cached_selection_words = Some(CachedSelectionWords {
            selection,
            text_generation,
            words,
        });
        (words.chars > 0).then_some(words)
    }

    fn selection_highlights_for_page(
        &self,
        page: &PreviewPage,
        display_scale: f32,
        cx: &App,
    ) -> Vec<AnyElement> {
        let Some((start, end)) = self.normalized_selection() else {
            return Vec::new();
        };
        let Some(accessibility) = &page.accessibility else {
            return Vec::new();
        };
        let Some(selection) = page_selection(page.index, accessibility, start, end) else {
            return Vec::new();
        };

        let color = cx.theme().colors().element_selection_background;
        accessibility
            .selection_rects(selection)
            .into_iter()
            .map(|rect| {
                div()
                    .absolute()
                    .left(px(rect.x0 as f32 * display_scale))
                    .top(px(rect.y0 as f32 * display_scale))
                    .w(px(rect.width() as f32 * display_scale))
                    .h(px(rect.height() as f32 * display_scale))
                    .bg(color)
                    .into_any_element()
            })
            .collect()
    }

    fn select_word_at(&mut self, position: TextPosition, whole_run: bool) {
        let Some(page) = self.pages.get(position.page) else {
            return;
        };
        let Some(accessibility) = &page.accessibility else {
            return;
        };
        let Some(run) = accessibility.text_runs().get(position.run) else {
            return;
        };

        let characters = run.text().chars().collect::<Vec<_>>();
        let Some((start, end)) = word_selection(&characters, position.character, whole_run) else {
            self.selection = None;
            return;
        };

        self.selection = Some(Selection {
            anchor: TextPosition {
                page: position.page,
                run: position.run,
                character: start,
            },
            focus: TextPosition {
                page: position.page,
                run: position.run,
                character: end,
            },
            dragging: false,
        });
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        window.focus(&self.focus_handle, cx);

        if event.click_count >= 2 {
            if let Some(position) = self.point_to_text_position(event.position) {
                self.select_word_at(position, event.click_count >= 3);
            } else {
                self.selection = None;
            }
            self.pressed_link = None;
            cx.notify();
            return;
        }

        // Shift-click extends the existing selection instead of starting a new
        // one, matching the editor's selection behaviour.
        let position = self.point_to_text_position(event.position);
        if event.modifiers.shift
            && let Some(selection) = &mut self.selection
            && let Some(position) = position
        {
            selection.focus = position;
            selection.dragging = true;
            self.pressed_link = None;
            cx.notify();
            return;
        }

        self.pressed_link = self
            .link_at(event.position)
            .map(|(page, href)| (page, href.to_string()));
        self.selection = self
            .point_to_text_position(event.position)
            .map(|position| Selection {
                anchor: position,
                focus: position,
                dragging: true,
            });
        cx.notify();
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hovered = self
            .link_at(event.position)
            .map(|(page, href)| (page, href.to_string()));
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            cx.notify();
        }

        let position = self.point_to_text_position(event.position);
        let Some(selection) = &mut self.selection else {
            return;
        };
        if !selection.dragging {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            selection.dragging = false;
            return;
        }

        if let Some(position) = position {
            selection.focus = position;
            cx.notify();
        }
    }

    fn handle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut selection) = self.selection.take() else {
            return;
        };
        selection.dragging = false;

        if selection.anchor == selection.focus {
            // A click, not a drag: follow a link if the pointer is still on
            // the one it went down on, otherwise jump to the source of the
            // text under it.
            let released = self
                .link_at(event.position)
                .map(|(page, href)| (page, href.to_string()));
            let pressed = self.pressed_link.take();
            match click_action(
                pressed.as_ref().map(|(page, href)| (*page, href.as_str())),
                released.as_ref().map(|(page, href)| (*page, href.as_str())),
            ) {
                ClickAction::FollowLink => {
                    if let Some((_, href)) = pressed {
                        self.follow_link(&href, window, cx);
                    }
                }
                ClickAction::JumpToSource => self.request_source_jump(event.position, window, cx),
            }
            self.selection = None;
        } else {
            self.selection = Some(selection);
        }
        cx.notify();
    }

    /// Asks Tinymist to open the source location that produced the clicked
    /// page point.
    ///
    /// Asks Tinymist to open the source location that produced the clicked
    /// page point.
    ///
    /// For a local or SSH preview, Tinymist owns the span map and answers with
    /// an LSP `window/showDocument`. In a collaborative preview, the host
    /// resolves the point and returns the location to this guest, so only the
    /// user who clicked jumps.
    fn request_source_jump(
        &self,
        window_position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((page_index, point)) = self.point_to_page_point(window_position) else {
            return;
        };
        let Some(collab) = self.owner.collab() else {
            self.session
                .request_source_jump(page_index, point.x, point.y);
            return;
        };

        let Some(lsp_store) = self.owner.lsp_store.upgrade() else {
            return;
        };
        let Some(server_id) = self.owner.server_id else {
            return;
        };
        let stream_id = collab.stream_id.clone();
        let project = self.project.clone();
        cx.spawn_in(window, async move |_this, mut cx| {
            let Ok(Some((uri, selection))) = RelayClient::resolve_at(
                &lsp_store, server_id, &stream_id, page_index, point.x, point.y, &mut cx,
            )
            .await
            else {
                return;
            };
            let Some(project) = project.as_ref().and_then(|project| project.upgrade()) else {
                return;
            };
            project.update(cx, |project, cx| {
                project.show_document(
                    project::LanguageServerShowDocumentRequest::without_response(
                        uri, true, selection,
                    ),
                    cx,
                );
            });
        })
        .detach();
    }

    // -- Zoom and navigation --

    /// The scrollable content size at the current zoom.
    ///
    /// [`ScrollHandle::max_offset`] is only refreshed during layout, so a zoom
    /// change has to predict the new content size itself. Otherwise the offset
    /// is clamped against the previous zoom's bounds and the view requests
    /// pages for a position that the next layout immediately scrolls away
    /// from.
    fn content_size(&self) -> gpui::Size<Pixels> {
        let sizes = self.pages.iter().map(|page| (page.width, page.height));
        let (width, height) = content_extent(sizes, self.page_scale());
        size(px(width), px(height))
    }

    fn max_scroll_offset(&self) -> Point<Pixels> {
        let viewport = self.scroll_handle.bounds().size;
        let content = self.content_size();
        point(
            (content.width - viewport.width).max(px(0.0)),
            (content.height - viewport.height).max(px(0.0)),
        )
    }

    fn set_scroll_offset(&self, offset: Point<Pixels>) {
        self.set_scroll_offset_within(offset, self.scroll_handle.max_offset());
    }

    fn set_scroll_offset_within(&self, offset: Point<Pixels>, max: Point<Pixels>) {
        let clamps = |value: Pixels, max: Pixels| {
            if value < -max {
                -max
            } else if value > px(0.0) {
                px(0.0)
            } else {
                value
            }
        };
        self.scroll_handle
            .set_offset(point(clamps(offset.x, max.x), clamps(offset.y, max.y)));
    }

    fn scroll_by(&mut self, dx: Pixels, dy: Pixels, cx: &mut Context<Self>) {
        let offset = self.scroll_handle.offset();
        self.set_scroll_offset(point(offset.x + dx, offset.y + dy));
        cx.notify();
    }

    fn scroll_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        let viewport_height = self.scroll_handle.bounds().size.height;
        let step = (viewport_height - px(SCROLL_PAGE_MARGIN_PX)).max(px(SCROLL_LINE_PX));
        self.scroll_by(px(0.0), -step * direction, cx);
    }

    fn set_zoom(
        &mut self,
        new_scale: f32,
        center: Option<Point<Pixels>>,
        render: ZoomRender,
        cx: &mut Context<Self>,
    ) {
        self.apply_zoom(ZoomMode::Custom, new_scale, center, render, cx);
    }

    /// The viewport point to keep fixed when zooming without a cursor target.
    ///
    /// Keyboard zoom and the fit modes use the viewport center. Scaling around
    /// the top-left instead makes the page jump by an amount proportional to
    /// the current scroll offset.
    fn zoom_anchor(&self) -> Option<Point<Pixels>> {
        let bounds = self.scroll_handle.bounds();
        (bounds.size.width > px(0.0) && bounds.size.height > px(0.0)).then(|| bounds.center())
    }

    /// Switches the zoom mode and/or scale, keeping the point under `center`
    /// fixed when one is given.
    fn apply_zoom(
        &mut self,
        mode: ZoomMode,
        zoom: f32,
        center: Option<Point<Pixels>>,
        render: ZoomRender,
        cx: &mut Context<Self>,
    ) {
        let old_scale = self.page_scale();
        self.zoom_mode = mode;
        if mode == ZoomMode::Custom {
            self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.save_zoom();
        let new_scale = self.page_scale();
        if (new_scale - old_scale).abs() <= 0.001 {
            cx.notify();
            return;
        }

        let target = if let Some(cursor) = center
            && old_scale > 0.0
        {
            let bounds = self.scroll_handle.bounds();
            let offset = self.scroll_handle.offset();
            let cursor_in_container = point(cursor.x - bounds.origin.x, cursor.y - bounds.origin.y);
            Some(anchored_scroll_offset(
                cursor_in_container,
                offset,
                new_scale / old_scale,
            ))
        } else {
            None
        };

        let max = self.max_scroll_offset();
        self.set_scroll_offset_within(target.unwrap_or_else(|| self.scroll_handle.offset()), max);

        // Existing bitmaps stay on screen and are scaled to the new page size
        // until their replacements arrive. They are not marked stale: the
        // render in flight (if any) still produces a valid, sharper-than-the-
        // old image, and requesting again while it runs would queue one full
        // batch per gesture event.
        self.want_full = true;
        match render {
            ZoomRender::Now => {
                self.zoom_settling = false;
                self._zoom_task = Task::ready(());
                self.request_render();
            }
            ZoomRender::OnSettle => {
                self.zoom_settling = true;
                self.schedule_zoom_render(cx);
            }
        }
        cx.notify();
    }

    /// Waits for a pinch or wheel-zoom gesture to pause, then requests sharp
    /// pixels for the final scale.
    fn schedule_zoom_render(&mut self, cx: &mut Context<Self>) {
        let generation = self.document_generation;
        self._zoom_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ZOOM_RENDER_DELAY).await;
            this.update(cx, |this, _cx| {
                this.zoom_settling = false;
                // A document update schedules its own settle render at the new
                // content; zooming before it arrives must not override it.
                if this.document_generation == generation {
                    this.request_render();
                }
            })
            .ok();
        });
    }

    fn save_zoom(&self) {
        saved_zooms()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                zoom_key(&self.path),
                SavedZoom {
                    mode: self.zoom_mode,
                    zoom: self.zoom,
                },
            );
    }

    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Ctrl/Cmd + wheel zooms. Every other wheel event belongs to the
        // scroll container, which owns both axes (vertical scrolling and
        // shift+wheel horizontal panning).
        if !is_zoom_gesture(event) {
            return;
        }

        let delta = wheel_line_delta(event);
        if delta == 0.0 {
            return;
        }

        let factor = 1.0 + delta.abs() * SCROLL_ZOOM_SENSITIVITY;
        let scale = self.page_scale();
        let scale = if delta > 0.0 {
            scale * factor
        } else {
            scale / factor
        };
        self.set_zoom(scale, Some(event.position), ZoomRender::OnSettle, cx);
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn handle_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.delta == 0.0 {
            return;
        }

        let factor = 1.0 + event.delta;
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }

        self.set_zoom(
            self.page_scale() * factor,
            Some(event.position),
            ZoomRender::OnSettle,
            cx,
        );
        cx.stop_propagation();
    }

    fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(
            self.page_scale() * ZOOM_STEP,
            self.zoom_anchor(),
            ZoomRender::Now,
            cx,
        );
    }

    fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(
            self.page_scale() / ZOOM_STEP,
            self.zoom_anchor(),
            ZoomRender::Now,
            cx,
        );
    }

    fn reset_zoom(&mut self, _: &ResetZoom, window: &mut Window, cx: &mut Context<Self>) {
        self.fit_width(&FitWidth, window, cx);
    }

    fn fit_width(&mut self, _: &FitWidth, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_zoom(
            ZoomMode::FitWidth,
            1.0,
            self.zoom_anchor(),
            ZoomRender::Now,
            cx,
        );
    }

    fn fit_page(&mut self, _: &FitPage, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_zoom(
            ZoomMode::FitPage,
            1.0,
            self.zoom_anchor(),
            ZoomRender::Now,
            cx,
        );
    }

    fn actual_size(&mut self, _: &ActualSize, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply_zoom(
            ZoomMode::ActualSize,
            1.0,
            self.zoom_anchor(),
            ZoomRender::Now,
            cx,
        );
    }

    fn toggle_outline(&mut self, _: &ToggleOutline, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_outline = !self.show_outline;
        cx.notify();
    }

    fn find_in_preview(&mut self, _: &FindInPreview, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = true;
        window.focus(&self.find_input.focus_handle(cx), cx);
        self.find_input.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
        self.refresh_find_matches(cx);
        if !self.find_matches.is_empty() {
            self.find_index = 0;
            self.show_find_match(cx);
        }
    }

    fn find_next(&mut self, _: &FindNext, _window: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_index = (self.find_index + 1) % self.find_matches.len();
        self.show_find_match(cx);
    }

    fn find_previous(&mut self, _: &FindPrevious, _window: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_index = self
            .find_index
            .checked_sub(1)
            .unwrap_or(self.find_matches.len() - 1);
        self.show_find_match(cx);
    }

    fn close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        self.find_matches.clear();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn confirm_find(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.find_next(&FindNext, window, cx);
    }

    fn cancel_find(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(&CloseFind, window, cx);
    }

    fn refresh_find_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.find_input.read(cx).text(cx);
        self.find_matches.clear();
        if query.is_empty() {
            self.find_index = 0;
            cx.notify();
            return;
        }

        for page in &self.pages {
            let Some(accessibility) = &page.accessibility else {
                continue;
            };
            for (run_index, run) in accessibility.text_runs().iter().enumerate() {
                for (start, end) in find_occurrences(run.text(), &query) {
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
                    if !rects.is_empty() {
                        self.find_matches.push(FindMatch {
                            page: page.index,
                            selection,
                            rects,
                        });
                    }
                }
            }
        }

        self.find_index = self
            .find_index
            .min(self.find_matches.len().saturating_sub(1));
        cx.notify();
    }

    fn show_find_match(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self.find_matches.get(self.find_index).cloned() else {
            cx.notify();
            return;
        };
        self.selection = Some(Selection {
            anchor: TextPosition {
                page: found.page,
                run: found.selection.anchor.run_index,
                character: found.selection.anchor.character_index,
            },
            focus: TextPosition {
                page: found.page,
                run: found.selection.focus.run_index,
                character: found.selection.focus.character_index,
            },
            dragging: false,
        });
        if let Some(rect) = found.rects.first() {
            self.scroll_to_page_point(found.page, rect.x0 as f32, rect.y0 as f32, cx);
        } else {
            cx.notify();
        }
    }

    fn find_highlights_for_page(
        &self,
        page: &PreviewPage,
        scale: f32,
        cx: &App,
    ) -> Vec<AnyElement> {
        if !self.find_open || self.find_matches.is_empty() {
            return Vec::new();
        }
        let match_color = cx.theme().colors().search_match_background;
        let active_color = cx.theme().colors().search_active_match_background;
        let mut highlights = Vec::new();
        for (index, found) in self.find_matches.iter().enumerate() {
            if found.page != page.index {
                continue;
            }
            let color = if index == self.find_index {
                active_color
            } else {
                match_color
            };
            for rect in &found.rects {
                highlights.push(
                    div()
                        .absolute()
                        .left(px(rect.x0 as f32 * scale))
                        .top(px(rect.y0 as f32 * scale))
                        .w(px(rect.width() as f32 * scale))
                        .h(px(rect.height() as f32 * scale))
                        .bg(color)
                        .into_any_element(),
                );
            }
        }
        highlights
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn scroll_to_page(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.pages.is_empty() {
            return;
        }
        let index = index.min(self.pages.len() - 1);
        let scale = self.page_scale();
        let mut top = CANVAS_PADDING;
        for page in self.pages.iter().take(index) {
            top += page.height * scale + PAGE_GAP;
        }
        let offset = self.scroll_handle.offset();
        self.set_scroll_offset_within(point(offset.x, px(-top)), self.max_scroll_offset());
        self.shown_page = None;
        cx.notify();
    }

    fn go_to_page(&mut self, _: &GoToPage, window: &mut Window, cx: &mut Context<Self>) {
        if self.pages.is_empty() {
            return;
        }
        let focus_handle = self.page_input.focus_handle(cx);
        window.focus(&focus_handle, cx);
        self.page_input.update(cx, |editor, cx| {
            editor.select_all(&SelectAll, window, cx);
        });
    }

    fn confirm_page(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.page_count();
        let typed = self.page_input.read(cx).text(cx);
        if let Ok(number) = typed.trim().parse::<usize>()
            && count > 0
        {
            self.scroll_to_page(number.clamp(1, count) - 1, cx);
        }
        self.leave_page_input(window, cx);
    }

    fn cancel_page(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_page_input(window, cx);
    }

    fn leave_page_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shown_page = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn previous_page(&mut self, _: &PreviousPage, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to_page(self.current_page().saturating_sub(1), cx);
    }

    fn next_page(&mut self, _: &NextPage, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to_page(self.current_page() + 1, cx);
    }

    /// Brings a page point into view, deferring until the page layout exists.
    fn scroll_to_page_point(&mut self, page: usize, x: f32, y: f32, cx: &mut Context<Self>) {
        if self.pages.get(page).is_none() || self.scroll_handle.bounds().size.width <= px(0.0) {
            self.pending_scroll = Some((page, x, y));
            return;
        }
        if self.scroll_to_page_point_now(page, x, y) {
            self.shown_page = None;
        }
        cx.notify();
    }

    /// Scrolls the point to a comfortable reading position. Returns whether the
    /// viewport moved.
    ///
    /// Prefer the painted page bounds: layout rounds pages to device pixels, so
    /// recomputing their positions from the model scale drifts by a pixel or
    /// more per page. Hit testing already uses painted bounds; navigation must
    /// use the same geometry or a click and its destination disagree. The model
    /// is only the fallback for a scroll requested before the first paint.
    fn scroll_to_page_point_now(&self, page_index: usize, x: f32, y: f32) -> bool {
        let Some(page) = self.pages.get(page_index) else {
            return false;
        };
        let viewport = self.scroll_handle.bounds();
        let viewport_width: f32 = viewport.size.width.into();
        let viewport_height: f32 = viewport.size.height.into();
        if viewport_width <= 0.0 || viewport_height <= 0.0 || page.width <= 0.0 {
            return false;
        }

        let painted_bounds = self.page_bounds.borrow().get(&page_index).copied();
        let content_point =
            if let Some(bounds) = painted_bounds {
                let painted_scale = f32::from(bounds.size.width) / page.width;
                let current = self.scroll_handle.offset();
                point(
                    px(f32::from(bounds.origin.x) - f32::from(viewport.origin.x)
                        + x * painted_scale
                        - f32::from(current.x)),
                    px(f32::from(bounds.origin.y) - f32::from(viewport.origin.y)
                        + y * painted_scale
                        - f32::from(current.y)),
                )
            } else {
                let scale = self.page_scale();
                let mut top = CANVAS_PADDING;
                for previous in self.pages.iter().take(page_index) {
                    top += previous.height * scale + PAGE_GAP;
                }
                let column_width = f32::from(self.content_size().width).max(viewport_width);
                let page_left = (column_width - page.width * scale) / 2.0;
                point(px(page_left + x * scale), px(top + y * scale))
            };

        let current = self.scroll_handle.offset();
        let max = self.max_scroll_offset();
        let offset = point(
            px(reveal_axis(
                f32::from(current.x),
                f32::from(content_point.x),
                viewport_width,
                REVEAL_MARGIN,
                0.5,
                f32::from(max.x),
            )),
            px(reveal_axis(
                f32::from(current.y),
                f32::from(content_point.y),
                viewport_height,
                REVEAL_MARGIN,
                1.0 / 3.0,
                f32::from(max.y),
            )),
        );
        let moved = offset != current;
        if moved {
            self.set_scroll_offset_within(offset, max);
        }
        moved
    }

    /// Opens a link from a page. Relative local paths resolve against the
    /// previewed document's directory.
    fn follow_link(&mut self, href: &str, window: &mut Window, cx: &mut Context<Self>) {
        match classify_link(href, self.path.parent()) {
            Some(LinkTarget::Document { page, x, y }) => {
                self.scroll_to_page_point(page, x, y, cx);
            }
            Some(LinkTarget::External(url)) => cx.open_url(&url),
            Some(LinkTarget::LocalFile(path)) => self.open_local_file(path, window, cx),
            None => {}
        }
    }

    fn open_local_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            cx.open_url(&format!("file://{}", path.display()));
            return;
        };
        let task = workspace.update(cx, |workspace, cx| {
            workspace.open_abs_path(path, OpenOptions::default(), window, cx)
        });
        task.detach_and_log_err(cx);
    }

    /// Sends the cursor position after a short quiet period, so typing does
    /// not issue an LSP round trip per keystroke.
    fn schedule_source_sync(
        &mut self,
        path: PathBuf,
        line: u32,
        character: u32,
        cx: &mut Context<Self>,
    ) {
        if path != self.path {
            return;
        }
        self.pending_source_sync = Some((path, line, character));
        self._sync_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SYNC_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if let Some((path, line, character)) = this.pending_source_sync.take() {
                    this.scroll_to_source(&path, line, character, cx);
                }
            })
            .ok();
        });
    }

    /// Asks Tinymist to resolve a source position and send back a `viewport`
    /// frame, which scrolls this pane there.
    pub(crate) fn scroll_to_source(&self, path: &Path, line: u32, character: u32, cx: &mut App) {
        let Some(lsp_store) = self.owner.lsp_store.upgrade() else {
            return;
        };
        let Some(server_id) = self.owner.server_id else {
            return;
        };
        let task_id = self.owner.task_id.clone();
        let arguments = vec![
            json!(task_id),
            json!({
                "event": "panelScrollTo",
                "filepath": path.display().to_string(),
                "line": line,
                "character": character,
            }),
        ];
        lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(
                    server_id,
                    "tinymist.scrollPreview".to_owned(),
                    arguments,
                    cx,
                )
            })
            .detach();
    }

    /// Exports the previewed document and opens the resulting PDF.
    ///
    /// Locally the compiler writes the PDF next to the source and the path is
    /// opened directly. In a collaborative project the compiler runs on the
    /// host, so the guest asks for the bytes (`write: false`) and stores them
    /// in a local temp file instead; the host's disk is left untouched.
    fn export_pdf(&mut self, _: &ExportPdf, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let Some(lsp_store) = self.owner.lsp_store.upgrade() else {
            return;
        };
        let Some(server_id) = self.owner.server_id else {
            return;
        };
        let is_collab = self.owner.collab().is_some();
        let task = lsp_store.update(cx, |store, cx| {
            store.execute_lsp_command(
                server_id,
                "tinymist.exportPdf".to_owned(),
                vec![
                    json!(path.display().to_string()),
                    json!({}),
                    json!({ "write": !is_collab }),
                ],
                cx,
            )
        });
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_this, cx| {
            let output = match task.await {
                Ok(Some(response)) if is_collab => {
                    match response.get("data").and_then(|data| data.as_str()) {
                        Some(data) => {
                            let data = data.to_owned();
                            let stem = path
                                .file_stem()
                                .map(|stem| stem.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "preview".to_owned());
                            cx.background_spawn(async move { write_temp_pdf(&data, &stem) })
                                .await
                        }
                        None => None,
                    }
                }
                Ok(Some(response)) => export_output_path(&response),
                Ok(None) => None,
                Err(error) => {
                    log::warn!("tinymist.exportPdf failed: {error:#}");
                    None
                }
            };
            workspace
                .update_in(cx, |workspace, window, cx| {
                    if let Some(output) = output {
                        let open =
                            workspace.open_abs_path(output, OpenOptions::default(), window, cx);
                        open.detach_and_log_err(cx);
                    } else {
                        workspace.show_toast(
                            Toast::new(
                                NotificationId::unique::<TypstPreviewToast>(),
                                "Tinymist could not export this document as a PDF.".to_string(),
                            ),
                            cx,
                        );
                    }
                })
                .ok();
        })
        .detach();
    }

    fn scroll_line_down(
        &mut self,
        _: &ScrollLineDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_by(px(0.0), px(-SCROLL_LINE_PX), cx);
    }

    fn scroll_line_up(&mut self, _: &ScrollLineUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(0.0), px(SCROLL_LINE_PX), cx);
    }

    fn scroll_page_down(
        &mut self,
        _: &ScrollPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_page(1.0, cx);
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_page(-1.0, cx);
    }

    fn scroll_to_top(&mut self, _: &ScrollToTop, _window: &mut Window, cx: &mut Context<Self>) {
        let offset = self.scroll_handle.offset();
        self.set_scroll_offset(point(offset.x, px(0.0)));
        cx.notify();
    }

    fn scroll_to_bottom(
        &mut self,
        _: &ScrollToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.scroll_handle.offset();
        self.set_scroll_offset(point(offset.x, -self.scroll_handle.max_offset().y));
        cx.notify();
    }

    fn scroll_left(&mut self, _: &ScrollLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(SCROLL_LINE_PX), px(0.0), cx);
    }

    fn scroll_right(&mut self, _: &ScrollRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(-SCROLL_LINE_PX), px(0.0), cx);
    }

    fn status_text(&self) -> SharedString {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if let Some((_, href)) = &self.hovered_link {
            return match classify_link(href, self.path.parent()) {
                Some(LinkTarget::Document { page, .. }) => {
                    format!("Internal link: page {}", page + 1).into()
                }
                Some(LinkTarget::LocalFile(path)) => format!("Open {}", path.display()).into(),
                Some(LinkTarget::External(url)) => format!("Open {url}").into(),
                None => "Link".into(),
            };
        }
        if !self.connected {
            return "Connecting to Tinymist…".into();
        }
        let zoom = (self.page_scale() * 100.0).round() as u32;
        let mut status = format!("{} page(s)  ·  {zoom}%", self.pages.len());
        if let Some((errors, warnings)) = self.diagnostics {
            if errors > 0 {
                status.push_str(&format!("  ·  {errors} error(s)"));
            }
            if warnings > 0 {
                status.push_str(&format!("  ·  {warnings} warning(s)"));
            }
        }
        status.into()
    }
}

/// Extracts the written file from Tinymist's `OnExportResponse`
fn export_output_path(response: &serde_json::Value) -> Option<PathBuf> {
    response
        .get("path")
        .and_then(|path| path.as_str())
        .or_else(|| response.get("items")?.get(0)?.get("path")?.as_str())
        .map(PathBuf::from)
}

/// Decodes a PDF returned by `tinymist.exportPdf` and writes it to a temp file.
///
/// The file stays on the guest's disk so its PDF viewer can open it. The name
/// carries the source stem and a unique counter to make repeated exports safe.
fn write_temp_pdf(data: &str, stem: &str) -> Option<PathBuf> {
    use base64::Engine as _;

    // Base64 is 4/3 the decoded size. Cap before decoding so a runaway export
    // cannot allocate the response twice.
    const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;
    if data.len() > MAX_PDF_BYTES.saturating_mul(2) {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    if bytes.len() > MAX_PDF_BYTES {
        return None;
    }
    static NEXT_PDF: AtomicU64 = AtomicU64::new(1);
    let name = format!(
        "suzuri-{}-{}-{}.pdf",
        stem.replace(['/', '\\'], "-"),
        std::process::id(),
        NEXT_PDF.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

struct TypstPreviewToast;

fn diagnostic_counts(project: &Entity<Project>, path: &Path, cx: &App) -> Option<(usize, usize)> {
    let project = project.read(cx);
    let project_path = project.project_path_for_absolute_path(path, cx)?;
    let summary = project.diagnostic_summary_for_path(&project_path, cx);
    Some((summary.error_count, summary.warning_count))
}

/// Whether a character belongs to a word for double-click selection.
fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Case-insensitive (ASCII) occurrences of `query` in `text`, as char
/// ranges.
fn find_occurrences(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let haystack: Vec<char> = text.chars().collect();
    let needle: Vec<char> = query
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect();
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

/// The character range selected by a double- or triple-click.
///
/// Double-click expands to the word around `index`, or a single character when
/// the pointer is over punctuation or whitespace. Triple-click selects the
/// whole run. Returns `None` for an empty run.
fn word_selection(characters: &[char], index: usize, whole_run: bool) -> Option<(usize, usize)> {
    if characters.is_empty() {
        return None;
    }
    if whole_run {
        return Some((0, characters.len()));
    }

    let mut start = index.min(characters.len() - 1);
    // A double-click just past a word still selects that word.
    if !is_word_character(characters[start])
        && start > 0
        && is_word_character(characters[start - 1])
    {
        start -= 1;
    }

    if !is_word_character(characters[start]) {
        return Some((start, start + 1));
    }

    let mut end = start;
    while start > 0 && is_word_character(characters[start - 1]) {
        start -= 1;
    }
    while end < characters.len() && is_word_character(characters[end]) {
        end += 1;
    }
    Some((start, end))
}

/// A short accessibility summary of a page's text runs.
///
/// Built when the page is rasterized, not while it is drawn: `render` runs for
/// every page on every frame, so summarising the runs there made the per-frame
/// cost grow with the document.
fn page_label(index: usize, accessibility: Option<&PageAccessibility>) -> SharedString {
    // Enough text for a screen reader to identify the page without copying
    // the whole thing into an attribute.
    const LIMIT: usize = 2000;

    let mut text = String::new();
    if let Some(accessibility) = accessibility {
        for run in accessibility.text_runs() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(run.text());
            if text.len() > LIMIT {
                break;
            }
        }
    }

    if text.is_empty() {
        format!("Page {}", index + 1).into()
    } else {
        format!("Page {}: {text}", index + 1).into()
    }
}

/// Joins the selected text of every page touched by a document selection.
///
/// Pages are addressed through `accessibility_at` rather than a slice so that
/// pages still loading (or without text) do not shift the indices of later ones.
fn selection_text<'a>(
    start: TextPosition,
    end: TextPosition,
    accessibility_at: impl Fn(usize) -> Option<&'a PageAccessibility>,
) -> String {
    let mut text = String::new();
    for page_index in start.page..=end.page {
        let Some(accessibility) = accessibility_at(page_index) else {
            continue;
        };
        let Some(selection) = page_selection(page_index, accessibility, start, end) else {
            continue;
        };

        let page_text = accessibility.selected_text(selection);
        if page_text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&page_text);
    }
    text
}

/// The part of a document selection that falls on one page.
///
/// `PageAccessibility::selection_rects` understands a contiguous run range, so
/// intermediate pages select everything from the first run to the last, while
/// the first and last pages select from/to the exact positions.
fn page_selection(
    page_index: usize,
    accessibility: &PageAccessibility,
    start: TextPosition,
    end: TextPosition,
) -> Option<PageTextSelection> {
    let runs = accessibility.text_runs();
    if runs.is_empty() || page_index < start.page || page_index > end.page {
        return None;
    }

    let clamp = |position: TextPosition| {
        let run_index = position.run.min(runs.len() - 1);
        let character_index = position.character.min(runs[run_index].character_count());
        PageTextPosition {
            run_index,
            character_index,
        }
    };

    let anchor = if page_index == start.page {
        clamp(start)
    } else {
        PageTextPosition {
            run_index: 0,
            character_index: 0,
        }
    };
    let focus = if page_index == end.page {
        clamp(end)
    } else {
        let run_index = runs.len() - 1;
        PageTextPosition {
            run_index,
            character_index: runs[run_index].character_count(),
        }
    };

    (anchor != focus).then_some(PageTextSelection { anchor, focus })
}

impl Render for TypstPreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut spans = FrameSpans::start();
        self.display_scale = window.scale_factor();
        // Repainted pages record their new bounds below; stale entries from a
        // previous layout would make hit testing point at the wrong page.
        self.page_bounds.borrow_mut().clear();
        self.request_render();

        if let Some((page, x, y)) = self.pending_scroll
            && !self.pages.is_empty()
            && self.scroll_handle.bounds().size.width > px(0.0)
        {
            self.pending_scroll = None;
            self.scroll_to_page_point_now(page, x, y);
        }

        let page_count = self.page_count();
        if page_count > 0 {
            let current_page = self.current_page();
            let typed = self.page_input.read(cx).text(cx);
            let untouched = self
                .shown_page
                .is_some_and(|shown| typed == (shown + 1).to_string());
            if self.shown_page.is_none() || (self.shown_page != Some(current_page) && untouched) {
                self.shown_page = Some(current_page);
                // Deferred past this frame: replacing an editor's text while
                // the window draws laid out an empty line, so the box showed a
                // blank number after the first page.
                cx.defer_in(window, move |this, window, cx| {
                    let focused = this.page_input.focus_handle(cx).is_focused(window);
                    this.page_input.update(cx, |editor, cx| {
                        editor.set_text((current_page + 1).to_string(), window, cx);
                        if focused {
                            editor.select_all(&SelectAll, window, cx);
                        }
                    });
                });
            }
        }

        let scale = self.page_scale();
        // Lay the column out at its real width. Without this it would be
        // stretched to the viewport and clip the pages that are wider than it
        // when zoomed, which leaves the scroll container with no horizontal
        // range: no shift+wheel panning and no horizontal scrollbar.
        let viewport_width: f32 = self.scroll_handle.bounds().size.width.into();
        let content_width: f32 = self.content_size().width.into();
        let mut pages_column = v_flex()
            .w(px(content_width.max(viewport_width)))
            .gap(px(PAGE_GAP))
            .items_center()
            .py(px(CANVAS_PADDING));
        spans.pages_started();
        for page in &self.pages {
            pages_column = pages_column.child(self.render_page_element(page, scale, cx));
        }
        spans.pages_done();

        let status = self.status_text();
        let status_color =
            if self.error.is_some() || self.diagnostics.is_some_and(|(errors, _)| errors > 0) {
                Color::Error
            } else {
                Color::Muted
            };
        spans.selection_started();
        let selection_words = self.selection_word_count();
        spans.selection_done();
        let word_label = match (self.word_count, selection_words) {
            (Some(total), Some(selected)) => {
                format!("{} selected  ·  {}", selected.words, total.label())
            }
            (Some(total), None) => total.label(),
            (None, Some(selected)) => format!("{} selected", selected.words),
            (None, None) => String::new(),
        };
        let zoom_percent = (self.page_scale() * 100.0).round() as u32;
        let page_digits = page_count.max(1).to_string().len() as f32;
        let page_box_width = px(page_digits * 9.0 + 30.0);

        let element = v_flex()
            .id("TypstLivePreview")
            .key_context("TypstLivePreview")
            .track_focus(&self.focus_handle(cx))
            .relative()
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::reset_zoom))
            .on_action(cx.listener(Self::fit_width))
            .on_action(cx.listener(Self::fit_page))
            .on_action(cx.listener(Self::actual_size))
            .on_action(cx.listener(Self::go_to_page))
            .on_action(cx.listener(Self::previous_page))
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::export_pdf))
            .on_action(cx.listener(Self::toggle_outline))
            .on_action(cx.listener(Self::find_in_preview))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(Self::scroll_line_down))
            .on_action(cx.listener(Self::scroll_line_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_to_top))
            .on_action(cx.listener(Self::scroll_to_bottom))
            .on_action(cx.listener(Self::scroll_left))
            .on_action(cx.listener(Self::scroll_right))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .when(page_count == 0, |row| {
                        row.child(Label::new("Loading…").size(LabelSize::Small))
                    })
                    .when(page_count > 0, |row| {
                        row.child(
                            IconButton::new("typst-prev-page", IconName::ArrowLeft)
                                .icon_size(IconSize::Small)
                                .tooltip(|_window, cx| {
                                    Tooltip::for_action("Previous Page", &PreviousPage, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.previous_page(&PreviousPage, window, cx)
                                })),
                        )
                        .child(
                            div()
                                .id("typst-page-input")
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
                            Label::new(format!("of {page_count}"))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            IconButton::new("typst-next-page", IconName::ArrowRight)
                                .icon_size(IconSize::Small)
                                .tooltip(|_window, cx| {
                                    Tooltip::for_action("Next Page", &NextPage, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.next_page(&NextPage, window, cx)
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        IconButton::new("typst-zoom-out", IconName::Dash)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| Tooltip::for_action("Zoom Out", &ZoomOut, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_out(&ZoomOut, window, cx)
                            })),
                    )
                    .child(
                        Button::new("typst-zoom-level", format!("{zoom_percent}%"))
                            .label_size(LabelSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Reset Zoom", &ResetZoom, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_zoom(&ResetZoom, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("typst-zoom-in", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| Tooltip::for_action("Zoom In", &ZoomIn, cx))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.zoom_in(&ZoomIn, window, cx)
                                }),
                            ),
                    )
                    .child(
                        IconButton::new("typst-fit-width", IconName::Maximize)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| Tooltip::for_action("Fit Width", &FitWidth, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.fit_width(&FitWidth, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("typst-fit-page", IconName::MaximizeAlt)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| Tooltip::for_action("Fit Page", &FitPage, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.fit_page(&FitPage, window, cx)
                            })),
                    )
                    .child(
                        Button::new("typst-actual-size", "100%")
                            .label_size(LabelSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Actual Size", &ActualSize, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.actual_size(&ActualSize, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("typst-outline", IconName::ListTree)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Document Outline", &ToggleOutline, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_outline(&ToggleOutline, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("typst-find", IconName::MagnifyingGlass)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Find in Preview", &FindInPreview, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.find_in_preview(&FindInPreview, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("typst-export-pdf", IconName::Download)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Export as PDF", &ExportPdf, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.export_pdf(&ExportPdf, window, cx)
                            })),
                    ),
            )
            .when(self.find_open, |root| {
                let count = self.find_matches.len();
                let position = if count == 0 { 0 } else { self.find_index + 1 };
                root.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_1()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            div()
                                .id("typst-find-input")
                                .on_action(cx.listener(Self::confirm_find))
                                .on_action(cx.listener(Self::cancel_find))
                                .w(px(180.0))
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .bg(cx.theme().colors().editor_background)
                                .text_ui_sm(cx)
                                .child(self.find_input.clone()),
                        )
                        .child(
                            Label::new(format!("{position}/{count}"))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            IconButton::new("typst-find-previous", IconName::ArrowLeft)
                                .icon_size(IconSize::Small)
                                .tooltip(|_window, cx| {
                                    Tooltip::for_action("Previous Match", &FindPrevious, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.find_previous(&FindPrevious, window, cx)
                                })),
                        )
                        .child(
                            IconButton::new("typst-find-next", IconName::ArrowRight)
                                .icon_size(IconSize::Small)
                                .tooltip(|_window, cx| {
                                    Tooltip::for_action("Next Match", &FindNext, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.find_next(&FindNext, window, cx)
                                })),
                        )
                        .child(
                            IconButton::new("typst-find-close", IconName::Close)
                                .icon_size(IconSize::Small)
                                .tooltip(|_window, cx| {
                                    Tooltip::for_action("Close Find", &CloseFind, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_find(&CloseFind, window, cx)
                                })),
                        )
                        .child(div().flex_1()),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("typst-live-preview-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .track_scroll(&self.scroll_handle)
                            .on_scroll(cx.listener(Self::handle_scroll_wheel))
                            .on_pinch(cx.listener(Self::handle_pinch))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
                            .on_mouse_move(cx.listener(Self::handle_mouse_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
                            .cursor(if self.hovered_link.is_some() {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::IBeam
                            })
                            .child(pages_column),
                    )
                    // The bars hang off a wrapper, not the scroll container: a
                    // child of the container is painted at the scroll offset,
                    // so the thumb would scroll away with the pages.
                    .custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Both)
                            .tracked_scroll_handle(&self.scroll_handle),
                        window,
                        cx,
                    ),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Label::new(status)
                            .size(LabelSize::Small)
                            .color(status_color),
                    )
                    .child(div().flex_1())
                    .when(!word_label.is_empty(), |row| {
                        row.child(
                            Label::new(word_label)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .when(self.show_outline, |root| {
                let mut panel = v_flex()
                    .id("typst-outline")
                    .absolute()
                    .top(px(44.0))
                    .left(px(8.0))
                    .w(px(300.0))
                    .max_h(px(420.0))
                    .overflow_y_scroll()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().elevated_surface_background)
                    .shadow_lg()
                    .p_1()
                    .gap_0p5();
                for (index, entry) in self.outline.iter().enumerate() {
                    let page = entry.page;
                    let (x, y) = (entry.x, entry.y);
                    panel = panel.child(
                        div()
                            .id(("typst-outline-item", index))
                            .pl(px(entry.depth as f32 * 12.0))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor(CursorStyle::PointingHand)
                            .hover(|style| style.bg(cx.theme().colors().element_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.show_outline = false;
                                if let Some(page) = page {
                                    this.scroll_to_page_point(page, x, y, cx);
                                } else {
                                    cx.notify();
                                }
                            }))
                            .child(Label::new(entry.title.clone()).size(LabelSize::Small)),
                    );
                }
                if self.outline.is_empty() {
                    panel = panel.child(
                        Label::new("No headings")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    );
                }
                root.child(panel)
            });

        if let Some(timing) = &mut self.frame_timing {
            timing.record(&spans, self.pages.len());
        }

        element
    }
}

impl Focusable for TypstPreviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TypstPreviewEvent> for TypstPreviewView {}

impl Item for TypstPreviewView {
    type Event = TypstPreviewEvent;

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let name = self
            .source_buffer
            .as_ref()
            .and_then(|buffer| buffer.upgrade())
            .and_then(|buffer| {
                buffer
                    .read(cx)
                    .file()
                    .map(|file| file.file_name(cx).to_owned())
            })
            .or_else(|| {
                self.path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Typst".to_owned());
        format!("Preview {name}").into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::File))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("typst live preview: open")
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            TypstPreviewEvent::TitleChanged => f(ItemEvent::UpdateTab),
        }
    }

    fn on_removed(&self, cx: &mut Context<Self>) {
        for page in &self.pages {
            if let Some(image) = &page.image {
                cx.drop_image(image.image.clone(), None);
            }
        }
        self.owner.kill(cx);
    }
}

/// The scroll offset that keeps the content point under `cursor` fixed when
/// the page scale changes by `ratio`.
fn anchored_scroll_offset(
    cursor_in_container: Point<Pixels>,
    offset: Point<Pixels>,
    ratio: f32,
) -> Point<Pixels> {
    let content_x = cursor_in_container.x - offset.x;
    let content_y = cursor_in_container.y - offset.y;
    point(
        cursor_in_container.x - content_x * ratio,
        cursor_in_container.y - content_y * ratio,
    )
}

/// The laid-out size of the page column at a given zoom.
///
/// Kept in sync with [`TypstPreviewView::render`]: a centered column of pages
/// separated by [`PAGE_GAP`] inside [`CANVAS_PADDING`].
fn content_extent(pages: impl IntoIterator<Item = (f32, f32)>, scale: f32) -> (f32, f32) {
    let mut width = 0.0_f32;
    let mut height = 0.0_f32;
    let mut count = 0;
    for (page_width, page_height) in pages {
        width = width.max(page_width * scale);
        height += page_height * scale;
        count += 1;
    }
    if count > 1 {
        height += PAGE_GAP * (count - 1) as f32;
    }
    (width + 2.0 * CANVAS_PADDING, height + 2.0 * CANVAS_PADDING)
}

/// The scroll offset that reveals one axis of a target.
///
/// A target already inside the viewport (with [`REVEAL_MARGIN`]) keeps the
/// current offset. Otherwise it is placed at `aligned_fraction` of the
/// viewport: centered horizontally, but higher than center vertically so the
/// reader still sees the context above the destination.
fn reveal_axis(
    current: f32,
    content: f32,
    viewport: f32,
    margin: f32,
    aligned_fraction: f32,
    max: f32,
) -> f32 {
    let local = content + current;
    if local >= margin && local <= viewport - margin {
        return current;
    }
    let desired = -(content - viewport * aligned_fraction);
    desired.clamp(-max, 0.0)
}

/// Whether a wheel event is a zoom gesture.
///
/// Only ctrl (or the platform modifier on macOS) zooms; everything else must
/// reach the scroll container. Locking this behind one predicate keeps the
/// fallback path from accidentally re-running the zoom logic.
fn is_zoom_gesture(event: &ScrollEvent) -> bool {
    event.modifiers.control || event.modifiers.platform
}

/// The vertical wheel delta in display pixels.
fn wheel_line_delta(event: &ScrollEvent) -> f32 {
    match event.delta {
        ScrollDelta::Pixels(pixels) => pixels.y.into(),
        ScrollDelta::Lines(lines) => lines.y * SCROLL_LINE_MULTIPLIER,
    }
}

/// What a completed click should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClickAction {
    /// The press and the release landed on the same link: open it.
    FollowLink,
    /// Anything else: ask Tinymist where the clicked page point came from.
    JumpToSource,
}

fn click_action(pressed: Option<(usize, &str)>, released: Option<(usize, &str)>) -> ClickAction {
    match (pressed, released) {
        (Some((pressed_page, pressed_href)), Some((released_page, released_href)))
            if pressed_page == released_page && pressed_href == released_href =>
        {
            ClickAction::FollowLink
        }
        _ => ClickAction::JumpToSource,
    }
}

/// Whether `path` is a Typst document.
pub fn is_typst_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("typ"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PageTextRun;

    fn position(page: usize, run: usize, character: usize) -> TextPosition {
        TextPosition {
            page,
            run,
            character,
        }
    }

    #[test]
    fn zoom_keeps_the_content_under_the_cursor_fixed() {
        let offset = point(px(-120.0), px(-340.0));
        let cursor = point(px(300.0), px(200.0));
        let ratio = 1.5;

        let target = anchored_scroll_offset(cursor, offset, ratio);

        let content_before = (
            f32::from(cursor.x - offset.x),
            f32::from(cursor.y - offset.y),
        );
        let content_after = (
            f32::from(cursor.x - target.x),
            f32::from(cursor.y - target.y),
        );
        assert!((content_after.0 - content_before.0 * ratio).abs() < 0.001);
        assert!((content_after.1 - content_before.1 * ratio).abs() < 0.001);
    }

    #[test]
    fn find_occurrences_matches_case_insensitively() {
        assert_eq!(
            find_occurrences("Typst typst TYPST", "typst"),
            vec![(0, 5), (6, 11), (12, 17)]
        );
        assert_eq!(find_occurrences("aaaa", "aa"), vec![(0, 2), (1, 3), (2, 4)]);
        assert!(find_occurrences("text", "missing").is_empty());
        assert!(find_occurrences("text", "").is_empty());
    }

    #[test]
    fn outline_items_flatten_with_depth_and_positions() {
        let outline = serde_json::json!({
            "items": [
                {
                    "title": "Chapter",
                    "position": { "page_no": 1, "x": 10.0, "y": 20.0 },
                    "children": [
                        {
                            "title": "Section",
                            "position": { "page_no": 3, "x": 0.0, "y": 5.0 },
                            "children": []
                        }
                    ]
                },
                { "title": "Appendix", "position": null, "children": [] }
            ]
        });

        let entries = parse_outline(&outline);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].title, "Chapter");
        assert_eq!(entries[0].depth, 0);
        assert_eq!(entries[0].page, Some(0));
        assert_eq!((entries[0].x, entries[0].y), (10.0, 20.0));
        assert_eq!(entries[1].title, "Section");
        assert_eq!(entries[1].depth, 1);
        assert_eq!(entries[1].page, Some(2));
        assert_eq!(entries[2].title, "Appendix");
        assert_eq!(entries[2].page, None);
    }

    fn page(texts: &[&str]) -> PageAccessibility {
        let mut accessibility = PageAccessibility::default();
        for (index, text) in texts.iter().enumerate() {
            let y = index as f64 * 20.0;
            let width = 10.0 * text.chars().count() as f64;
            accessibility.push_text_run(PageTextRun::new(
                *text,
                kurbo::Rect::new(0.0, y, width, y + 20.0),
                [],
            ));
        }
        accessibility
    }

    #[test]
    fn word_selection_expands_to_word_boundaries() {
        let characters = "hello, world".chars().collect::<Vec<_>>();
        assert_eq!(word_selection(&characters, 1, false), Some((0, 5)));
        assert_eq!(word_selection(&characters, 9, false), Some((7, 12)));
    }

    #[test]
    fn word_selection_after_a_word_selects_that_word() {
        let characters = "hello, world".chars().collect::<Vec<_>>();
        // `,` at index 5 is punctuation; a double click there still hits `hello`.
        assert_eq!(word_selection(&characters, 5, false), Some((0, 5)));
        // Whitespace between words has no word to attach to.
        assert_eq!(word_selection(&characters, 6, false), Some((6, 7)));
    }

    #[test]
    fn word_selection_selects_the_whole_run_on_triple_click() {
        let characters = "hello, world".chars().collect::<Vec<_>>();
        assert_eq!(word_selection(&characters, 4, true), Some((0, 12)));
    }

    #[test]
    fn word_selection_handles_empty_and_out_of_range_runs() {
        assert_eq!(word_selection(&[], 0, false), None);
        assert_eq!(word_selection(&[], 0, true), None);

        let characters = "hi".chars().collect::<Vec<_>>();
        assert_eq!(word_selection(&characters, 99, false), Some((0, 2)));
    }

    #[test]
    fn page_selection_uses_exact_positions_on_first_and_last_page() {
        let accessibility = page(&["Hello", "World"]);
        let selection = page_selection(0, &accessibility, position(0, 0, 2), position(0, 1, 3))
            .expect("selection on page zero");
        assert_eq!(
            selection.anchor,
            PageTextPosition {
                run_index: 0,
                character_index: 2,
            }
        );
        assert_eq!(
            selection.focus,
            PageTextPosition {
                run_index: 1,
                character_index: 3,
            }
        );
    }

    #[test]
    fn page_selection_covers_whole_intermediate_pages() {
        let accessibility = page(&["Middle", "Page"]);
        let selection = page_selection(1, &accessibility, position(0, 1, 4), position(2, 0, 2))
            .expect("selection on the middle page");
        assert_eq!(
            selection.anchor,
            PageTextPosition {
                run_index: 0,
                character_index: 0,
            }
        );
        assert_eq!(
            selection.focus,
            PageTextPosition {
                run_index: 1,
                character_index: 4,
            }
        );
    }

    #[test]
    fn page_selection_ignores_pages_outside_the_span() {
        let accessibility = page(&["Hello"]);
        assert!(page_selection(3, &accessibility, position(0, 0, 1), position(1, 0, 1)).is_none());
        assert!(page_selection(0, &accessibility, position(1, 0, 1), position(2, 0, 1)).is_none());
    }

    #[test]
    fn page_selection_clamps_to_existing_text() {
        let accessibility = page(&["Hello"]);
        let selection = page_selection(0, &accessibility, position(0, 7, 40), position(0, 8, 2))
            .expect("clamped selection");
        assert_eq!(
            selection.anchor,
            PageTextPosition {
                run_index: 0,
                character_index: 5,
            }
        );
        assert_eq!(
            selection.focus,
            PageTextPosition {
                run_index: 0,
                character_index: 2,
            }
        );
    }

    #[test]
    fn page_selection_is_empty_for_a_collapsed_range() {
        let accessibility = page(&["Hello"]);
        assert!(page_selection(0, &accessibility, position(0, 0, 2), position(0, 0, 2)).is_none());
        assert!(
            page_selection(
                0,
                &PageAccessibility::default(),
                position(0, 0, 0),
                position(0, 0, 1)
            )
            .is_none()
        );
    }

    #[test]
    fn selection_text_joins_pages_with_newlines() {
        let pages = [
            page(&["Hello"]),
            page(&["Middle", "Page"]),
            page(&["World"]),
        ];
        let text = selection_text(position(0, 0, 3), position(2, 0, 2), |index| {
            pages.get(index)
        });
        assert_eq!(text, "lo\nMiddlePage\nWo");
    }

    #[test]
    fn selection_text_keeps_indices_aligned_across_missing_pages() {
        let pages = [Some(page(&["Hello"])), None, Some(page(&["World"]))];
        let text = selection_text(position(0, 0, 2), position(2, 0, 3), |index| {
            pages.get(index).and_then(|page| page.as_ref())
        });
        assert_eq!(text, "llo\nWor");
    }

    #[test]
    fn selection_text_is_empty_without_pages() {
        let pages: [PageAccessibility; 0] = [];
        let text = selection_text(position(0, 0, 1), position(0, 0, 3), |index| {
            pages.get(index)
        });
        assert_eq!(text, "");
    }

    #[test]
    fn content_extent_matches_the_page_column_layout() {
        let pages = [(100.0, 200.0), (50.0, 300.0)];
        let (width, height) = content_extent(pages, 1.0);

        // Widest page at 1:1, plus the canvas padding.
        assert_eq!(width, 100.0 + 2.0 * CANVAS_PADDING);
        // Both pages at 1:1, one gap between them, padding above and below.
        assert_eq!(height, 500.0 + PAGE_GAP + 2.0 * CANVAS_PADDING);
    }

    #[test]
    fn content_extent_has_no_gap_for_a_single_page() {
        let (_, height) = content_extent([(100.0, 200.0)], 1.5);
        assert_eq!(height, 300.0 + 2.0 * CANVAS_PADDING);
    }

    #[test]
    fn content_extent_is_just_padding_without_pages() {
        let (width, height) = content_extent([], 1.0);
        assert_eq!(width, 2.0 * CANVAS_PADDING);
        assert_eq!(height, 2.0 * CANVAS_PADDING);
    }

    #[test]
    fn reveal_keeps_a_visible_target_still() {
        // 400 px into a 600 px viewport at the top: already comfortable.
        assert_eq!(reveal_axis(0.0, 400.0, 600.0, 24.0, 1.0 / 3.0, 1000.0), 0.0);
        // The same target while scrolled down by 350 px is still visible.
        assert_eq!(
            reveal_axis(-350.0, 400.0, 600.0, 24.0, 1.0 / 3.0, 1000.0),
            -350.0
        );
    }

    #[test]
    fn reveal_positions_an_offscreen_target() {
        // A target below the viewport moves to one third from the top.
        assert_eq!(
            reveal_axis(0.0, 900.0, 600.0, 24.0, 1.0 / 3.0, 2000.0),
            -700.0
        );
        // A target above the viewport scrolls up, but not past the top.
        assert_eq!(
            reveal_axis(-500.0, 100.0, 600.0, 24.0, 1.0 / 3.0, 2000.0),
            0.0
        );
    }

    #[test]
    fn reveal_only_moves_axes_that_need_it() {
        // Vertically visible, horizontally off to the right.
        assert_eq!(reveal_axis(-100.0, 900.0, 400.0, 24.0, 0.5, 2000.0), -700.0);
        // Horizontally visible, vertically below.
        assert_eq!(
            reveal_axis(-100.0, 800.0, 600.0, 24.0, 1.0 / 3.0, 2000.0),
            -600.0
        );
    }

    #[test]
    fn reveal_clamps_to_the_scrollable_range() {
        assert_eq!(
            reveal_axis(0.0, 2000.0, 600.0, 24.0, 1.0 / 3.0, 500.0),
            -500.0
        );
        assert_eq!(
            reveal_axis(-500.0, 10.0, 600.0, 24.0, 1.0 / 3.0, 500.0),
            0.0
        );
    }

    #[test]
    fn a_click_follows_a_link_only_when_press_and_release_match() {
        assert_eq!(
            click_action(
                Some((0, "https://typst.app")),
                Some((0, "https://typst.app"))
            ),
            ClickAction::FollowLink
        );
        // Released somewhere else on the same page.
        assert_eq!(
            click_action(Some((0, "https://typst.app")), None),
            ClickAction::JumpToSource
        );
        // Released on a different link.
        assert_eq!(
            click_action(Some((0, "https://typst.app")), Some((0, "https://zed.dev"))),
            ClickAction::JumpToSource
        );
        // Plain text.
        assert_eq!(click_action(None, None), ClickAction::JumpToSource);
    }

    #[test]
    fn only_ctrl_or_platform_wheel_zooms() {
        let wheel = |modifiers| ScrollEvent {
            delta: ScrollDelta::Lines(point(0.0, 1.0)),
            modifiers,
            ..Default::default()
        };

        // A plain wheel scrolls the document, and shift+wheel pans it.
        assert!(!is_zoom_gesture(&wheel(gpui::Modifiers::none())));
        assert!(!is_zoom_gesture(&wheel(gpui::Modifiers::shift())));
        assert!(is_zoom_gesture(&wheel(gpui::Modifiers::control())));
        assert!(is_zoom_gesture(&wheel(gpui::Modifiers::command())));
    }

    #[test]
    fn wheel_delta_uses_pixels_and_scales_lines() {
        let event = ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(0.0), px(-24.0))),
            ..Default::default()
        };
        assert_eq!(wheel_line_delta(&event), -24.0);

        let event = ScrollEvent {
            delta: ScrollDelta::Lines(point(0.0, 2.0)),
            ..Default::default()
        };
        assert_eq!(wheel_line_delta(&event), 2.0 * SCROLL_LINE_MULTIPLIER);
    }

    #[test]
    fn needs_render_upgrades_but_never_downgrades_resolution() {
        let content = Fingerprint::from_pair(1, 2);

        // No image yet, or an image from older content, always renders.
        assert!(needs_render_for(None, None, content, 1.0));
        assert!(needs_render_for(
            Some(Fingerprint::from_pair(0, 0)),
            Some(2.0),
            content,
            2.0
        ));

        // A draft image is upgraded to full resolution...
        assert!(needs_render_for(Some(content), Some(1.0), content, 2.0));
        // ...but a full image is kept when draft quality is enough again.
        assert!(!needs_render_for(Some(content), Some(2.0), content, 1.0));
        assert!(!needs_render_for(Some(content), Some(2.0), content, 2.0));
        // Sub-pixel differences are not worth a render.
        assert!(!needs_render_for(Some(content), Some(1.98), content, 2.0));
    }

    #[test]
    fn render_order_puts_the_viewport_before_its_buffer() {
        // Every page is stale, and the viewport shows pages 5 and 6. Page 6 is
        // in flight already, so it is not requested again.
        let order = order_render_pages(5..7, 4, 8, |index| index != 6);
        assert_eq!(order, vec![5, 4, 7]);
    }

    #[test]
    fn render_order_keeps_visible_pages_in_page_order() {
        let order = order_render_pages(5..8, 3, 9, |_| true);
        assert_eq!(order, vec![5, 6, 7, 4, 8, 3]);
    }

    #[test]
    fn page_render_scale_stays_inside_vello_bins() {
        fn bins(width: f32, height: f32, scale: f64) -> u32 {
            let width = (f64::from(width) * scale / VELLO_BIN_SIZE).ceil();
            let height = (f64::from(height) * scale / VELLO_BIN_SIZE).ceil();
            (width * height) as u32
        }

        // A4 at the cap: well under 256 bins, and the crash scale from the
        // desktop-freeze log (5.75x) is clamped before it is requested.
        let a4 = page_render_scale_cap(595.0, 842.0);
        assert!(a4 < 5.75);
        assert!(bins(595.0, 842.0, a4) <= 256);

        // A square page is limited by the pixel budget, not the dimension cap.
        let square = page_render_scale_cap(2000.0, 2000.0);
        assert!(bins(2000.0, 2000.0, square) <= 256);

        // A panorama is limited by the dimension cap.
        let panorama = page_render_scale_cap(20_000.0, 200.0);
        assert!(20000.0 * panorama <= MAX_TILE_DIM);
        assert!(bins(20_000.0, 200.0, panorama) <= 256);

        // A pathological page produces a cap below the minimum zoom scale.
        // The caller must take the cap rather than panic in `clamp`.
        let absurd = page_render_scale_cap(1.0e9, 1.0e9);
        assert!(absurd > 0.0 && absurd < 0.1);
        assert_eq!(clamp_render_scale(6.0, absurd), absurd);
        assert_eq!(clamp_render_scale(f64::NAN, 2.0), 1.0);
    }

    #[test]
    fn a_failed_render_is_not_retried_until_the_content_changes() {
        let content = Fingerprint::from_pair(1, 2);
        let page = PreviewPage {
            index: 0,
            width: 100.0,
            height: 100.0,
            content_hash: content,
            image: None,
            image_hash: None,
            accessibility: None,
            accessibility_label: "Page 1".into(),
            in_flight: None,
            render_failure: Some(RenderFailure {
                content_hash: content,
                scale: 1.0,
                message: "the Vello renderer panicked".into(),
            }),
        };

        // Re-requesting the same content at the same quality would fail again;
        // without this the pane would spin on a broken page.
        assert!(!page.needs_render(1.0));
        // A sharper request gets one retry, so a transient draft failure can
        // still recover.
        assert!(page.needs_render(2.0));

        // An edit changes the hash, which makes the page renderable again.
        let edited = PreviewPage {
            content_hash: Fingerprint::from_pair(1, 3),
            ..page
        };
        assert!(edited.needs_render(1.0));
    }

    #[test]
    fn collab_pdf_exports_are_written_to_unique_temp_files() {
        use base64::Engine as _;

        let bytes = b"%PDF-1.7\ncollab export\n";
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);

        let first = write_temp_pdf(&encoded, "paper").expect("decode and write");
        let second = write_temp_pdf(&encoded, "paper").expect("decode and write");

        assert_ne!(
            first, second,
            "repeated exports must not overwrite each other"
        );
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        assert_eq!(std::fs::read(&second).unwrap(), bytes);
        assert_eq!(first.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("suzuri-paper-")
        );

        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
    }

    #[test]
    fn pdf_export_names_cannot_escape_the_temp_directory() {
        use base64::Engine as _;

        let encoded = base64::engine::general_purpose::STANDARD.encode(b"pdf");
        let path = write_temp_pdf(&encoded, "dir/sub\\name").expect("decode and write");

        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            !name.contains('/') && !name.contains('\\'),
            "unsanitized export name: {name}"
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn invalid_pdf_payloads_are_rejected() {
        assert!(write_temp_pdf("not base64!", "paper").is_none());
    }

    #[test]
    fn export_responses_accept_both_tinymist_shapes() {
        assert_eq!(
            export_output_path(&serde_json::json!({ "path": "/tmp/a.pdf" })),
            Some(PathBuf::from("/tmp/a.pdf"))
        );
        assert_eq!(
            export_output_path(&serde_json::json!({ "items": [{ "path": "/tmp/b.pdf" }] })),
            Some(PathBuf::from("/tmp/b.pdf"))
        );
        assert_eq!(
            export_output_path(&serde_json::json!({ "items": [] })),
            None
        );
        assert_eq!(export_output_path(&serde_json::json!({})), None);
    }
}
