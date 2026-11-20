//! Native live preview for Typst, driven by Tinymist's preview data plane.
//!
//! Tinymist compiles the document and streams a vector IR (the
//! [`reflexo`] "flat vector" format) over a WebSocket. This crate merges that
//! stream incrementally and converts pages into Vello scenes, which it
//! rasterizes with the GPU. The UI layer only ever sees pixels plus page
//! geometry, so it does not need the compiler, a PDF, or a webview.
//!
//! The protocol, IR, and scene construction are adapted from
//! [`tinymist-viewer`](https://github.com/Myriad-Dreamin/tinymist/tree/main/crates/tinymist-viewer)
//! at Tinymist 0.15.10 (Apache-2.0), minus its masonry/xilem shell and its
//! AccessKit bridge. The Vello rasterizer is adapted from that crate's renderer
//! test suite.
//!
//! # Integration surface
//!
//! Almost everything lives in this crate. The only hooks into Zed's own crates
//! are:
//!
//! - [`init`] registers the workspace actions; `crates/zed/src/main.rs` calls it.
//! - `crates/project_panel` asks [`is_typst_path`] before adding its context-menu
//!   entry, and dispatches [`OpenPreview`].
//! - `crates/zed/src/zed/quick_action_bar/preview.rs` asks [`is_typst_editor`]
//!   and calls [`open_preview_for_editor`].
//! - `Workspace::pane_for_lsp_jump`, `Workspace::pane_containing_project_path`,
//!   and `Workspace::open_abs_path_in_pane` keep language-server jumps next to
//!   the preview. They are generic and only used by
//!   `editor::items::handle_lsp_show_document`.
//! - `LspStore::watch_custom_notification` forwards the outline notification;
//!   this crate owns the method name and payload schema.
//! - `LspStore::ShowDocumentOrigin::Preview` marks a `window/showDocument` request
//!   that must not move the local editor (collaborative click-through), and
//!   `crates/editor/src/editor.rs` ignores those requests.
//! - `LspStore::register_command_handler` lets the host answer
//!   `suzuri.typstPreview.*` commands that arrive through the existing
//!   `ExecuteLspCommand` collab forwarding. This is the only transport the
//!   public collab server understands, so a guest works on zed.dev without a
//!   custom server. [collab] owns the protocol and the relay.
//!
//! Everything else — protocol, IR, rendering, session, tunneling, selection,
//! find, zoom, outline, word count, PDF export, and the host/guest relay — is
//! internal to the crate.

use std::{fmt, sync::Arc};

use ecow::EcoVec;
use reflexo::hash::Fingerprint;
use vello::kurbo::{self, Affine, Point, Rect};
use vello::peniko;

pub mod collab;
pub mod connection;
mod frame_timing;
pub mod incr;
pub mod links;
pub mod protocol;
mod raster;
mod render;
pub mod session;
pub mod start;
mod tunnel;
pub mod view;
pub mod word_count;

pub use raster::{RasterizedPage, SceneRasterizer};
pub use session::{PageMeta, PreviewSession, SessionEvent, SessionHandle};
pub use start::{init, is_typst_editor, open_preview_for_editor};
pub use view::{OpenPreview, TypstPreviewView, is_typst_path};
pub use word_count::{WordsCount, count_words};

/// A raster image format resolved for an SVG-linked image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgResourceFormat {
    /// JPEG image data.
    Jpeg,
    /// PNG image data.
    Png,
    /// GIF image data.
    Gif,
    /// WebP image data.
    Webp,
}

/// A raster image resolved from an SVG `<image href=...>` reference.
#[derive(Debug, Clone)]
pub struct SvgResource {
    /// The format of the encoded image data.
    pub format: SvgResourceFormat,
    /// The encoded image data.
    pub data: Arc<Vec<u8>>,
}

impl SvgResource {
    /// Creates a new resolved SVG image resource.
    pub fn new(format: SvgResourceFormat, data: impl Into<Vec<u8>>) -> Self {
        Self {
            format,
            data: Arc::new(data.into()),
        }
    }
}

