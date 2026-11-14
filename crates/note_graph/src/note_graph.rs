//! The local graph: the notes linked to and from the note being edited,
//! drawn as dots joined by lines in a pane beside it. The graph follows the
//! active note, and clicking a dot opens that note.
//!
//! Links are read from the files on disk, so the graph is right for notes
//! that are not open. The note at the center is read from its buffer
//! instead, so a link shows up as it is typed.

mod link_index;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use editor::Editor;
use fs::MTime;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathBuilder, PinchEvent, Pixels, Point, Render, ScrollEvent,
    SharedString, Styled, Subscription, Task, WeakEntity, Window, actions, canvas, div, point, px,
    size,
};
use language::{Buffer, BufferEvent};
use project::{Project, ProjectPath};
use theme::ActiveTheme as _;
use ui::{
    Button, ButtonCommon as _, Clickable as _, Color, Icon, IconButton, IconName, Label,
    LabelCommon as _, LabelSize, Toggleable as _, Tooltip, h_flex, prelude::*,
};
use util::ResultExt as _;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::{Pane, SplitDirection, Workspace};

use link_index::{
    DOT_RADIUS, LABEL_HEIGHT, LABEL_WIDTH, LinkGraph, LinkReference, Neighbourhood, NoteCatalog,
    extract_links,
};

actions!(
    note_graph,
    [
        /// Opens a graph of the notes linked to and from the current note.
        OpenLocalGraph,
        /// Zooms the graph in.
        ZoomIn,
        /// Zooms the graph out.
        ZoomOut,
        /// Zooms and pans so the whole graph fits the view.
        ZoomToFit,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        // Attached per render, and only while the active item is a note, so
        // the menu item greys out where there is nothing to graph.
        workspace.register_action_renderer(|div, workspace, _window, cx| {
            if active_note(workspace, cx).is_none() {
                return div;
            }
            div.on_action(cx.listener(|workspace, _: &OpenLocalGraph, window, cx| {
                open_local_graph(workspace, window, cx);
            }))
        });
    })
    .detach();
}

struct ActiveNote {
    path: ProjectPath,
    buffer: Entity<Buffer>,
    pane: WeakEntity<Pane>,
}

/// The note in the active editor. Runs on every render, so it only looks at
/// the path.
fn active_note(workspace: &Workspace, cx: &App) -> Option<ActiveNote> {
    let editor = workspace.active_item(cx)?.downcast::<Editor>()?;
    let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    let path = ProjectPath {
        worktree_id: file.worktree_id(cx),
        path: file.path().clone(),
    };
    is_note(&path).then(|| ActiveNote {
        path,
        buffer,
        pane: workspace.active_pane().downgrade(),
    })
}

/// Wikilinks only ever resolve to `.md` files, so those are the notes.
fn is_note(path: &ProjectPath) -> bool {
    path.path.extension() == Some("md")
}

/// Opens the graph for the note in the active editor, in a split beside
/// it, or points the graph that is already open at that note.
pub fn open_local_graph(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(note) = active_note(workspace, cx) else {
        return;
    };
    let existing = workspace.items_of_type::<GraphView>(cx).next();
    if let Some(existing) = existing {
        existing.update(cx, |view, cx| view.set_center(note, cx));
        workspace.activate_item(&existing, false, false, window, cx);
        return;
    }
    let source_pane = workspace.active_pane().clone();
    let project = workspace.project().clone();
    let workspace_handle = cx.entity();
    let view = cx.new(|cx| GraphView::new(&workspace_handle, project, note, window, cx));
    let pane = workspace.split_pane(source_pane.clone(), SplitDirection::Right, window, cx);
    pane.update(cx, |pane, cx| {
        pane.add_item(Box::new(view), false, false, None, window, cx);
    });
    // The graph is something to glance at while writing, so the cursor
    // stays in the note.
    window.focus(&source_pane.focus_handle(cx), cx);
}

struct IndexedNote {
    path: ProjectPath,
    absolute: PathBuf,
    mtime: Option<MTime>,
    links: Arc<[LinkReference]>,
}

struct Drag {
    start: Point<Pixels>,
    start_offset: Point<f32>,
    pressed_note: Option<usize>,
    moved: bool,
}

