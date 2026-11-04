//! A read-only viewer for JSON Canvas files (`.canvas`, the format Obsidian's
//! Canvas uses): cards for notes, text, images, and links, joined by arrows.
//!
//! The file is read through the project's buffer for it, so the view follows
//! every change to the file: an agent rewriting it on disk, or an edit in
//! the JSON editor that "Edit JSON" opens beside it.

mod canvas_model;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;
use editor::Editor;
use file_icons::FileIcons;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, ParentElement, PathBuilder, PinchEvent, Pixels, Point, Render,
    ScrollEvent, SharedString, Styled, StyledImage as _, Subscription, Task, Window, actions,
    canvas, div, img, point, px, relative, rems, rgb, size,
};
use language::{Buffer, BufferEvent};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::{Project, ProjectEntryId, ProjectItem, ProjectPath};
use theme::ActiveTheme as _;
use ui::utils::WithRemSize;
use ui::{
    Color, Icon, IconButton, IconName, IconSize, Label, LabelCommon as _, Tooltip, h_flex,
    prelude::*,
};
use util::ResultExt as _;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::{Pane, SplitDirection, Workspace, WorkspaceId};

use canvas_model::{Canvas, CanvasColor, EdgeCurve, Node, NodeKind, Side, side_direction};

actions!(
    json_canvas,
    [
        /// Zooms the canvas in.
        ZoomIn,
        /// Zooms the canvas out.
        ZoomOut,
        /// Zooms and pans so the whole canvas fits the view.
        ZoomToFit,
        /// Opens the canvas file as JSON in a split beside the canvas.
        EditJson,
    ]
);

pub fn init(cx: &mut App) {
    workspace::register_project_item::<CanvasView>(cx);
}

pub fn is_canvas_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("canvas"))
}

pub struct CanvasItem {
    buffer: Entity<Buffer>,
    project_path: ProjectPath,
    abs_path: PathBuf,
    /// What `file` paths in the canvas are relative to. Obsidian writes them
    /// relative to the vault, and the nearest thing here is the folder the
    /// project opened.
    vault_root: PathBuf,
}

impl CanvasItem {
    fn file_name(&self) -> &str {
        self.abs_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("untitled.canvas")
    }
}

impl ProjectItem for CanvasItem {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<Result<Entity<Self>>>> {
        let worktree = project.read(cx).worktree_for_id(path.worktree_id, cx)?;
        // A file opened on its own is a worktree whose only entry has an
        // empty relative path, so the extension is only on the absolute path.
        let abs_path = worktree.read(cx).absolutize(&path.path);
        if !is_canvas_path(&abs_path) {
            return None;
        }
        let worktree_root = worktree.read(cx).abs_path().to_path_buf();
        let vault_root = if worktree_root == abs_path {
            abs_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(worktree_root)
        } else {
            worktree_root
        };
        let project_path = path.clone();
        let open_buffer = project.update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        });
        Some(cx.spawn(async move |cx| {
            let buffer = open_buffer.await?;
            Ok(cx.new(|_| CanvasItem {
                buffer,
                project_path,
                abs_path,
                vault_root,
            }))
        }))
    }

    fn entry_id(&self, cx: &App) -> Option<ProjectEntryId> {
        ProjectItem::entry_id(self.buffer.read(cx), cx)
    }

    fn project_path(&self, _cx: &App) -> Option<ProjectPath> {
        Some(self.project_path.clone())
    }

    fn is_dirty(&self) -> bool {
        // The viewer never edits. Unsaved JSON edits belong to the editor
        // that made them, which prompts for them itself.
        false
    }
}

/// What a card shows besides its frame, built once per node and rebuilt
/// only when the node's own content changes.
enum CardContent {
    Text(Entity<Markdown>),
    Note {
        markdown: Entity<Markdown>,
        project_path: ProjectPath,
        _load: Task<()>,
    },
    Image {
        abs_path: PathBuf,
        project_path: ProjectPath,
    },
    OtherFile {
        project_path: ProjectPath,
        name: SharedString,
    },
    Missing {
        path: SharedString,
    },
    None,
}

struct Card {
    /// The node content this card was built from, to tell on reload whether
    /// it can be kept.
    source: NodeKind,
    content: CardContent,
}

struct Drag {
    start: Point<Pixels>,
    start_offset: Point<f32>,
    pressed_card: Option<String>,
    moved: bool,
}

/// A press that travels less than this is a click, not a pan.
const CLICK_SLOP: f32 = 4.0;
const FIT_MARGIN: f32 = 40.0;
const ZOOM_STEP: f32 = 1.25;