/// Resolves image resources linked from inside SVG image data.
///
/// Vector documents only carry the encoded image bytes, so callers that still
/// know the original document or asset context can provide this hook to resolve
/// relative SVG image links.
pub trait SvgResourceResolver: Send + Sync {
    /// Resolves `href` from an SVG image.
    ///
    /// `svg_data` is the encoded SVG being parsed. Implementations can use it
    /// to recover a virtual base path for images that came from an asset store.
    fn resolve_svg_resource(&self, svg_data: &[u8], href: &str) -> Option<SvgResource>;
}

/// A vello scene corresponding to a typst page.
#[derive(Debug, Clone)]
pub struct VecPage {
    size: kurbo::Vec2,
    elem: Arc<VecScene>,
    accessibility: PageAccessibility,
    content_hash: Fingerprint,
}

impl VecPage {
    /// The page size in Typst page coordinates.
    pub fn size(&self) -> kurbo::Vec2 {
        self.size
    }

    /// The page scene, in page coordinates.
    pub fn scene(&self) -> &Arc<VecScene> {
        &self.elem
    }

    /// The page's links and selectable text.
    pub fn accessibility(&self) -> &PageAccessibility {
        &self.accessibility
    }

    /// The IR content hash this page was built from.
    ///
    /// Rasterized pages are reused while this hash and the size are unchanged.
    pub fn content_hash(&self) -> Fingerprint {
        self.content_hash
    }
}

/// A hyperlink on a rendered page, in page coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    href: String,
    bounds: Rect,
}

impl PageLink {
    /// Creates a page-coordinate link.
    pub fn new(href: impl Into<String>, bounds: Rect) -> Self {
        Self {
            href: href.into(),
            bounds,
        }
    }

    /// The link target.
    pub fn href(&self) -> &str {
        &self.href
    }

    /// The page-coordinate bounds of the link.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    fn transformed(&self, transform: Affine) -> Self {
        Self {
            href: self.href.clone(),
            bounds: transform_rect_bbox(transform, self.bounds),
        }
    }
}

/// Links and selectable text for a rendered page.
///
/// This is the data the preview pane needs for click-through and selection. It
/// intentionally carries no AccessKit nodes: wiring this into the platform
/// accessibility tree is a separate concern, and the renderer should not depend
/// on it.
#[derive(Debug, Clone, Default)]
pub struct PageAccessibility {
    links: EcoVec<PageLink>,
    text_runs: EcoVec<PageTextRun>,
}

impl PageAccessibility {
    /// Returns the page links in paint order.
    pub fn links(&self) -> &[PageLink] {
        &self.links
    }

    /// Returns selectable text runs in page coordinates.
    ///
    /// Runs include zero-width entries carrying content hints such as `\n`,
    /// which separate blocks in selections and copied text.
    pub fn text_runs(&self) -> &[PageTextRun] {
        &self.text_runs
    }