const FIT_MARGIN: f32 = 40.0;
const ZOOM_STEP: f32 = 1.25;
/// A press that moves less than this is a click, not a pan.
const CLICK_SLOP: f32 = 4.0;
const MAX_HOPS: usize = 3;
/// Past this many notes a drawing stops being readable, and each one costs
/// layout time on every frame.
const NOTE_LIMIT: usize = 400;
const INDEX_DEBOUNCE: Duration = Duration::from_millis(150);
const TYPING_DEBOUNCE: Duration = Duration::from_millis(300);

pub struct GraphView {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    notes: Vec<IndexedNote>,
    graph: LinkGraph,
    /// False until the notes' links have been read once.
    indexed: bool,
    center: ProjectPath,
    /// The center note's links as its buffer has them, which may be ahead
    /// of the file.
    center_links: Arc<[LinkReference]>,
    center_buffer: Entity<Buffer>,
    /// Where the center note is open, and so where a clicked note opens.
    note_pane: WeakEntity<Pane>,
    hops: usize,
    shown: Neighbourhood,
    hovered: Option<usize>,
    zoom: f32,
    /// Where the center note sits, from the viewport's top-left corner.
    offset: Point<f32>,
    fitted: bool,
    /// The viewport as last laid out, written during prepaint.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    index_task: Task<()>,
    typing_task: Task<()>,
    center_subscription: Subscription,
    _subscriptions: Vec<Subscription>,
}

pub enum GraphViewEvent {}

impl EventEmitter<GraphViewEvent> for GraphView {}