pub struct CanvasView {
    item: Entity<CanvasItem>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    canvas: Canvas,
    parse_error: Option<SharedString>,
    cards: HashMap<String, Card>,
    zoom: f32,
    /// Where canvas `(0, 0)` sits, from the viewport's top-left corner.
    offset: Point<f32>,
    fitted: bool,
    /// The viewport as last laid out, written during prepaint.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    /// Each note card's buffer and its subscription to it, by node id. The
    /// project holds buffers weakly, so without this handle a note would be
    /// dropped once loaded and its edits would never arrive.
    note_buffers: HashMap<String, (Entity<Buffer>, Subscription)>,
    _buffer_subscription: Subscription,
}

pub enum CanvasViewEvent {}

impl EventEmitter<CanvasViewEvent> for CanvasView {}

impl CanvasView {
    fn new(
        item: Entity<CanvasItem>,
        project: Entity<Project>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = item.read(cx).buffer.clone();
        let buffer_subscription =
            cx.subscribe(&buffer, |this, _, event: &BufferEvent, cx| match event {
                BufferEvent::Edited { .. } | BufferEvent::Reloaded => this.reload(cx),
                _ => {}
            });
        let mut this = Self {
            item,
            project,
            focus_handle: cx.focus_handle(),
            canvas: Canvas::default(),
            parse_error: None,
            cards: HashMap::new(),
            zoom: 1.0,
            offset: point(0.0, 0.0),
            fitted: false,
            viewport: Rc::default(),
            drag: None,
            note_buffers: HashMap::new(),
            _buffer_subscription: buffer_subscription,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let source = self.item.read(cx).buffer.read(cx).text();
        match canvas_model::parse_canvas(&source) {
            Ok(canvas) => {
                // A canvas that was empty has nothing to keep in view, so
                // fit to whatever was just written into it.
                if self.canvas.nodes.is_empty() {
                    self.fitted = false;
                }
                self.canvas = canvas;
                self.parse_error = None;
                self.rebuild_cards(cx);
            }
            // Most likely a write caught halfway. Keep showing the last
            // canvas that parsed, and say why it is not updating.
            Err(error) => self.parse_error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn rebuild_cards(&mut self, cx: &mut Context<Self>) {
        let mut previous = std::mem::take(&mut self.cards);
        for node in self.canvas.nodes.clone() {
            let card = match previous.remove(&node.id) {
                Some(card) if card.source == node.kind => card,
                _ => {
                    self.note_buffers.remove(&node.id);
                    Card {
                        source: node.kind.clone(),
                        content: self.card_content(&node, cx),
                    }
                }
            };
            self.cards.insert(node.id, card);
        }
        for removed in previous.keys() {
            self.note_buffers.remove(removed);
        }
    }

    fn card_content(&self, node: &Node, cx: &mut Context<Self>) -> CardContent {
        let languages = self.project.read(cx).languages().clone();
        match &node.kind {
            NodeKind::Text { text } => CardContent::Text(
                cx.new(|cx| Markdown::new(text.clone().into(), Some(languages), None, cx)),
            ),
            NodeKind::File { path, subpath } => {
                self.file_content(&node.id, path, subpath.clone(), cx)
            }
            NodeKind::Link { .. } | NodeKind::Group { .. } | NodeKind::Unknown { .. } => {
                CardContent::None
            }
        }
    }

    fn file_content(
        &self,
        node_id: &str,
        path: &str,
        subpath: Option<String>,
        cx: &mut Context<Self>,
    ) -> CardContent {
        let abs_path = self.item.read(cx).vault_root.join(path);
        let project = self.project.read(cx);
        let Some(project_path) = project
            .find_project_path(&abs_path, cx)
            .filter(|project_path| {
                project
                    .entry_for_path(project_path, cx)
                    .is_some_and(|entry| entry.is_file())
            })
        else {
            return CardContent::Missing {
                path: path.to_string().into(),
            };
        };
        let extension = abs_path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let is_local = project
            .worktree_for_id(project_path.worktree_id, cx)
            .is_some_and(|worktree| worktree.read(cx).is_local());
        let name: SharedString = abs_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
            .into();

        match extension.as_str() {
            "md" | "markdown" => {
                let languages = project.languages().clone();
                let markdown =
                    cx.new(|cx| Markdown::new(SharedString::default(), Some(languages), None, cx));
                let open_buffer = self.project.update(cx, |project, cx| {
                    project.open_buffer(project_path.clone(), cx)
                });
                let load = cx.spawn({
                    let markdown = markdown.clone();
                    let node_id = node_id.to_string();
                    async move |this, cx| {
                        let Some(buffer) = open_buffer.await.log_err() else {
                            return;
                        };
                        this.update(cx, |this, cx| {
                            show_note(&markdown, &buffer, subpath.as_deref(), cx);
                            // Follow edits to the note, so a note being
                            // written shows its current text.
                            let subscription = cx.subscribe(&buffer, {
                                let markdown = markdown.clone();
                                move |_, buffer, event: &BufferEvent, cx| {
                                    if matches!(
                                        event,
                                        BufferEvent::Edited { .. } | BufferEvent::Reloaded
                                    ) {
                                        show_note(&markdown, &buffer, subpath.as_deref(), cx);
                                    }
                                }
                            });
                            this.note_buffers.insert(node_id, (buffer, subscription));
                        })
                        .log_err();
                    }
                });
                CardContent::Note {
                    markdown,
                    project_path,
                    _load: load,
                }
            }
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" if is_local => {
                CardContent::Image {
                    abs_path,
                    project_path,
                }
            }
            _ => CardContent::OtherFile { project_path, name },
        }
    }
}

fn show_note(
    markdown: &Entity<Markdown>,
    buffer: &Entity<Buffer>,
    subpath: Option<&str>,
    cx: &mut App,
) {
    let excerpt = canvas_model::note_excerpt(&buffer.read(cx).text(), subpath);
    markdown.update(cx, |markdown, cx| markdown.reset(excerpt.into(), cx));
}

/// Obsidian's shades for the six preset colors.
fn canvas_color(color: CanvasColor) -> Hsla {
    let hex = match color {
        CanvasColor::Preset(1) => 0xe93147,
        CanvasColor::Preset(2) => 0xec7500,
        CanvasColor::Preset(3) => 0xe0ac00,
        CanvasColor::Preset(4) => 0x08b94e,
        CanvasColor::Preset(5) => 0x00bfbc,
        CanvasColor::Preset(_) => 0x7852ee,
        CanvasColor::Rgb(hex) => hex,
    };
    rgb(hex).into()
}

/// A document can link anything the system knows how to open, including
/// local files and applications, so only web and mail links are followed.
fn is_followable_url(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    })
}