    /// Returns whether this page has neither links nor selectable text.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.text_runs.is_empty()
    }

    /// Returns the topmost link href at the provided page-coordinate point.
    pub fn hit_test_link(&self, point: Point) -> Option<&str> {
        self.links
            .iter()
            .rev()
            .find(|link| rect_contains(link.bounds, point))
            .map(|link| link.href.as_str())
    }

    /// Returns whether the provided page-coordinate point is over selectable text.
    pub fn hit_test_text(&self, point: Point) -> Option<PageTextPosition> {
        self.text_runs
            .iter()
            .enumerate()
            .rev()
            .find_map(|(run_index, run)| {
                run.character_index_at_point(point)
                    .map(|character_index| PageTextPosition {
                        run_index,
                        character_index,
                    })
            })
    }

    /// Returns text covered by a selection.
    pub fn selected_text(&self, selection: PageTextSelection) -> String {
        let Some((start, end)) = selection.normalized() else {
            return String::new();
        };
        if start.run_index >= self.text_runs.len()
            || end.run_index >= self.text_runs.len()
            || start == end
        {
            return String::new();
        }

        let mut selected = String::new();
        for run_index in start.run_index..=end.run_index {
            let run = &self.text_runs[run_index];
            let start_index = if run_index == start.run_index {
                start.character_index
            } else {
                0
            };
            let end_index = if run_index == end.run_index {
                end.character_index
            } else {
                run.character_count()
            };
            selected.push_str(run.text_slice(start_index, end_index));
        }
        selected
    }

    /// Returns page-coordinate rectangles covered by a text selection.
    pub fn selection_rects(&self, selection: PageTextSelection) -> Vec<Rect> {
        let Some((start, end)) = selection.normalized() else {
            return Vec::new();
        };
        if start.run_index >= self.text_runs.len()
            || end.run_index >= self.text_runs.len()
            || start == end
        {
            return Vec::new();
        }

        let mut rects = Vec::new();
        for run_index in start.run_index..=end.run_index {
            let run = &self.text_runs[run_index];
            let start_index = if run_index == start.run_index {
                start.character_index
            } else {
                0
            };
            let end_index = if run_index == end.run_index {
                end.character_index
            } else {
                run.character_count()
            };
            if let Some(rect) = run.selection_rect(start_index, end_index) {
                rects.push(rect);
            }
        }
        rects
    }

    pub(crate) fn push_link(&mut self, link: PageLink) {
        self.links.push(link);
    }

    pub(crate) fn push_text_run(&mut self, text_run: PageTextRun) {
        self.text_runs.push(text_run);
    }

    pub(crate) fn link_node(href: impl Into<String>, bounds: Rect) -> PageLink {
        PageLink::new(href, bounds)
    }
}

/// A selectable text run in page coordinates.
///
/// Zero-width runs are content hints (block separators). They contribute to
/// selection text but are never hit-tested.
#[derive(Debug, Clone, PartialEq)]
pub struct PageTextRun {
    text: String,
    bounds: Rect,
    character_bounds: EcoVec<Rect>,
}

impl PageTextRun {
    /// Creates a page text run from text and per-character page-coordinate boxes.
    pub fn new(
        text: impl Into<String>,
        bounds: Rect,
        character_bounds: impl IntoIterator<Item = Rect>,
    ) -> Self {
        let text = text.into();
        let character_count = text.chars().count();
        let mut character_bounds: EcoVec<_> = character_bounds.into_iter().collect();
        if character_count == 0 {
            character_bounds.clear();
        } else if character_bounds.len() != character_count {
            character_bounds = split_rect_horizontally(bounds, character_count);
        }

        Self {
            text,
            bounds,
            character_bounds,
        }
    }

    /// Returns the plain text for this run.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the page-coordinate run bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Returns page-coordinate per-character bounds.
    pub fn character_bounds(&self) -> &[Rect] {
        &self.character_bounds
    }

    /// Returns the number of selectable characters in this run.
    pub fn character_count(&self) -> usize {
        self.text.chars().count().min(self.character_bounds.len())
    }

    fn transformed(&self, transform: Affine) -> Self {
        Self {
            text: self.text.clone(),
            bounds: transform_rect_bbox(transform, self.bounds),
            character_bounds: self
                .character_bounds
                .iter()
                .map(|bounds| transform_rect_bbox(transform, *bounds))
                .collect(),
        }
    }

    fn character_index_at_point(&self, point: Point) -> Option<usize> {
        if !rect_contains(self.bounds, point) {
            return None;
        }

        let character_count = self.character_count();
        if character_count == 0 {
            return Some(0);
        }

        for (index, bounds) in self
            .character_bounds
            .iter()
            .take(character_count)
            .enumerate()
        {
            if point.x >= bounds.x0 && point.x < bounds.x1 {
                let midpoint = (bounds.x0 + bounds.x1) / 2.0;
                return Some(if point.x < midpoint { index } else { index + 1 });
            }
        }

        if point.x < self.character_bounds[0].x0 {
            Some(0)
        } else {
            Some(character_count)
        }
    }

    fn selection_rect(&self, start: usize, end: usize) -> Option<Rect> {
        let character_count = self.character_count();
        let start = start.min(character_count);
        let end = end.min(character_count);
        if start >= end {
            return None;
        }

        self.character_bounds
            .iter()
            .skip(start)
            .take(end - start)
            .copied()
            .reduce(rect_union)
    }