impl GraphView {
    fn new(
        workspace: &Entity<Workspace>,
        project: Entity<Project>,
        note: ActiveNote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.subscribe_in(
                workspace,
                window,
                |this, workspace, event: &workspace::Event, _window, cx| {
                    if let workspace::Event::ActiveItemChanged = event
                        // Any other active item, the graph itself included,
                        // leaves the graph on the note it was following.
                        && let Some(note) = active_note(workspace.read(cx), cx)
                    {
                        this.set_center(note, cx);
                    }
                },
            ),
            cx.subscribe(&project, |this, _, event: &project::Event, cx| {
                if matches!(
                    event,
                    project::Event::WorktreeUpdatedEntries(..)
                        | project::Event::WorktreeAdded(_)
                        | project::Event::WorktreeRemoved(_)
                ) {
                    this.refresh_index(INDEX_DEBOUNCE, cx);
                }
            }),
        ];
        let center_subscription = Self::follow_buffer(&note.buffer, cx);
        let mut this = Self {
            workspace: workspace.downgrade(),
            project,
            focus_handle: cx.focus_handle(),
            notes: Vec::new(),
            graph: LinkGraph::default(),
            indexed: false,
            center_links: extract_links(&note.buffer.read(cx).text()).into(),
            center: note.path,
            center_buffer: note.buffer,
            note_pane: note.pane,
            hops: 1,
            shown: Neighbourhood::default(),
            hovered: None,
            zoom: 1.0,
            offset: point(0.0, 0.0),
            fitted: false,
            viewport: Rc::default(),
            drag: None,
            index_task: Task::ready(()),
            typing_task: Task::ready(()),
            center_subscription,
            _subscriptions: subscriptions,
        };
        this.refresh_index(Duration::ZERO, cx);
        this
    }

    fn follow_buffer(buffer: &Entity<Buffer>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(buffer, |this, _, event: &BufferEvent, cx| {
            if let BufferEvent::Edited { .. } = event {
                this.typing_task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(TYPING_DEBOUNCE).await;
                    this.update(cx, |this, cx| {
                        let links: Arc<[LinkReference]> =
                            extract_links(&this.center_buffer.read(cx).text()).into();
                        if links != this.center_links {
                            this.center_links = links;
                            this.rebuild_graph(cx);
                        }
                    })
                    .log_err();
                });
            }
        })
    }

    fn set_center(&mut self, note: ActiveNote, cx: &mut Context<Self>) {
        self.note_pane = note.pane;
        if note.path == self.center && note.buffer == self.center_buffer {
            return;
        }
        self.center_subscription = Self::follow_buffer(&note.buffer, cx);
        self.typing_task = Task::ready(());
        self.center_links = extract_links(&note.buffer.read(cx).text()).into();
        self.center = note.path;
        self.center_buffer = note.buffer;
        self.fitted = false;
        self.hovered = None;
        self.rebuild_graph(cx);
    }

    /// Lists the project's notes and reads the links of those that changed
    /// since they were last read.
    fn refresh_index(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.index_task = cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Some((mut notes, to_read, fs)) = this
                .update(cx, |this, cx| {
                    let known: HashMap<&ProjectPath, &IndexedNote> =
                        this.notes.iter().map(|note| (&note.path, note)).collect();
                    let project = this.project.read(cx);
                    let mut notes = Vec::new();
                    let mut to_read = Vec::new();
                    for worktree in project.worktrees(cx) {
                        let worktree = worktree.read(cx);
                        for entry in worktree.entries(false, 0) {
                            let path = ProjectPath {
                                worktree_id: worktree.id(),
                                path: entry.path.clone(),
                            };
                            if !entry.is_file() || !is_note(&path) {
                                continue;
                            }
                            let unchanged = known
                                .get(&path)
                                .filter(|note| entry.mtime.is_some() && note.mtime == entry.mtime);
                            if unchanged.is_none() {
                                to_read.push(notes.len());
                            }
                            notes.push(IndexedNote {
                                absolute: worktree.absolutize(&entry.path),
                                mtime: entry.mtime,
                                links: unchanged.map(|note| note.links.clone()).unwrap_or_default(),
                                path,
                            });
                        }
                    }
                    // Only a local project's files can be read from here.
                    // A remote one still gets the center note's own links,
                    // which come from its buffer.
                    if !project.is_local() {
                        to_read.clear();
                    }
                    (notes, to_read, project.fs().clone())
                })
                .log_err()
            else {
                return;
            };

            let paths: Vec<(usize, PathBuf)> = to_read
                .into_iter()
                .map(|index| (index, notes[index].absolute.clone()))
                .collect();
            let read = cx
                .background_spawn(async move {
                    let mut read = Vec::with_capacity(paths.len());
                    for (index, path) in paths {
                        match fs.load(&path).await {
                            Ok(text) => read.push((index, extract_links(&text))),
                            Err(error) => {
                                log::debug!("note_graph: skipping {}: {error}", path.display())
                            }
                        }
                    }
                    read
                })
                .await;
            for (index, links) in read {
                notes[index].links = links.into();
            }
            this.update(cx, |this, cx| {
                this.notes = notes;
                this.indexed = true;
                this.rebuild_graph(cx);
            })
            .log_err();
        });
    }

    fn rebuild_graph(&mut self, cx: &mut Context<Self>) {
        let catalog = NoteCatalog::new(self.notes.iter().map(|note| {
            (
                note.path.path.as_unix_str().to_string(),
                note.absolute.clone(),
            )
        }));
        self.graph = LinkGraph::new(
            &catalog,
            self.notes.iter().map(|note| {
                if note.path == self.center {
                    &*self.center_links
                } else {
                    &*note.links
                }
            }),
        );
        self.rebuild_neighbourhood(cx);
    }

    fn rebuild_neighbourhood(&mut self, cx: &mut Context<Self>) {
        let shown = match self.notes.iter().position(|note| note.path == self.center) {
            Some(center) => Neighbourhood::around(&self.graph, center, self.hops, NOTE_LIMIT),
            None => Neighbourhood::default(),
        };
        if shown != self.shown {
            // A graph that had nothing but its center has no view worth
            // keeping, so fit to what just arrived.
            if self.shown.notes.len() <= 1 {
                self.fitted = false;
            }
            self.shown = shown;
            self.hovered = None;
        }
        cx.notify();
    }

    fn set_hops(&mut self, hops: usize, cx: &mut Context<Self>) {
        if hops != self.hops {
            self.hops = hops.clamp(1, MAX_HOPS);
            self.fitted = false;
            self.rebuild_neighbourhood(cx);
        }
    }

    fn note_name(&self, note: usize) -> SharedString {
        self.notes
            .get(note)
            .and_then(|note| note.path.path.file_stem())
            .unwrap_or_default()
            .to_string()
            .into()
    }

    fn viewport_size(&self) -> gpui::Size<f32> {
        let viewport = self.viewport.get().size;
        size(viewport.width.into(), viewport.height.into())
    }

    fn zoom_to_fit(&mut self) {
        let viewport = self.viewport_size();
        if viewport.width <= 0.0 || viewport.height <= 0.0 {
            return;
        }
        if let Some(content) = self.shown.bounds() {
            (self.zoom, self.offset) = link_index::fit(content, viewport, FIT_MARGIN);
            self.fitted = true;
        }
    }

    fn zoom_by(&mut self, factor: f32, anchor: Point<f32>) {
        (self.zoom, self.offset) =
            link_index::zoom_about(self.zoom, self.offset, self.zoom * factor, anchor);
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

    /// The shown note under a window position, as a position in `shown`.
    fn note_at(&self, position: Point<Pixels>) -> Option<usize> {
        let local = self.to_viewport(position);
        self.shown.note_at(point(
            (local.x - self.offset.x) / self.zoom,
            (local.y - self.offset.y) / self.zoom,
        ))
    }

    fn to_view_point(&self, position: Point<f32>) -> Point<f32> {
        point(
            position.x * self.zoom + self.offset.x,
            position.y * self.zoom + self.offset.y,
        )
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
            pressed_note: self.note_at(event.position),
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
            let hovered = self.note_at(event.position);
            if hovered != self.hovered {
                self.hovered = hovered;
                cx.notify();
            }
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
        if let Some(pressed) = drag.pressed_note
            && self.note_at(event.position) == Some(pressed)
        {
            self.open_note(pressed, window, cx);
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

    /// Opens a shown note where the center note is open, so the graph stays
    /// beside whatever is being read.
    fn open_note(&mut self, shown: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .shown
            .notes
            .get(shown)
            .and_then(|placed| self.notes.get(placed.note))
            .map(|note| note.path.clone())
        else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let own_pane = workspace.read(cx).pane_for_item_id(cx.entity_id());
        let panes = workspace.read(cx).panes();
        // The pane the note was in may have been closed since; any pane but
        // the graph's own keeps the graph in view.
        let pane = self
            .note_pane
            .upgrade()
            .filter(|pane| panes.contains(pane))
            .or_else(|| {
                panes
                    .iter()
                    .find(|pane| Some(*pane) != own_pane.as_ref())
                    .cloned()
            })
            .map(|pane| pane.downgrade());
        workspace.update(cx, |workspace, cx| {
            workspace
                .open_path(path, pane, true, window, cx)
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

    fn render_note(&self, shown: usize, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let placed = &self.shown.notes[shown];
        let is_center = placed.hops == 0;
        let is_hovered = self.hovered == Some(shown);
        let center = self.to_view_point(placed.position);
        let radius = DOT_RADIUS * self.zoom * if is_center { 1.5 } else { 1.0 };
        let width = LABEL_WIDTH * self.zoom;
        let font_size = 12.0 * self.zoom;
        let dot_color = if is_center || is_hovered {
            colors.text_accent
        } else if placed.hops == 1 {
            colors.text
        } else {
            colors.text_muted
        };
        // Too small to read, a label is only noise; the note under the
        // pointer and the center keep theirs at a readable size.
        let label = if is_hovered || is_center {
            Some(font_size.max(12.0))
        } else {
            (font_size >= 7.0).then_some(font_size)
        };

        div()
            .id(("note-graph-node", shown))
            .debug_selector(|| format!("note-graph-{}", self.note_name(placed.note)))
            .absolute()
            .left(px(center.x - width / 2.0))
            .top(px(center.y - radius))
            .w(px(width))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.0 * self.zoom))
            .cursor_pointer()
            .child(div().size(px(radius * 2.0)).rounded_full().bg(dot_color))
            .when_some(label, |this, font_size| {
                this.child(
                    div()
                        .text_size(px(font_size))
                        .line_height(px(LABEL_HEIGHT * font_size / 12.0))
                        .text_color(if is_center || is_hovered {
                            colors.text
                        } else {
                            colors.text_muted
                        })
                        .whitespace_nowrap()
                        .map(|this| {
                            if is_hovered {
                                // The full name, over whatever is beside it.
                                this.px_1()
                                    .rounded_sm()
                                    .bg(colors.elevated_surface_background)
                            } else {
                                this.max_w_full().overflow_hidden().text_ellipsis()
                            }
                        })
                        .child(self.note_name(placed.note)),
                )
            })
            .into_any_element()
    }
}

impl Focusable for GraphView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

struct PaintedLink {
    from: Point<f32>,
    to: Point<f32>,
    color: Hsla,
}

impl Render for GraphView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.fitted {
            self.zoom_to_fit();
        }
        let colors = cx.theme().colors();
        let viewport_size = self.viewport_size();
        let margin = LABEL_WIDTH * self.zoom;
        let is_visible = |position: Point<f32>| {
            position.x > -margin
                && position.y > -margin
                && position.x < viewport_size.width + margin
                && position.y < viewport_size.height + margin
        };

        let links: Vec<PaintedLink> = self
            .shown
            .links
            .iter()
            .filter_map(|(from, to)| {
                let touches_hovered = self.hovered == Some(*from) || self.hovered == Some(*to);
                let from = self.to_view_point(self.shown.notes.get(*from)?.position);
                let to = self.to_view_point(self.shown.notes.get(*to)?.position);
                Some(PaintedLink {
                    from,
                    to,
                    color: if touches_hovered {
                        colors.text_accent
                    } else {
                        colors.text_muted.opacity(0.4)
                    },
                })
            })
            .collect();

        let mut notes = Vec::new();
        for shown in 0..self.shown.notes.len() {
            let position = self.to_view_point(self.shown.notes[shown].position);
            if is_visible(position) && self.hovered != Some(shown) {
                notes.push(self.render_note(shown, cx));
            }
        }
        // Last, so its full name is drawn over its neighbours' labels.
        if let Some(hovered) = self
            .hovered
            .filter(|hovered| *hovered < self.shown.notes.len())
        {
            notes.push(self.render_note(hovered, cx));
        }

        let message = if !self.indexed {
            Some("Reading the project\u{2019}s notes\u{2026}".to_string())
        } else if self.shown.notes.is_empty() {
            Some("This note is not part of the project.".to_string())
        } else if self.shown.notes.len() == 1 {
            Some("No other note links to or from this one.".to_string())
        } else if self.shown.omitted > 0 {
            Some(format!(
                "Showing the nearest {} of {} linked notes.",
                self.shown.notes.len(),
                self.shown.notes.len() + self.shown.omitted
            ))
        } else {
            None
        };

        let viewport = self.viewport.clone();
        let view_id = cx.entity_id();
        let zoom = self.zoom;
        let zoom_percent = format!("{:.0}%", self.zoom * 100.0);

        div()
            .id("note-graph")
            .key_context("NoteGraph")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_to_fit_action))
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
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        // Notes are placed from the viewport's size, which
                        // is only known once laid out, so a change
                        // re-renders.
                        if viewport.get() != bounds {
                            viewport.set(bounds);
                            cx.notify(view_id);
                        }
                    },
                    move |bounds, _, window, _cx| {
                        let at = |position: Point<f32>| {
                            point(
                                bounds.origin.x + px(position.x),
                                bounds.origin.y + px(position.y),
                            )
                        };
                        for link in &links {
                            let mut stroke = PathBuilder::stroke(px(zoom.clamp(0.75, 2.0)));
                            stroke.move_to(at(link.from));
                            stroke.line_to(at(link.to));
                            if let Some(path) = stroke.build().log_err() {
                                window.paint_path(path, link.color);
                            }
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(notes)
            .when_some(message, |this, message| {
                this.child(
                    h_flex().absolute().top_2().left_2().child(
                        Label::new(message)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
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
                        Label::new("Depth")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .ml_1(),
                    )
                    .children((1..=MAX_HOPS).map(|hops| {
                        Button::new(("note-graph-depth", hops), hops.to_string())
                            .label_size(LabelSize::Small)
                            .toggle_state(self.hops == hops)
                            .tooltip(Tooltip::text(match hops {
                                1 => "Notes linked to or from this one",
                                2 => "And the notes linked to those",
                                _ => "And one link further",
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_hops(hops, cx)))
                    }))
                    .child(
                        IconButton::new("zoom-out", IconName::Dash)
                            .tooltip(Tooltip::text("Zoom Out"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_out(&ZoomOut, window, cx)
                            })),
                    )
                    .child(
                        Label::new(zoom_percent)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        IconButton::new("zoom-in", IconName::Plus)
                            .tooltip(Tooltip::text("Zoom In"))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.zoom_in(&ZoomIn, window, cx)
                                }),
                            ),
                    )
                    .child(
                        IconButton::new("zoom-to-fit", IconName::Maximize)
                            .tooltip(Tooltip::text("Zoom to Fit"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.zoom_to_fit_action(&ZoomToFit, window, cx)
                            })),
                    ),
            )
    }
}

impl Item for GraphView {
    type Event = GraphViewEvent;

    fn to_item_events(event: &Self::Event, _: &mut dyn FnMut(ItemEvent)) {
        match *event {}
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Local Graph".into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitGraph))
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(
            format!(
                "Notes linked to and from {}",
                self.center.path.as_unix_str()
            )
            .into(),
        )
    }
}

#[cfg(test)]
mod tests;