impl CanvasView {
    fn viewport_size(&self) -> gpui::Size<f32> {
        let viewport = self.viewport.get().size;
        size(viewport.width.into(), viewport.height.into())
    }

    fn zoom_to_fit(&mut self) {
        let viewport = self.viewport_size();
        if viewport.width <= 0.0 || viewport.height <= 0.0 {
            return;
        }
        if let Some(content) = self.canvas.content_bounds() {
            (self.zoom, self.offset) = canvas_model::fit(content, viewport, FIT_MARGIN);
        }
        self.fitted = true;
    }

    fn zoom_by(&mut self, factor: f32, anchor: Point<f32>) {
        (self.zoom, self.offset) =
            canvas_model::zoom_about(self.zoom, self.offset, self.zoom * factor, anchor);
    }

    fn viewport_center(&self) -> Point<f32> {
        let viewport = self.viewport_size();
        point(viewport.width / 2.0, viewport.height / 2.0)
    }

    /// A window position as a position inside the viewport.
    fn to_viewport(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self.viewport.get().origin;
        point(
            (position.x - origin.x).into(),
            (position.y - origin.y).into(),
        )
    }

    fn card_id_at(&self, position: Point<Pixels>) -> Option<String> {
        let local = self.to_viewport(position);
        let on_canvas = point(
            (local.x - self.offset.x) / self.zoom,
            (local.y - self.offset.y) / self.zoom,
        );
        self.canvas.card_at(on_canvas).map(|node| node.id.clone())
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.drag = Some(Drag {
            start: event.position,
            start_offset: self.offset,
            pressed_card: self.card_id_at(event.position),
            moved: false,
        });
    }

    fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let delta_x: f32 = (event.position.x - drag.start.x).into();
        let delta_y: f32 = (event.position.y - drag.start.y).into();
        if !drag.moved && delta_x.hypot(delta_y) < CLICK_SLOP {
            return;
        }
        drag.moved = true;
        self.offset = point(drag.start_offset.x + delta_x, drag.start_offset.y + delta_y);
        cx.notify();
    }

    fn handle_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if drag.moved {
            return;
        }
        if let Some(pressed) = drag.pressed_card
            && self.card_id_at(event.position).as_ref() == Some(&pressed)
        {
            self.open_card(&pressed, window, cx);
        }
    }

    fn handle_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(window.line_height());
        let (mut delta_x, mut delta_y): (f32, f32) = (delta.x.into(), delta.y.into());
        if event.modifiers.platform || event.modifiers.control {
            let anchor = self.to_viewport(event.position);
            self.zoom_by((delta_y * 0.01).exp(), anchor);
        } else {
            // A mouse wheel only scrolls vertically; shift turns it sideways.
            if event.modifiers.shift && delta_x == 0.0 {
                (delta_x, delta_y) = (delta_y, 0.0);
            }
            self.offset = point(self.offset.x + delta_x, self.offset.y + delta_y);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn handle_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.to_viewport(event.position);
        self.zoom_by(1.0 + event.delta, anchor);
        cx.notify();
    }

    fn open_card(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.canvas.node(id) else {
            return;
        };
        if let NodeKind::Link { url } = &node.kind {
            if is_followable_url(url) {
                cx.open_url(url);
            } else {
                log::info!("json_canvas: not opening link {url:?}, only web and mail links open");
            }
            return;
        }
        let project_path = match self.cards.get(id).map(|card| &card.content) {
            Some(
                CardContent::Note { project_path, .. }
                | CardContent::Image { project_path, .. }
                | CardContent::OtherFile { project_path, .. },
            ) => project_path.clone(),
            _ => return,
        };
        let Some(workspace) = Workspace::for_window(window, cx) else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace
                .open_path(project_path, None, true, window, cx)
                .detach_and_log_err(cx);
        });
    }

    fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, self.viewport_center());
        cx.notify();
    }

    fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, self.viewport_center());
        cx.notify();
    }

    fn zoom_to_fit_action(&mut self, _: &ZoomToFit, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_to_fit();
        cx.notify();
    }

    fn edit_json(&mut self, _: &EditJson, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = Workspace::for_window(window, cx) else {
            return;
        };
        let buffer = self.item.read(cx).buffer.clone();
        let project = self.project.clone();
        let editor = cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx));
        workspace.update(cx, |workspace, cx| {
            workspace.split_item(SplitDirection::Right, Box::new(editor), window, cx);
        });
    }

    fn to_view_bounds(&self, bounds: Bounds<f32>) -> Bounds<Pixels> {
        Bounds::new(
            point(
                px(bounds.origin.x * self.zoom + self.offset.x),
                px(bounds.origin.y * self.zoom + self.offset.y),
            ),
            size(
                px(bounds.size.width * self.zoom),
                px(bounds.size.height * self.zoom),
            ),
        )
    }

    fn to_view_point(&self, position: Point<f32>) -> Point<f32> {
        point(
            position.x * self.zoom + self.offset.x,
            position.y * self.zoom + self.offset.y,
        )
    }

    fn render_group(&self, node: &Node, bounds: Bounds<Pixels>, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let accent = node.color.map(canvas_color);
        let label = match &node.kind {
            NodeKind::Group { label } => label.clone(),
            _ => None,
        };
        div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .rounded(px(8.0 * self.zoom))
            .border(px((2.0 * self.zoom).clamp(1.0, 3.0)))
            .border_color(accent.unwrap_or(colors.border))
            .bg(accent.unwrap_or(colors.text_muted).opacity(0.06))
            .debug_selector(|| format!("canvas-node-{}", node.id))
            .when_some(label, |this, label| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_full()
                        .pb(px(4.0 * self.zoom))
                        .text_size(px(16.0 * self.zoom))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(accent.unwrap_or(colors.text_muted))
                        .whitespace_nowrap()
                        .child(label),
                )
            })
            .into_any_element()
    }

    fn render_card(
        &self,
        node: &Node,
        bounds: Bounds<Pixels>,
        markdown_style: &MarkdownStyle,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let accent = node.color.map(canvas_color);
        let content = self.cards.get(&node.id).map(|card| &card.content);
        let opens = matches!(node.kind, NodeKind::Link { .. })
            || matches!(
                content,
                Some(
                    CardContent::Note { .. }
                        | CardContent::Image { .. }
                        | CardContent::OtherFile { .. }
                )
            );
        let file_name = match &node.kind {
            NodeKind::File { path, .. } => Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            _ => None,
        };

        let body: AnyElement = match (&node.kind, content) {
            (_, Some(CardContent::Text(markdown)) | Some(CardContent::Note { markdown, .. })) => {
                div()
                    .size_full()
                    .p(rems(1.0))
                    .child(MarkdownElement::new(
                        markdown.clone(),
                        markdown_style.clone(),
                    ))
                    .into_any_element()
            }
            (_, Some(CardContent::Image { abs_path, .. })) => img(abs_path.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            (_, Some(CardContent::OtherFile { name, project_path })) => {
                let icon = FileIcons::get_icon(project_path.path.as_std_path(), cx);
                centered_card_body(
                    icon.map(Icon::from_path)
                        .unwrap_or_else(|| Icon::new(IconName::File)),
                    name.clone(),
                    colors.text,
                )
            }
            (_, Some(CardContent::Missing { path })) => centered_card_body(
                Icon::new(IconName::Warning).color(Color::Warning),
                format!("Not found: {path}").into(),
                colors.text_muted,
            ),
            (NodeKind::Link { url }, _) => centered_card_body(
                Icon::new(IconName::Link).color(Color::Accent),
                url.clone().into(),
                colors.text_accent,
            ),
            (NodeKind::Unknown { kind }, _) => centered_card_body(
                Icon::new(IconName::Warning).color(Color::Muted),
                format!("Unsupported card type \u{201c}{kind}\u{201d}").into(),
                colors.text_muted,
            ),
            _ => div().into_any_element(),
        };

        let card = div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .debug_selector(|| format!("canvas-node-{}", node.id))
            .when(opens, |this| this.cursor_pointer())
            .when_some(file_name, |this, name| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_full()
                        .pb(px(3.0 * self.zoom))
                        .text_size(px(12.0 * self.zoom))
                        .text_color(colors.text_muted)
                        .whitespace_nowrap()
                        .child(name),
                )
            })
            .child(
                div()
                    .size_full()
                    .rounded(px(8.0 * self.zoom))
                    .border(px((2.0 * self.zoom).clamp(1.0, 3.0)))
                    .border_color(accent.unwrap_or(colors.border))
                    .bg(colors.editor_background)
                    .overflow_hidden()
                    .child(
                        div()
                            .size_full()
                            .when_some(accent, |this, accent| this.bg(accent.opacity(0.08)))
                            // Everything inside a card is sized in rems, so
                            // scaling the rem scales the card's contents
                            // with the zoom.
                            .child(
                                WithRemSize::new(window.rem_size() * self.zoom)
                                    .size_full()
                                    .child(body),
                            ),
                    ),
            );
        card.into_any_element()
    }
}