    fn text_slice(&self, start: usize, end: usize) -> &str {
        let character_count = self.character_count();
        let start = start.min(character_count);
        let end = end.min(character_count);
        if start >= end {
            return "";
        }

        let start_byte = char_to_byte_index(&self.text, start);
        let end_byte = char_to_byte_index(&self.text, end);
        &self.text[start_byte..end_byte]
    }
}

/// A position inside the page text semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageTextPosition {
    /// Index into [`PageAccessibility::text_runs`].
    pub run_index: usize,
    /// Character index inside the run.
    pub character_index: usize,
}

/// A text selection inside the page text semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTextSelection {
    /// Fixed end of the selection.
    pub anchor: PageTextPosition,
    /// Active end of the selection.
    pub focus: PageTextPosition,
}

impl PageTextSelection {
    /// Creates a collapsed selection at a text position.
    pub fn collapsed(position: PageTextPosition) -> Self {
        Self {
            anchor: position,
            focus: position,
        }
    }

    /// Returns whether the selection is collapsed.
    pub fn is_collapsed(self) -> bool {
        self.anchor == self.focus
    }

    fn normalized(self) -> Option<(PageTextPosition, PageTextPosition)> {
        if self.is_collapsed() {
            None
        } else if self.anchor <= self.focus {
            Some((self.anchor, self.focus))
        } else {
            Some((self.focus, self.anchor))
        }
    }
}

fn rect_contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.x0 && point.x < rect.x1 && point.y >= rect.y0 && point.y < rect.y1
}

fn rect_union(a: Rect, b: Rect) -> Rect {
    a.union(b)
}

fn split_rect_horizontally(rect: Rect, count: usize) -> EcoVec<Rect> {
    if count == 0 {
        return EcoVec::new();
    }

    let width = rect.width() / count as f64;
    (0..count)
        .map(|index| {
            let x0 = rect.x0 + width * index as f64;
            Rect::new(x0, rect.y0, x0 + width, rect.y1)
        })
        .collect()
}

fn char_to_byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(byte_index, _)| byte_index)
}

/// A scene that can be rendered to a [`vello::Scene`].
#[derive(Clone)]
pub enum VecScene {
    /// A group of scenes that are rendered together.
    Group(GroupScene),
    /// A path that is rendered to a scene.
    Path(kurbo::BezPath, peniko::Color),
    /// A scene that is rendered to a scene.
    Scene(Box<vello::Scene>, Option<kurbo::Affine>),
}

impl fmt::Debug for VecScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VecScene::Group(..) => write!(f, "GroupScene"),
            VecScene::Path(..) => write!(f, "Path"),
            VecScene::Scene(..) => write!(f, "Scene"),
        }
    }
}

impl VecScene {
    /// Renders the scene to a [`vello::Scene`].
    pub fn render(&self, scene: &mut vello::Scene) {
        match self {
            VecScene::Group(group) => group.render(scene),
            VecScene::Path(path, color) => {
                if !path_is_renderable(path) {
                    return;
                }
                scene.fill(
                    peniko::Fill::NonZero,
                    Affine::IDENTITY,
                    &peniko::Brush::Solid(*color),
                    None,
                    path,
                );
            }
            VecScene::Scene(sub, transform) => scene.append(sub, *transform),
        }
    }

    /// Collects links and text runs in page coordinates.
    pub fn page_accessibility(&self) -> PageAccessibility {
        let mut accessibility = PageAccessibility::default();
        self.collect_accessibility(Affine::IDENTITY, &mut accessibility);
        accessibility
    }

    fn collect_accessibility(&self, transform: Affine, accessibility: &mut PageAccessibility) {
        match self {
            VecScene::Group(group) => group.collect_accessibility(transform, accessibility),
            VecScene::Path(..) | VecScene::Scene(..) => {}
        }
    }
}

/// A group of scenes that are rendered together.
#[derive(Debug, Clone)]
pub struct GroupScene {
    clip: Option<kurbo::BezPath>,
    ts: Affine,
    items: EcoVec<GroupSceneItem>,
}

/// An item inside a [`GroupScene`], ordered as emitted by the renderer.
#[derive(Debug, Clone)]
pub enum GroupSceneItem {
    /// A visual scene drawn at the given group-local position.
    Scene {
        /// The group-local position of the scene.
        pos: kurbo::Vec2,
        /// The scene to render.
        scene: Arc<VecScene>,
    },
    /// A link associated with the group.
    Accessibility(PageLink),
    /// A selectable text run associated with the group.
    Text(PageTextRun),
}

impl GroupScene {
    /// Renders the group to a [`vello::Scene`].
    pub fn render(&self, scene: &mut vello::Scene) {
        // A group whose transform collapses space paints nothing. Typst allows
        // zero-sized text (`#set text(size: 0pt)`), whose text group is scaled
        // by the font size: `0`. Feeding those collapsed glyph outlines to
        // Vello makes its flattener normalize a zero-length tangent and panic
        // on the resulting NaN, which used to kill the whole preview session.
        if !transform_paints(self.ts) {
            return;
        }
        if let Some(clip) = &self.clip {
            scene.push_clip_layer(peniko::Fill::NonZero, self.ts, clip);
        }
        let ts = self.ts;
        for item in self.items.iter() {
            if let GroupSceneItem::Scene {
                pos,
                scene: child_scene,
            } = item
            {
                let ts = ts.pre_translate(*pos);
                let mut sub_scene = vello::Scene::new();
                child_scene.render(&mut sub_scene);
                scene.append(&sub_scene, Some(ts));
            }
        }
        if self.clip.is_some() {
            scene.pop_layer();
        }
    }

    fn collect_accessibility(&self, transform: Affine, accessibility: &mut PageAccessibility) {
        let transform = transform * self.ts;
        for item in self.items.iter() {
            match item {
                GroupSceneItem::Scene {
                    pos,
                    scene: child_scene,
                } => {
                    child_scene.collect_accessibility(transform.pre_translate(*pos), accessibility);
                }
                GroupSceneItem::Accessibility(link) => {
                    accessibility.push_link(link.transformed(transform));
                }
                GroupSceneItem::Text(text_run) => {
                    accessibility.push_text_run(text_run.transformed(transform));
                }
            }
        }
    }
}

/// Whether a group transform can paint anything at all.
///
/// A singular transform (zero scale, as produced for `0pt` text) or one with
/// non-finite coefficients collapses every child into a point. Vello cannot
/// flatten those degenerately, so the subtree is skipped instead.
fn transform_paints(ts: Affine) -> bool {
    const MIN_DETERMINANT: f64 = 1e-12;
    let [a, b, c, d, e, f] = ts.as_coeffs();
    [a, b, c, d, e, f].iter().all(|value| value.is_finite())
        && ts.determinant().abs() > MIN_DETERMINANT
}

/// Whether a path has finite coordinates. Vello's flattener assumes them.
fn path_is_renderable(path: &kurbo::BezPath) -> bool {
    path.elements().iter().all(|element| match element {
        kurbo::PathEl::MoveTo(point) | kurbo::PathEl::LineTo(point) => point_is_finite(*point),
        kurbo::PathEl::QuadTo(control, point) => {
            point_is_finite(*control) && point_is_finite(*point)
        }
        kurbo::PathEl::CurveTo(control1, control2, point) => {
            point_is_finite(*control1) && point_is_finite(*control2) && point_is_finite(*point)
        }
        kurbo::PathEl::ClosePath => true,
    })
}