fn centered_card_body(icon: Icon, text: SharedString, color: Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .p(rems(1.0))
        .gap(rems(0.5))
        .items_center()
        .justify_center()
        .child(icon.size(IconSize::Custom(rems(1.5))))
        .child(
            div()
                .text_size(rems(0.875))
                .text_color(color)
                .text_center()
                .child(text),
        )
        .into_any_element()
}

struct PaintedEdge {
    curve: EdgeCurve,
    color: Hsla,
    from_arrow: bool,
    to_arrow: bool,
}

fn paint_edge(edge: &PaintedEdge, origin: Point<Pixels>, zoom: f32, window: &mut Window) {
    let at = |position: Point<f32>| point(origin.x + px(position.x), origin.y + px(position.y));
    let arrow_length = (14.0 * zoom).clamp(6.0, 28.0);
    // An arrowed end stops at the arrow's base, so the stroke does not poke
    // through its tip.
    let pull_back = |end: Point<f32>, side: Side, arrow: bool| {
        if !arrow {
            return end;
        }
        let direction = side_direction(side);
        point(
            end.x + direction.x * arrow_length,
            end.y + direction.y * arrow_length,
        )
    };
    let curve = edge.curve;
    let start = pull_back(curve.start, curve.start_side, edge.from_arrow);
    let end = pull_back(curve.end, curve.end_side, edge.to_arrow);

    let mut stroke = PathBuilder::stroke(px((2.0 * zoom).clamp(1.0, 4.0)));
    stroke.move_to(at(start));
    stroke.cubic_bezier_to(at(end), at(curve.control_start), at(curve.control_end));
    if let Some(path) = stroke.build().log_err() {
        window.paint_path(path, edge.color);
    }

    let mut paint_arrow = |tip: Point<f32>, side: Side| {
        let direction = side_direction(side);
        let base = point(
            tip.x + direction.x * arrow_length,
            tip.y + direction.y * arrow_length,
        );
        let half_width = arrow_length * 0.5;
        let across = point(-direction.y * half_width, direction.x * half_width);
        let mut arrow = PathBuilder::fill();
        arrow.add_polygon(
            &[
                at(tip),
                at(point(base.x + across.x, base.y + across.y)),
                at(point(base.x - across.x, base.y - across.y)),
            ],
            true,
        );
        if let Some(path) = arrow.build().log_err() {
            window.paint_path(path, edge.color);
        }
    };
    if edge.to_arrow {
        paint_arrow(curve.end, curve.end_side);
    }
    if edge.from_arrow {
        paint_arrow(curve.start, curve.start_side);
    }
}

impl Focusable for CanvasView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CanvasView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.fitted {
            self.zoom_to_fit();
        }
        let colors = cx.theme().colors();
        let viewport_size = self.viewport.get().size;
        // Cards just outside the view are kept so their labels, which sit
        // above them, do not pop in late.
        let visible = Bounds::new(point(px(-200.0), px(-200.0)), viewport_size).dilate(px(200.0));

        let mut markdown_style = MarkdownStyle::themed(MarkdownFont::Preview, window, cx);
        // `themed` sizes the line height and code in pixels; rems make them
        // follow the zoom along with the text.
        markdown_style.base_text_style.line_height = relative(1.5);
        markdown_style.prevent_mouse_interaction = true;
        markdown_style.code_block.text.font_size = Some(rems(0.85).into());
        markdown_style.inline_code.font_size = Some(rems(0.85).into());

        let mut groups = Vec::new();
        let mut cards = Vec::new();
        for node in &self.canvas.nodes {
            let bounds = self.to_view_bounds(node.bounds);
            if !bounds.intersects(&visible) {
                continue;
            }
            if matches!(node.kind, NodeKind::Group { .. }) {
                groups.push(self.render_group(node, bounds, cx));
            } else {
                cards.push(self.render_card(node, bounds, &markdown_style, window, cx));
            }
        }

        let mut edges = Vec::new();
        let mut edge_labels = Vec::new();
        for edge in &self.canvas.edges {
            let (Some(from), Some(to)) = (
                self.canvas.node(&edge.from_node),
                self.canvas.node(&edge.to_node),
            ) else {
                continue;
            };
            let curve = EdgeCurve::between(from, edge.from_side, to, edge.to_side);
            let color = edge.color.map(canvas_color).unwrap_or(colors.text_muted);
            let view_curve = EdgeCurve {
                start: self.to_view_point(curve.start),
                control_start: self.to_view_point(curve.control_start),
                control_end: self.to_view_point(curve.control_end),
                end: self.to_view_point(curve.end),
                ..curve
            };
            if let Some(label) = &edge.label {
                let middle = view_curve.midpoint();
                let label_width = 260.0 * self.zoom;
                let label_height = 22.0 * self.zoom;
                edge_labels.push(
                    h_flex()
                        .absolute()
                        .left(px(middle.x - label_width / 2.0))
                        .top(px(middle.y - label_height / 2.0))
                        .w(px(label_width))
                        .h(px(label_height))
                        .justify_center()
                        .child(
                            div()
                                .px(px(6.0 * self.zoom))
                                .rounded(px(4.0 * self.zoom))
                                .bg(colors.editor_background)
                                .text_size(px(13.0 * self.zoom))
                                .text_color(colors.text_muted)
                                .whitespace_nowrap()
                                .child(label.clone()),
                        ),
                );
            }
            edges.push(PaintedEdge {
                curve: view_curve,
                color,
                from_arrow: edge.from_arrow,
                to_arrow: edge.to_arrow,
            });
        }

        let viewport = self.viewport.clone();
        let view_id = cx.entity_id();
        let zoom = self.zoom;
        let zoom_percent = format!("{:.0}%", self.zoom * 100.0);
        let is_empty = self.canvas.nodes.is_empty();

        div()
            .id("json-canvas")
            .key_context("JsonCanvas")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_to_fit_action))
            .on_action(cx.listener(Self::edit_json))
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(colors.panel_background)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::handle_mouse_down))
            .on_mouse_move(cx.listener(Self::handle_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::handle_mouse_up))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None),
            )
            .on_scroll(cx.listener(Self::handle_scroll))
            .on_pinch(cx.listener(Self::handle_pinch))
            .children(groups)
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        // Cards are placed from the viewport's size, which is
                        // only known once laid out, so a change re-renders.
                        if viewport.get() != bounds {
                            viewport.set(bounds);
                            cx.notify(view_id);
                        }
                    },
                    move |bounds, _, window, _cx| {
                        for edge in &edges {
                            paint_edge(edge, bounds.origin, zoom, window);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(cards)
            .children(edge_labels)
            .when(is_empty, |this| {
                this.child(
                    h_flex()
                        .absolute()
                        .size_full()
                        .justify_center()
                        .child(Label::new("This canvas is empty.").color(Color::Muted)),
                )
            })
            .when_some(self.parse_error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .absolute()
                        .top_2()
                        .left_2()
                        .right_2()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .occlude()
                        .bg(colors.elevated_surface_background)
                        .border_1()
                        .border_color(colors.border)
                        .child(Icon::new(IconName::Warning).color(Color::Warning))
                        .child(
                            Label::new(format!(
                                "This canvas could not be read, so the last version that could is shown: {error}"
                            ))
                            .size(ui::LabelSize::Small),
                        ),
                )
            })
            .child(
                h_flex()
                    .absolute()
                    .bottom_2()
                    .right_2()
                    .gap_0p5()
                    .p_0p5()
                    .rounded_md()
                    .occlude()
                    .bg(colors.elevated_surface_background)
                    .border_1()
                    .border_color(colors.border)
                    .child(
                        IconButton::new("zoom-out", IconName::Dash)
                            .tooltip(Tooltip::text("Zoom Out"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_out(&ZoomOut, window, cx)
                            })),
                    )
                    .child(
                        Label::new(zoom_percent)
                            .size(ui::LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("zoom-in", IconName::Plus)
                            .tooltip(Tooltip::text("Zoom In"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_in(&ZoomIn, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("zoom-to-fit", IconName::Maximize)
                            .tooltip(Tooltip::text("Zoom to Fit"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_to_fit_action(&ZoomToFit, window, cx)
                            })),
                    )
                    .child(
                        IconButton::new("edit-json", IconName::Code)
                            .tooltip(Tooltip::text("Edit JSON"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.edit_json(&EditJson, window, cx)
                            })),
                    ),
            )
    }
}