fn point_is_finite(point: Point) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn transform_rect_bbox(transform: Affine, rect: Rect) -> Rect {
    let points = [
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y0),
        Point::new(rect.x1, rect.y1),
        Point::new(rect.x0, rect.y1),
    ]
    .map(|point| transform * point);

    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for point in points {
        x0 = x0.min(point.x);
        y0 = y0.min(point.y);
        x1 = x1.max(point.x);
        y1 = y1.max(point.y);
    }

    Rect::new(x0, y0, x1, y1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect::new(x0, y0, x1, y1)
    }

    fn square_scene() -> VecScene {
        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((10.0, 0.0));
        path.line_to((10.0, 10.0));
        path.close_path();

        let mut inner = EcoVec::new();
        inner.push(GroupSceneItem::Scene {
            pos: kurbo::Vec2::ZERO,
            scene: Arc::new(VecScene::Path(path, peniko::Color::BLACK)),
        });
        let mut outer = EcoVec::new();
        outer.push(GroupSceneItem::Scene {
            pos: kurbo::Vec2::ZERO,
            scene: Arc::new(VecScene::Group(GroupScene {
                clip: None,
                ts: Affine::IDENTITY,
                items: inner,
            })),
        });
        VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::IDENTITY,
            items: outer,
        })
    }

    /// `#set text(size: 0pt)` makes Tinymist emit a text group whose transform
    /// scales by zero. Vello panics flattening the collapsed outlines
    /// (`assertion failed: !n.is_nan()`), so such groups must never reach it.
    #[test]
    fn zero_scale_groups_render_nothing() {
        let mut scene = square_scene();
        let VecScene::Group(group) = &mut scene else {
            unreachable!()
        };
        group.ts = Affine::scale(0.0);

        let mut vello_scene = vello::Scene::new();
        scene.render(&mut vello_scene);
        assert_eq!(
            vello_scene.encoding().n_paths,
            0,
            "a zero-scale group must not emit geometry"
        );

        // The same scene at a real scale still paints.
        let scene = square_scene();
        let mut vello_scene = vello::Scene::new();
        scene.render(&mut vello_scene);
        assert_eq!(vello_scene.encoding().n_paths, 1);
    }

    #[test]
    fn degenerate_transforms_do_not_paint() {
        assert!(!transform_paints(Affine::scale(0.0)));
        assert!(!transform_paints(Affine::scale_non_uniform(0.0, 1.0)));
        assert!(!transform_paints(Affine::new([
            f64::NAN,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
        ])));
        assert!(transform_paints(Affine::scale(0.001)));
    }

    #[test]
    fn non_finite_paths_are_not_rendered() {
        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((f64::NAN, 1.0));
        assert!(!path_is_renderable(&path));

        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((1.0, 1.0));
        assert!(path_is_renderable(&path));
    }

    #[test]
    fn page_accessibility_hit_tests_topmost_link() {
        let mut accessibility = PageAccessibility::default();
        accessibility.push_link(PageAccessibility::link_node(
            "https://bottom.example",
            rect(0.0, 0.0, 20.0, 20.0),
        ));
        accessibility.push_link(PageAccessibility::link_node(
            "https://top.example",
            rect(10.0, 10.0, 30.0, 30.0),
        ));

        assert_eq!(
            accessibility.hit_test_link(Point::new(12.0, 12.0)),
            Some("https://top.example")
        );
        assert_eq!(
            accessibility.hit_test_link(Point::new(5.0, 5.0)),
            Some("https://bottom.example")
        );
        assert!(
            accessibility
                .hit_test_link(Point::new(40.0, 40.0))
                .is_none()
        );
    }

    #[test]
    fn page_accessibility_collects_transformed_nested_links() {
        let mut child_items = EcoVec::new();
        child_items.push(GroupSceneItem::Accessibility(PageAccessibility::link_node(
            "https://example.com",
            rect(0.0, 0.0, 5.0, 10.0),
        )));
        let child = Arc::new(VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::scale(2.0),
            items: child_items,
        }));

        let mut items = EcoVec::new();
        items.push(GroupSceneItem::Scene {
            pos: kurbo::Vec2::new(10.0, 20.0),
            scene: child,
        });
        let scene = VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::IDENTITY,
            items,
        });

        let accessibility = scene.page_accessibility();
        let [link] = accessibility.links() else {
            panic!("expected one link");
        };
        assert_eq!(link.href(), "https://example.com");
        assert_eq!(link.bounds(), rect(10.0, 20.0, 20.0, 40.0));
        assert!(
            accessibility
                .hit_test_link(Point::new(15.0, 30.0))
                .is_some()
        );
        assert!(
            accessibility
                .hit_test_link(Point::new(25.0, 30.0))
                .is_none()
        );
    }

    #[test]
    fn page_accessibility_hit_tests_links_in_group_item_order() {
        let mut child_items = EcoVec::new();
        child_items.push(GroupSceneItem::Accessibility(PageAccessibility::link_node(
            "https://bottom.example",
            rect(0.0, 0.0, 20.0, 20.0),
        )));
        let child = Arc::new(VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::IDENTITY,
            items: child_items,
        }));

        let mut items = EcoVec::new();
        items.push(GroupSceneItem::Scene {
            pos: kurbo::Vec2::ZERO,
            scene: child,
        });
        items.push(GroupSceneItem::Accessibility(PageAccessibility::link_node(
            "https://top.example",
            rect(0.0, 0.0, 20.0, 20.0),
        )));
        let scene = VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::IDENTITY,
            items,
        });

        assert_eq!(
            scene
                .page_accessibility()
                .hit_test_link(Point::new(10.0, 10.0)),
            Some("https://top.example")
        );
    }

    #[test]
    fn page_accessibility_collects_transformed_text_runs() {
        let text_run = PageTextRun::new(
            "hi",
            rect(0.0, 0.0, 20.0, 10.0),
            [rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 0.0, 20.0, 10.0)],
        );

        let mut items = EcoVec::new();
        items.push(GroupSceneItem::Text(text_run));
        let scene = VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::translate((5.0, 10.0)) * Affine::scale(2.0),
            items,
        });

        let accessibility = scene.page_accessibility();
        let [run] = accessibility.text_runs() else {
            panic!("expected one text run");
        };
        assert_eq!(run.text(), "hi");
        assert_eq!(run.bounds(), rect(5.0, 10.0, 45.0, 30.0));
        assert_eq!(
            run.character_bounds(),
            &[rect(5.0, 10.0, 25.0, 30.0), rect(25.0, 10.0, 45.0, 30.0)]
        );
    }

    #[test]
    fn page_accessibility_keeps_content_hints_in_selection_text() {
        let mut items = EcoVec::new();
        items.push(GroupSceneItem::Text(PageTextRun::new(
            "hello",
            rect(0.0, 20.0, 50.0, 30.0),
            [],
        )));
        items.push(GroupSceneItem::Text(PageTextRun::new(
            "\n",
            rect(0.0, 0.0, 0.0, 0.0),
            [],
        )));
        items.push(GroupSceneItem::Text(PageTextRun::new(
            "world",
            rect(0.0, 40.0, 50.0, 50.0),
            [],
        )));
        let scene = VecScene::Group(GroupScene {
            clip: None,
            ts: Affine::IDENTITY,
            items,
        });

        let accessibility = scene.page_accessibility();
        let runs = accessibility.text_runs();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].text(), "\n");
        assert_eq!(runs[1].character_bounds(), &[rect(0.0, 0.0, 0.0, 0.0)]);

        let selection = PageTextSelection {
            anchor: PageTextPosition {
                run_index: 0,
                character_index: 0,
            },
            focus: PageTextPosition {
                run_index: 2,
                character_index: 5,
            },
        };
        let selected = accessibility.selected_text(selection);
        assert_eq!(selected, "hello\nworld");
        assert_eq!(crate::word_count::count_words(&selected).words, 2);

        // The zero-width hint itself is not a click target.
        assert!(accessibility.hit_test_text(Point::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn page_accessibility_selects_text_by_page_point() {
        let mut accessibility = PageAccessibility::default();
        accessibility.push_text_run(PageTextRun::new(
            "hello",
            rect(0.0, 0.0, 50.0, 10.0),
            (0..5).map(|index| {
                let x0 = index as f64 * 10.0;
                rect(x0, 0.0, x0 + 10.0, 10.0)
            }),
        ));

        let anchor = accessibility
            .hit_test_text(Point::new(1.0, 5.0))
            .expect("left edge should hit text");
        let focus = accessibility
            .hit_test_text(Point::new(49.0, 5.0))
            .expect("right edge should hit text");
        let selection = PageTextSelection { anchor, focus };

        assert_eq!(accessibility.selected_text(selection), "hello");
        assert_eq!(
            accessibility.selection_rects(selection),
            [rect(0.0, 0.0, 50.0, 10.0)]
        );
    }
}