impl Item for CanvasView {
    type Event = CanvasViewEvent;

    fn to_item_events(event: &Self::Event, _: &mut dyn FnMut(ItemEvent)) {
        match *event {}
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.item.read(cx).file_name().to_string().into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, cx: &App) -> Option<Icon> {
        FileIcons::get_icon(&self.item.read(cx).abs_path, cx).map(Icon::from_path)
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(self.item.read(cx).abs_path.display().to_string().into())
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        f(self.item.entity_id(), self.item.read(cx))
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
        let item = self.item.clone();
        let project = self.project.clone();
        let (zoom, offset) = (self.zoom, self.offset);
        Task::ready(Some(cx.new(|cx| {
            let mut view = Self::new(item, project, window, cx);
            (view.zoom, view.offset, view.fitted) = (zoom, offset, true);
            view
        })))
    }

    fn buffer_kind(&self, _cx: &App) -> workspace::item::ItemBufferKind {
        workspace::item::ItemBufferKind::Singleton
    }
}

impl workspace::item::ProjectItem for CanvasView {
    type Item = CanvasItem;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use serde_json::json;
    use std::sync::Arc;
    use util::{path, rel_path::rel_path};
    use workspace::{AppState, MultiWorkspace};

    const CANVAS: &str = r#"{
      "nodes": [
        {"id":"papers","type":"group","x":-20,"y":-40,"width":900,"height":400,"label":"Papers"},
        {"id":"note","type":"file","x":0,"y":0,"width":300,"height":200,"file":"notes/Chen2021.md"},
        {"id":"gone","type":"file","x":400,"y":0,"width":300,"height":200,"file":"notes/Deleted.md"},
        {"id":"claim","type":"text","x":0,"y":300,"width":300,"height":120,"text":"**Synthesis**"},
        {"id":"web","type":"link","x":400,"y":300,"width":300,"height":120,"url":"https://jsoncanvas.org/"},
        {"id":"app","type":"link","x":800,"y":300,"width":300,"height":120,"url":"file:///Applications/Calculator.app"}
      ],
      "edges": [
        {"id":"e","fromNode":"note","toNode":"claim","label":"supports"}
      ]
    }"#;

    struct Fixture<'a> {
        fs: Arc<FakeFs>,
        workspace: Entity<Workspace>,
        view: Entity<CanvasView>,
        cx: &'a mut VisualTestContext,
    }

    async fn open_canvas(cx: &mut TestAppContext) -> Fixture<'_> {
        cx.update(|cx| {
            let _state = AppState::test(cx);
            editor::init(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/vault"),
            json!({
                "map.canvas": CANVAS,
                "notes": {
                    "Chen2021.md": "---\ntags: [paper]\n---\n# Chen (2021)\nRemote work raised output.",
                },
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/vault").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .worktrees(cx)
                .next()
                .expect("a worktree")
                .read(cx)
                .id()
        });
        let item = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(
                    (worktree_id, rel_path("map.canvas")),
                    None,
                    true,
                    window,
                    cx,
                )
            })
            .await
            .expect("the canvas opens");
        let view = item
            .downcast::<CanvasView>()
            .expect("a .canvas file opens in the canvas viewer, not as JSON");
        redraw(cx);
        Fixture {
            fs,
            workspace,
            view,
            cx,
        }
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    fn card_center(id: &str, cx: &mut VisualTestContext) -> Point<Pixels> {
        // `debug_bounds` takes a `'static` selector.
        let selector: &'static str = format!("canvas-node-{id}").leak();
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("card {id} is laid out"))
            .center()
    }

    fn click(position: Point<Pixels>, cx: &mut VisualTestContext) {
        cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
    }

    fn active_path(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<String> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)?
                .project_path(cx)
                .map(|path| path.path.as_unix_str().to_string())
        })
    }

    #[gpui::test]
    async fn a_canvas_opens_fitted_with_every_card_drawn(cx: &mut TestAppContext) {
        let Fixture { view, cx, .. } = open_canvas(cx).await;
        view.read_with(cx, |view, _| {
            assert_eq!(view.canvas.nodes.len(), 6);
            assert!(view.fitted, "the first frame fits the canvas to the view");
            assert!(view.zoom <= 1.0);
        });
        for id in ["papers", "note", "gone", "claim", "web", "app"] {
            card_center(id, cx);
        }
        let shown = view.read_with(cx, |view, cx| match &view.cards["note"].content {
            CardContent::Note { markdown, .. } => markdown.read(cx).source().to_string(),
            _ => panic!("a markdown file is shown as a note"),
        });
        assert_eq!(
            shown, "# Chen (2021)\nRemote work raised output.",
            "the note's frontmatter is left out"
        );
        view.read_with(cx, |view, _| {
            assert!(matches!(
                view.cards["gone"].content,
                CardContent::Missing { .. }
            ));
        });
    }

    #[gpui::test]
    async fn clicking_a_note_card_opens_the_note(cx: &mut TestAppContext) {
        let Fixture { workspace, cx, .. } = open_canvas(cx).await;
        click(card_center("note", cx), cx);
        assert_eq!(
            active_path(&workspace, cx).as_deref(),
            Some("notes/Chen2021.md")
        );
    }

    #[gpui::test]
    async fn clicking_cards_that_open_nothing_stays_on_the_canvas(cx: &mut TestAppContext) {
        let Fixture { workspace, cx, .. } = open_canvas(cx).await;
        for id in ["gone", "claim", "app"] {
            click(card_center(id, cx), cx);
            assert_eq!(active_path(&workspace, cx).as_deref(), Some("map.canvas"));
        }
        assert_eq!(
            cx.opened_url(),
            None,
            "a link to a local application is not followed"
        );

        click(card_center("web", cx), cx);
        assert_eq!(cx.opened_url().as_deref(), Some("https://jsoncanvas.org/"));
    }

    /// A drag that starts on a card pans the canvas; it is not a click.
    #[gpui::test]
    async fn dragging_pans_instead_of_opening(cx: &mut TestAppContext) {
        let Fixture {
            workspace,
            view,
            cx,
            ..
        } = open_canvas(cx).await;
        let offset_before = view.read_with(cx, |view, _| view.offset);
        let start = card_center("note", cx);
        let end = point(start.x + px(120.0), start.y + px(30.0));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let offset_after = view.read_with(cx, |view, _| view.offset);
        assert_eq!(
            point(
                offset_after.x - offset_before.x,
                offset_after.y - offset_before.y
            ),
            point(120.0, 30.0)
        );
        assert_eq!(active_path(&workspace, cx).as_deref(), Some("map.canvas"));
    }

    /// The point of the viewer: an agent rewrites the file, and the canvas
    /// on screen follows. A write caught halfway keeps the last good canvas.
    #[gpui::test]
    async fn the_canvas_follows_the_file_on_disk(cx: &mut TestAppContext) {
        let Fixture { fs, view, cx, .. } = open_canvas(cx).await;
        let canvas_path = Path::new(path!("/vault/map.canvas"));

        fs.insert_file(canvas_path, br#"{"nodes": [{"id":"only","type":"text","x":0,"y":0,"width":100,"height":50,"text":"new"}]}"#.to_vec())
            .await;
        redraw(cx);
        view.read_with(cx, |view, _| {
            let ids: Vec<_> = view
                .canvas
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect();
            assert_eq!(ids, ["only"]);
            assert!(view.cards.len() == 1 && view.note_buffers.is_empty());
        });

        fs.insert_file(canvas_path, br#"{"nodes": [{"id":"#.to_vec())
            .await;
        redraw(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.canvas.nodes.len(), 1, "the last good canvas stays");
            assert!(view.parse_error.is_some());
        });
    }

    #[gpui::test]
    async fn a_note_card_follows_edits_to_its_note(cx: &mut TestAppContext) {
        let Fixture { fs, view, cx, .. } = open_canvas(cx).await;
        fs.insert_file(
            Path::new(path!("/vault/notes/Chen2021.md")),
            b"# Chen (2021)\nRevised.".to_vec(),
        )
        .await;
        redraw(cx);
        let shown = view.read_with(cx, |view, cx| match &view.cards["note"].content {
            CardContent::Note { markdown, .. } => markdown.read(cx).source().to_string(),
            _ => panic!("still a note"),
        });
        assert_eq!(shown, "# Chen (2021)\nRevised.");
    }

    #[gpui::test]
    async fn edit_json_opens_the_file_beside_the_canvas(cx: &mut TestAppContext) {
        let Fixture {
            workspace,
            view,
            cx,
            ..
        } = open_canvas(cx).await;
        view.update_in(cx, |view, window, cx| view.edit_json(&EditJson, window, cx));
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.panes().len(), 2);
            let editor = workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<Editor>())
                .expect("the JSON opens in an editor");
            assert!(editor.read(cx).text(cx).contains("\"nodes\""));
        });
    }
}
