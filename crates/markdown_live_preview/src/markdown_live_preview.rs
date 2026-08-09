//! Obsidian-style live preview for markdown buffers.
//!
//! When enabled, markdown syntax markers (`**`, `*`, `~~`, backticks, link
//! targets, list bullets) are hidden and rendered inline, and block elements
//! (headings, tables, images, mermaid diagrams, horizontal rules) are replaced
//! with rendered widgets. Raw markdown is revealed for editing per token: an
//! inline construct reveals when the selection touches it, and a block
//! reveals when the selection reaches its lines, mirroring Obsidian's Live
//! Preview mode.

use std::{any::TypeId, ops::Range, path::PathBuf, sync::Arc};

use collections::{HashMap, HashSet};
use editor::{
    Addon, Editor, EditorEvent, FoldPlaceholder, HighlightKey,
    display_map::{
        BlockPlacement, BlockProperties, BlockStyle, Concealment, CustomBlockId, RenderBlock,
    },
};
use gpui::{
    App, AppContext as _, Context, Empty, Entity, FontWeight, HighlightStyle, ImageSource,
    IntoElement, MouseButton, Resource, SharedString, SharedUri, StrikethroughStyle, Subscription,
    TextStyleRefinement, WeakEntity, Window, actions, rems,
};
use language::LanguageName;
use markdown::{HeadingLevelStyles, Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use multi_buffer::{
    Anchor, MultiBufferOffset, MultiBufferRow, MultiBufferSnapshot, ToOffset as _, ToPoint as _,
};
use settings::{RegisterSetting, Settings};
use text::Point;
use ui::{Checkbox, ToggleState, prelude::*};
use util::ResultExt as _;

actions!(
    markdown,
    [
        /// Toggles Obsidian-style live preview rendering in the current markdown buffer.
        ToggleLivePreview
    ]
);

/// Type tag used to scope this crate's folds so they can be added and removed
/// without disturbing user folds or other fold consumers.
struct LivePreviewFoldTag;

const MARKDOWN: &str = "Markdown";
const MARKDOWN_INLINE: &str = "Markdown-Inline";

#[derive(Clone, Copy, Debug, Default, RegisterSetting)]
pub struct MarkdownLivePreviewSettings {
    pub enabled: bool,
}

impl Settings for MarkdownLivePreviewSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.markdown_live_preview.clone().unwrap_or_default();
        Self {
            enabled: content.enabled.unwrap_or(true),
        }
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(register_editor).detach();
}

fn register_editor(editor: &mut Editor, window: Option<&mut Window>, cx: &mut Context<Editor>) {
    let Some(window) = window else {
        return;
    };
    if !editor.mode().is_full() {
        return;
    }

    let mut subscriptions = Vec::new();
    subscriptions.push(cx.subscribe_self(|editor, event: &EditorEvent, cx| match event {
        EditorEvent::Reparsed(_) => recompute(editor, cx),
        EditorEvent::SelectionsChanged { .. } => {
            if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                let resizing = addon
                    .last_resize_at
                    .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(500));
                if !resizing {
                    addon.selected_image = None;
                }
            }
            apply_decorations(editor, cx);
        }
        _ => {}
    }));

    subscriptions.push(cx.observe_global::<theme::GlobalTheme>(|editor, cx| {
        let markers = editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.markers.clone());
        apply_emphasis_highlights(editor, markers.as_deref(), cx);
    }));

    let weak_editor = cx.weak_entity();
    subscriptions.push(
        editor.register_action::<ToggleLivePreview>(move |_, _window, cx| {
            weak_editor
                .update(cx, |editor, cx| {
                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                        let enabled = addon.enabled_override.unwrap_or_else(|| {
                            MarkdownLivePreviewSettings::get_global(cx).enabled
                        });
                        addon.enabled_override = Some(!enabled);
                    }
                    recompute(editor, cx);
                })
                .log_err();
        }),
    );

    editor.register_addon(LivePreviewAddon {
        enabled_override: None,
        markers: None,
        applied_blocks: Vec::new(),
        selected_image: None,
        last_resize_at: None,
        _subscriptions: subscriptions,
    });

    // The buffer may already be parsed by the time this editor is created, in
    // which case no `Reparsed` event will arrive; compute an initial pass.
    let weak_editor = cx.weak_entity();
    window.defer(cx, move |_window, cx| {
        weak_editor
            .update(cx, |editor, cx| recompute(editor, cx))
            .ok();
    });
}

struct LivePreviewAddon {
    /// Per-editor override set by the toggle action; falls back to the setting.
    enabled_override: Option<bool>,
    markers: Option<Arc<MarkerSet>>,
    applied_blocks: Vec<AppliedBlock>,
    /// The image widget currently selected (Obsidian-style click state),
    /// identified by its marker range.
    selected_image: Option<Range<Anchor>>,
    /// When a resize drag last wrote a width, so selection isn't cleared by
    /// the selection refresh that buffer edits trigger mid-drag.
    last_resize_at: Option<std::time::Instant>,
    _subscriptions: Vec<Subscription>,
}

impl Addon for LivePreviewAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

struct MarkerSet {
    inline: Vec<InlineMarker>,
    blocks: Vec<BlockMarker>,
    /// Ranges that get an always-on strikethrough text decoration: themes
    /// color `~~struck~~` spans but do not apply the actual line-through, and
    /// with the delimiters hidden there would otherwise be no visual cue.
    strikethrough: Vec<Range<Anchor>>,
    /// Emphasis content, restyled preview-like (plain text color, true
    /// italic/bold) instead of source-mode syntax-highlight colors.
    italic: Vec<Range<Anchor>>,
    bold: Vec<Range<Anchor>>,
    /// Link text, restyled to upright accent color so links read as
    /// clickable color while italics remain the only slanted text.
    link_text: Vec<Range<Anchor>>,
    /// All `[label]: url` reference definitions in the document, appended to
    /// each widget's mini-document so reference links and images resolve.
    definitions: String,
    /// Definition lines are muted: the preview hides them entirely, but
    /// invisible text is confusing in an editor, so they recede instead.
    definition_ranges: Vec<Range<Anchor>>,
    /// Ordered-list markers, restyled to the plain text color for
    /// consistency with bullet glyphs.
    ordered_markers: Vec<Range<Anchor>>,
}

#[derive(Clone)]
struct InlineMarker {
    range: Range<Anchor>,
    kind: InlineKind,
}

#[derive(Clone)]
enum InlineKind {
    /// Pure syntax to hide: emphasis delimiters, backticks, link brackets and
    /// destinations, etc. Reveals when the selection touches the enclosing
    /// construct (per-token reveal), not the whole line.
    Hide {
        /// The whole construct (e.g. `**bold**` including delimiters); the
        /// marker reveals when the selection touches this span.
        reveal_span: Range<Anchor>,
    },
    /// An unordered list marker, rendered as a bullet glyph.
    Bullet,
    /// A task list marker (`- [ ]` / `- [x]`), rendered as a clickable checkbox.
    Checkbox {
        checked: bool,
        /// The range of the `[ ]`/`[x]` marker itself, edited on toggle.
        marker_range: Range<Anchor>,
    },
}

struct BlockMarker {
    range: Range<Anchor>,
    height_estimate: u32,
    kind: BlockRenderKind,
    /// Leading-whitespace columns of the first line, so nested widgets (e.g.
    /// a code block inside a list item) keep their indentation.
    indent_columns: u32,
}

#[derive(Clone, PartialEq)]
enum BlockRenderKind {
    /// Rendered through `MarkdownElement`.
    Markdown,
    /// A horizontal rule, rendered as a plain divider: a lone `---` fed to
    /// the markdown parser would be misread as a frontmatter opener.
    Rule,
    /// YAML/TOML frontmatter, rendered as a compact properties card instead
    /// of the markdown crate's oversized metadata table.
    Frontmatter,
    /// A standalone image, width-capped and honoring Obsidian's
    /// `![alt|640](path)` size syntax. When the destination is known the
    /// widget renders a bare image element (which the selection border hugs
    /// exactly); reference-style images fall back to the markdown renderer.
    Image {
        display_width: Option<f32>,
        destination: Option<String>,
        alt: String,
    },
}

struct AppliedBlock {
    range: Range<Anchor>,
    source: String,
    block_id: CustomBlockId,
}

fn is_enabled(addon: &LivePreviewAddon, cx: &App) -> bool {
    addon
        .enabled_override
        .unwrap_or_else(|| MarkdownLivePreviewSettings::get_global(cx).enabled)
}

fn recompute(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<LivePreviewAddon>() else {
        return;
    };
    let enabled = is_enabled(addon, cx);

    let markers = if enabled && !editor.read_only(cx) {
        extract_markers(editor, cx).map(Arc::new)
    } else {
        None
    };

    let Some(addon) = editor.addon_mut::<LivePreviewAddon>() else {
        return;
    };
    addon.markers = markers.clone();
    apply_emphasis_highlights(editor, markers.as_deref(), cx);
    apply_decorations(editor, cx);
}

/// Emphasis spans get preview-like typography: the plain text color with true
/// bold/italic styling, overriding the theme's source-mode markup colors
/// (e.g. blue non-slanted italics, orange bold), plus a real line-through for
/// strikethrough, which themes color but never strike.
fn apply_emphasis_highlights(editor: &mut Editor, markers: Option<&MarkerSet>, cx: &mut Context<Editor>) {
    const STRIKE: usize = 0;
    const ITALIC: usize = 1;
    const BOLD: usize = 2;
    const LINK: usize = 3;
    const DEFINITION: usize = 4;
    const ORDERED_MARKER: usize = 5;
    let text_color = cx.theme().colors().text;
    let accent_color = cx.theme().colors().text_accent;
    let muted_color = cx.theme().colors().text_muted;
    let sets = [
        (
            STRIKE,
            markers.map(|markers| markers.strikethrough.clone()),
            HighlightStyle {
                strikethrough: Some(StrikethroughStyle {
                    thickness: gpui::px(1.),
                    color: None,
                }),
                ..Default::default()
            },
        ),
        (
            ITALIC,
            markers.map(|markers| markers.italic.clone()),
            HighlightStyle {
                color: Some(text_color),
                font_style: Some(gpui::FontStyle::Italic),
                ..Default::default()
            },
        ),
        (
            BOLD,
            markers.map(|markers| markers.bold.clone()),
            HighlightStyle {
                color: Some(text_color),
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        ),
        (
            LINK,
            markers.map(|markers| markers.link_text.clone()),
            HighlightStyle {
                color: Some(accent_color),
                font_style: Some(gpui::FontStyle::Normal),
                ..Default::default()
            },
        ),
        (
            DEFINITION,
            markers.map(|markers| markers.definition_ranges.clone()),
            HighlightStyle {
                color: Some(muted_color),
                font_style: Some(gpui::FontStyle::Normal),
                ..Default::default()
            },
        ),
        (
            ORDERED_MARKER,
            markers.map(|markers| markers.ordered_markers.clone()),
            HighlightStyle {
                color: Some(text_color),
                ..Default::default()
            },
        ),
    ];
    for (key, ranges, style) in sets {
        match ranges {
            Some(ranges) if !ranges.is_empty() => {
                editor.highlight_text(HighlightKey::MarkdownLivePreview(key), ranges, style, cx);
            }
            _ => editor.clear_highlights(HighlightKey::MarkdownLivePreview(key), cx),
        }
    }
}

fn apply_decorations(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon_mut::<LivePreviewAddon>() else {
        return;
    };
    let markers = addon.markers.clone();
    let applied_blocks = std::mem::take(&mut addon.applied_blocks);

    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let Some(markers) = markers else {
        clear_decorations(editor, applied_blocks, cx);
        return;
    };

    // Session restore can resurrect concealments saved as folds by older
    // builds as plain `⋯` folds this addon does not own; heal them whenever
    // decorations refresh.
    remove_stale_restored_folds(editor, cx);

    let selection_rows = selection_row_ranges(editor, &snapshot);
    let selection_offsets = selection_offset_ranges(editor, &snapshot);

    // --- Inline concealments ---

    let weak_editor = cx.weak_entity();
    let mut concealments = Vec::new();
    for marker in &markers.inline {
        let start = marker.range.start.to_point(&snapshot);
        let end = marker.range.end.to_point(&snapshot);
        if start >= end {
            continue;
        }
        // Per-token: reveal only when the selection touches the marker's
        // enclosing construct (for list markers, the marker itself), leaving
        // the rest of the line rendered.
        let reveal_span = match &marker.kind {
            InlineKind::Hide { reveal_span } => reveal_span,
            InlineKind::Bullet | InlineKind::Checkbox { .. } => &marker.range,
        };
        let span =
            reveal_span.start.to_offset(&snapshot).0..reveal_span.end.to_offset(&snapshot).0;
        let revealed = selection_offsets
            .iter()
            .any(|selection| selection.start <= span.end && span.start <= selection.end);
        if revealed {
            continue;
        }
        concealments.push(Concealment {
            range: marker.range.clone(),
            placeholder: fold_placeholder(marker, weak_editor.clone()),
            content_key: marker_content_key(&marker.kind),
        });
    }
    editor.set_concealments(TypeId::of::<LivePreviewFoldTag>(), concealments, cx);

    // --- Block widgets ---

    let mut desired_blocks: HashMap<(usize, usize), (&BlockMarker, String)> = HashMap::default();
    for marker in &markers.blocks {
        let start = marker.range.start.to_point(&snapshot);
        let end = marker.range.end.to_point(&snapshot);
        if start > end {
            continue;
        }
        if rows_intersect(&selection_rows, start.row, end.row) {
            continue;
        }
        let start_offset = marker.range.start.to_offset(&snapshot);
        let end_offset = marker.range.end.to_offset(&snapshot);
        let mut source: String = snapshot.text_for_range(start_offset..end_offset).collect();
        if source.trim().is_empty() {
            continue;
        }
        // Reference links/images inside a widget resolve against the whole
        // document's definitions, which live outside the widget's slice.
        if matches!(
            marker.kind,
            BlockRenderKind::Markdown | BlockRenderKind::Image { .. }
        ) && !markers.definitions.is_empty()
        {
            source.push_str("\n\n");
            source.push_str(&markers.definitions);
        }
        desired_blocks.insert((start_offset.0, end_offset.0), (marker, source));
    }

    let mut new_applied_blocks = Vec::new();
    let mut block_ids_to_remove = HashSet::default();
    for applied in applied_blocks {
        let start = applied.range.start.to_offset(&snapshot).0;
        let end = applied.range.end.to_offset(&snapshot).0;
        let keep = desired_blocks
            .get(&(start, end))
            .is_some_and(|(_, source)| *source == applied.source);
        if keep {
            desired_blocks.remove(&(start, end));
            new_applied_blocks.push(applied);
        } else {
            block_ids_to_remove.insert(applied.block_id);
        }
    }

    let mut blocks_to_insert = Vec::new();
    let mut pending_applied = Vec::new();
    let base_directory = buffer_base_directory(editor, cx);
    // The language registry lets rendered code blocks (and code spans in
    // tables/quotes) get syntax highlighting.
    let language_registry = editor
        .buffer()
        .read(cx)
        .as_singleton()
        .and_then(|buffer| buffer.read(cx).language_registry());
    for (marker, source) in desired_blocks.into_values() {
        let render = match &marker.kind {
            BlockRenderKind::Markdown => {
                let markdown = cx.new(|cx| {
                    Markdown::new_with_options(
                        SharedString::from(source.clone()),
                        language_registry.clone(),
                        None,
                        markdown::MarkdownOptions {
                            parse_html: true,
                            render_mermaid_diagrams: true,
                            ..Default::default()
                        },
                        cx,
                    )
                });
                render_markdown_block(
                    markdown,
                    weak_editor.clone(),
                    marker.range.clone(),
                    base_directory.clone(),
                    marker.indent_columns,
                )
            }
            BlockRenderKind::Image {
                display_width,
                destination,
                alt,
            } => {
                let markdown = cx.new(|cx| {
                    Markdown::new_with_options(
                        SharedString::from(source.clone()),
                        language_registry.clone(),
                        None,
                        markdown::MarkdownOptions {
                            parse_html: true,
                            render_mermaid_diagrams: true,
                            ..Default::default()
                        },
                        cx,
                    )
                });
                render_image_block(
                    markdown,
                    weak_editor.clone(),
                    marker.range.clone(),
                    base_directory.clone(),
                    marker.indent_columns,
                    *display_width,
                    destination.clone(),
                    SharedString::from(alt.clone()),
                )
            }
            BlockRenderKind::Rule => {
                render_rule_block(weak_editor.clone(), marker.range.clone(), marker.indent_columns)
            }
            BlockRenderKind::Frontmatter => {
                render_frontmatter_block(weak_editor.clone(), marker.range.clone(), source.clone())
            }

        };
        blocks_to_insert.push(BlockProperties {
            placement: BlockPlacement::Replace(marker.range.start..=marker.range.end),
            height: Some(marker.height_estimate),
            style: BlockStyle::Flex,
            render,
            priority: 0,
        });
        pending_applied.push((marker.range.clone(), source));
    }

    if !block_ids_to_remove.is_empty() {
        editor.remove_blocks(block_ids_to_remove, None, cx);
    }
    if !blocks_to_insert.is_empty() {
        let block_ids = editor.insert_blocks(blocks_to_insert, None, cx);
        for ((range, source), block_id) in pending_applied.into_iter().zip(block_ids) {
            new_applied_blocks.push(AppliedBlock {
                range,
                source,
                block_id,
            });
        }
    }

    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
        addon.applied_blocks = new_applied_blocks;
    }
}

/// Sessions saved before concealment folds were excluded from persistence
/// restore them as plain `⋯` folds this addon does not own; remove any
/// untagged fold that sits exactly on a marker range.
fn remove_stale_restored_folds(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(markers) = editor
        .addon::<LivePreviewAddon>()
        .and_then(|addon| addon.markers.clone())
    else {
        return;
    };
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let marker_offsets: HashSet<(usize, usize)> = markers
        .inline
        .iter()
        .map(|marker| {
            (
                marker.range.start.to_offset(&snapshot).0,
                marker.range.end.to_offset(&snapshot).0,
            )
        })
        .collect();

    let display_snapshot = editor.display_snapshot(cx);
    let stale: Vec<Range<MultiBufferOffset>> = display_snapshot
        .folds_in_range(MultiBufferOffset(0)..snapshot.len())
        .filter(|fold| fold.placeholder.type_tag.is_none())
        .filter_map(|fold| {
            let start = fold.range.start.to_offset(&snapshot);
            let end = fold.range.end.to_offset(&snapshot);
            marker_offsets
                .contains(&(start.0, end.0))
                .then_some(start..end)
        })
        .collect();
    if !stale.is_empty() {
        editor.unfold_ranges(&stale, false, false, cx);
    }
}

fn clear_decorations(
    editor: &mut Editor,
    applied_blocks: Vec<AppliedBlock>,
    cx: &mut Context<Editor>,
) {
    editor.set_concealments(TypeId::of::<LivePreviewFoldTag>(), Vec::new(), cx);
    if !applied_blocks.is_empty() {
        let block_ids = applied_blocks
            .into_iter()
            .map(|block| block.block_id)
            .collect();
        editor.remove_blocks(block_ids, None, cx);
    }
}

/// Inclusive row ranges covered by the current selections.
fn selection_row_ranges(editor: &Editor, snapshot: &MultiBufferSnapshot) -> Vec<Range<u32>> {
    let mut rows = Vec::new();
    for selection in editor.selections.disjoint_anchors().iter() {
        let range = selection.range();
        rows.push(range.start.to_point(snapshot).row..range.end.to_point(snapshot).row);
    }
    if let Some(pending) = editor.selections.pending_anchor() {
        let range = pending.range();
        rows.push(range.start.to_point(snapshot).row..range.end.to_point(snapshot).row);
    }
    rows
}

/// Selection ranges as offsets, including the pending mouse selection.
fn selection_offset_ranges(editor: &Editor, snapshot: &MultiBufferSnapshot) -> Vec<Range<usize>> {
    let mut offsets = Vec::new();
    for selection in editor.selections.disjoint_anchors().iter() {
        let range = selection.range();
        offsets.push(range.start.to_offset(snapshot).0..range.end.to_offset(snapshot).0);
    }
    if let Some(pending) = editor.selections.pending_anchor() {
        let range = pending.range();
        offsets.push(range.start.to_offset(snapshot).0..range.end.to_offset(snapshot).0);
    }
    offsets
}

fn rows_intersect(selection_rows: &[Range<u32>], start_row: u32, end_row: u32) -> bool {
    selection_rows
        .iter()
        .any(|rows| rows.start <= end_row && start_row <= rows.end)
}

fn fold_placeholder(marker: &InlineMarker, editor: WeakEntity<Editor>) -> FoldPlaceholder {
    // Pure hides collapse to zero-width text; bullets and checkboxes keep the
    // default placeholder text, whose visual is replaced by the rendered
    // element at its measured width.
    let collapsed_text = match &marker.kind {
        InlineKind::Hide { .. } => Some(SharedString::new_static("")),
        InlineKind::Bullet | InlineKind::Checkbox { .. } => None,
    };
    let render: Arc<dyn Send + Sync + Fn(_, _, &mut App) -> gpui::AnyElement> = match &marker.kind {
        InlineKind::Hide { .. } => Arc::new(|_, _, _| Empty.into_any_element()),
        InlineKind::Bullet => Arc::new(|_, _, cx| {
            let theme_settings = theme_settings::ThemeSettings::get_global(cx);
            div()
                .font(theme_settings.buffer_font.clone())
                .text_size(theme_settings.buffer_font_size(cx))
                .text_color(cx.theme().colors().text)
                .child("•")
                .into_any_element()
        }),
        InlineKind::Checkbox {
            checked,
            marker_range,
        } => {
            let checked = *checked;
            let marker_range = marker_range.clone();
            Arc::new(move |fold_id, _, _| {
                let editor = editor.clone();
                let marker_range = marker_range.clone();
                Checkbox::new(
                    fold_id,
                    if checked {
                        ToggleState::Selected
                    } else {
                        ToggleState::Unselected
                    },
                )
                .on_click(move |_, _, cx| {
                    toggle_task_marker(&editor, &marker_range, checked, cx);
                })
                .into_any_element()
            })
        }
    };
    FoldPlaceholder {
        render,
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(TypeId::of::<LivePreviewFoldTag>()),
        collapsed_text,
    }
}

fn marker_content_key(kind: &InlineKind) -> u64 {
    match kind {
        InlineKind::Hide { .. } => 0,
        InlineKind::Bullet => 1,
        InlineKind::Checkbox { checked, .. } => 2 + u64::from(*checked),
    }
}

fn toggle_task_marker(
    editor: &WeakEntity<Editor>,
    marker_range: &Range<Anchor>,
    currently_checked: bool,
    cx: &mut App,
) {
    editor
        .update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let range =
                marker_range.start.to_offset(&snapshot)..marker_range.end.to_offset(&snapshot);
            let existing: String = snapshot.text_for_range(range.clone()).collect();
            let expected = if currently_checked { "[x]" } else { "[ ]" };
            if existing.eq_ignore_ascii_case(expected) {
                let replacement = if currently_checked { "[ ]" } else { "[x]" };
                editor.edit([(range, replacement)], cx);
            }
        })
        .log_err();
}

fn buffer_base_directory(editor: &Editor, cx: &App) -> Option<PathBuf> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    let local = file.as_local()?;
    let mut path = local.abs_path(cx);
    path.pop();
    Some(path)
}

fn render_markdown_block(
    markdown: Entity<Markdown>,
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    base_directory: Option<PathBuf>,
    indent_columns: u32,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let style = block_markdown_style(block_cx.window, block_cx.app);
        let editor = editor.clone();
        let start = range.start;
        let base_directory = base_directory.clone();
        let gutter_width =
            block_cx.margins.gutter.full_width() + block_cx.em_width * indent_columns as f32;
        let max_width = block_cx.max_width;
        div()
            .pl(gutter_width)
            .w(max_width)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        let snapshot = editor.buffer().read(cx).snapshot(cx);
                        let offset = start.to_offset(&snapshot);
                        editor.change_selections(Default::default(), window, cx, |selections| {
                            selections.select_ranges([offset..offset]);
                        });
                    })
                    .log_err();
            })
            .child(
                MarkdownElement::new(markdown.clone(), style).image_resolver(move |destination| {
                    resolve_image_source(destination, base_directory.as_deref())
                }),
            )
            .into_any_element()
    })
}

/// Drag payload for the image resize handle; renders no preview.
struct ImageResizeDrag {
    range: Range<Anchor>,
    content_left_offset: gpui::Pixels,
}

struct EmptyDragPreview;

impl gpui::Render for EmptyDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Rewrites (or inserts) Obsidian's `|width` suffix in an image's alt text.
fn with_image_width(image_markdown: &str, width: u32) -> Option<String> {
    let alt_start = image_markdown.find("![")? + 2;
    let alt_end = alt_start + image_markdown.get(alt_start..)?.find(']')?;
    let alt = image_markdown.get(alt_start..alt_end)?;
    let base_alt = alt.rsplit_once('|').map_or(alt, |(base, suffix)| {
        if suffix.chars().all(|c| c.is_ascii_digit() || c == 'x') && !suffix.is_empty() {
            base
        } else {
            alt
        }
    });
    Some(format!(
        "{}{}|{}{}",
        &image_markdown[..alt_start],
        base_alt,
        width,
        &image_markdown[alt_end..]
    ))
}

/// An Obsidian-style image widget: click to select, showing a border, a
/// corner drag handle that resizes by rewriting `|width` into the source,
/// and a code button that reveals the raw markdown.
fn render_image_block(
    markdown: Entity<Markdown>,
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    base_directory: Option<PathBuf>,
    indent_columns: u32,
    display_width: Option<f32>,
    destination: Option<String>,
    alt: SharedString,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let mut style = block_markdown_style(block_cx.window, block_cx.app);
        // Suppress the paragraph's trailing margin so the selection border
        // hugs the image instead of leaving a gap beneath it.
        style.height_is_multiple_of_line_height = true;
        let start = range.start;
        let base_directory = base_directory.clone();
        let gutter_width =
            block_cx.margins.gutter.full_width() + block_cx.em_width * indent_columns as f32;
        let max_width = block_cx.max_width;
        let accent = block_cx.app.theme().colors().text_accent;
        let surface = block_cx.app.theme().colors().elevated_surface_background;

        // Images render at their explicit width, or capped at two thirds of
        // the pane so screenshots do not dominate the note.
        let content_width = display_width
            .map(|width| gpui::px(width).min(max_width))
            .map(gpui::Length::from);

        let selected = editor
            .upgrade()
            .map(|editor_entity| {
                let editor_ref = editor_entity.read(block_cx.app);
                let snapshot = editor_ref.buffer().read(block_cx.app).snapshot(block_cx.app);
                editor_ref
                    .addon::<LivePreviewAddon>()
                    .and_then(|addon| addon.selected_image.as_ref())
                    .is_some_and(|selection| {
                        selection.start.to_offset(&snapshot) == range.start.to_offset(&snapshot)
                            && selection.end.to_offset(&snapshot) == range.end.to_offset(&snapshot)
                    })
            })
            .unwrap_or(false);

        let select_editor = editor.clone();
        let select_range = range.clone();
        let reveal_editor = editor.clone();
        let drag_editor = editor.clone();
        let drag_range = range.clone();

        // A direct image element lets the selection border hug the image
        // exactly; reference-style images (no inline destination) fall back
        // to the markdown renderer.
        let resolved = destination
            .as_deref()
            .and_then(|destination| resolve_image_source(destination, base_directory.as_deref()));
        let muted = block_cx.app.theme().colors().text_muted;
        let image_content: gpui::AnyElement = match (&destination, resolved) {
            (Some(_), Some(source)) => {
                let fallback_alt = alt.clone();
                gpui::img(source)
                    .id(("mdlp-image", f32::from(max_width) as u64))
                    .max_w_full()
                    .rounded_sm()
                    .when(content_width.is_some(), |this| this.w_full())
                    .with_fallback(move || {
                        div()
                            .text_color(muted)
                            .child(fallback_alt.clone())
                            .into_any_element()
                    })
                    .into_any_element()
            }
            (Some(_), None) => div()
                .text_color(muted)
                .child(alt.clone())
                .into_any_element(),
            (None, _) => MarkdownElement::new(markdown.clone(), style)
                .image_resolver(move |destination| {
                    resolve_image_source(destination, base_directory.as_deref())
                })
                .into_any_element(),
        };

        let mut content = div()
            .relative()
            .border_2()
            .rounded_sm()
            .map(|this| {
                if selected {
                    this.border_color(accent)
                } else {
                    this.border_color(gpui::transparent_black())
                }
            })
            .when_some(content_width, |this, width| this.w(width))
            .when(content_width.is_none(), |this| {
                this.max_w(max_width * 0.66)
            })
            .child(image_content);

        if selected {
            content = content
                .child(
                    // Reveal-source button, top right.
                    div()
                        .absolute()
                        .top_1()
                        .right_1()
                        .child(
                            IconButton::new("mdlp-show-source", IconName::Code)
                                .style(ButtonStyle::Filled)
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    reveal_editor
                                        .update(cx, |editor, cx| {
                                            let snapshot =
                                                editor.buffer().read(cx).snapshot(cx);
                                            let offset = start.to_offset(&snapshot);
                                            editor.change_selections(
                                                Default::default(),
                                                window,
                                                cx,
                                                |selections| {
                                                    selections
                                                        .select_ranges([offset..offset]);
                                                },
                                            );
                                        })
                                        .log_err();
                                }),
                        ),
                )
                .child(
                    // Resize handle, bottom right.
                    div()
                        .id("mdlp-resize-handle")
                        .absolute()
                        .bottom_neg_1()
                        .right_neg_1()
                        .size_3()
                        .rounded_full()
                        .border_2()
                        .border_color(accent)
                        .bg(surface)
                        .cursor_col_resize()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_drag(
                            ImageResizeDrag {
                                range: drag_range,
                                content_left_offset: gutter_width,
                            },
                            |_, _, _, cx| cx.new(|_| EmptyDragPreview),
                        ),
                );
        }

        div()
            .pl(gutter_width)
            .w(max_width)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                cx.stop_propagation();
                select_editor
                    .update(cx, |editor, cx| {
                        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                            addon.selected_image = Some(select_range.clone());
                        }
                        cx.notify();
                    })
                    .log_err();
            })
            .on_drag_move::<ImageResizeDrag>(move |event, _window, cx| {
                let drag = event.drag(cx);
                let width = (event.event.position.x
                    - event.bounds.left()
                    - drag.content_left_offset)
                    .max(gpui::px(64.));
                let width = (f32::from(width) / 4.).round() * 4.;
                let drag_range = drag.range.clone();
                drag_editor
                    .update(cx, |editor, cx| {
                        resize_image_to_width(editor, &drag_range, width as u32, cx);
                    })
                    .log_err();
            })
            .child(div().max_w(max_width).child(content))
            .into_any_element()
    })
}

/// Writes `|width` into the image markdown at `range`, throttled to actual
/// changes; the edit round-trips through the normal reparse pipeline, so the
/// widget re-renders at the new size and the whole drag undoes as one step.
fn resize_image_to_width(
    editor: &mut Editor,
    range: &Range<Anchor>,
    width: u32,
    cx: &mut Context<Editor>,
) {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let start = range.start.to_offset(&snapshot);
    let end = range.end.to_offset(&snapshot);
    if start >= end {
        return;
    }
    let current: String = snapshot.text_for_range(start..end).collect();
    let Some(updated) = with_image_width(current.trim(), width) else {
        return;
    };
    if updated == current.trim() {
        return;
    }
    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
        addon.last_resize_at = Some(std::time::Instant::now());
        // Keep the widget selected across the rewrite.
        addon.selected_image = Some(range.clone());
    }
    editor.buffer().update(cx, |multibuffer, cx| {
        multibuffer.edit([(start..end, updated)], None, cx);
    });
}

fn render_rule_block(
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    indent_columns: u32,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let editor = editor.clone();
        let start = range.start;
        let border_color = block_cx.app.theme().colors().border;
        let gutter_width =
            block_cx.margins.gutter.full_width() + block_cx.em_width * indent_columns as f32;
        div()
            .pl(gutter_width)
            .w(block_cx.max_width)
            .h(block_cx.line_height)
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        let snapshot = editor.buffer().read(cx).snapshot(cx);
                        let offset = start.to_offset(&snapshot);
                        editor.change_selections(Default::default(), window, cx, |selections| {
                            selections.select_ranges([offset..offset]);
                        });
                    })
                    .log_err();
            })
            .child(div().flex_1().h(gpui::px(2.)).bg(border_color))
            .into_any_element()
    })
}

fn render_frontmatter_block(
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    source: String,
) -> RenderBlock {
    let properties: Vec<(String, String)> = source
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed == "---" || trimmed == "+++" {
                return None;
            }
            let (key, value) = trimmed
                .split_once(':')
                .or_else(|| trimmed.split_once('='))
                .unwrap_or(("", trimmed));
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    Arc::new(move |block_cx| {
        let editor = editor.clone();
        let start = range.start;
        let colors = block_cx.app.theme().colors().clone();
        let gutter_width = block_cx.margins.gutter.full_width();
        div()
            .pl(gutter_width)
            .w(block_cx.max_width)
            .py(gpui::px(2.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                editor
                    .update(cx, |editor, cx| {
                        let snapshot = editor.buffer().read(cx).snapshot(cx);
                        let offset = start.to_offset(&snapshot);
                        editor.change_selections(Default::default(), window, cx, |selections| {
                            selections.select_ranges([offset..offset]);
                        });
                    })
                    .log_err();
            })
            .child(
                v_flex()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(colors.elevated_surface_background)
                    .px_3()
                    .py_1p5()
                    .gap_0p5()
                    .text_size(rems(0.85))
                    .children(properties.iter().map(|(key, value)| {
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                div()
                                    .min_w(rems(7.))
                                    .text_color(colors.text_muted)
                                    .child(SharedString::from(key.clone())),
                            )
                            .child(div().flex_1().child(SharedString::from(value.clone())))
                    })),
            )
            .into_any_element()
    })
}

fn resolve_image_source(
    destination: &str,
    base_directory: Option<&std::path::Path>,
) -> Option<ImageSource> {
    if destination.starts_with("data:") {
        return None;
    }
    if destination.starts_with("http://") || destination.starts_with("https://") {
        return Some(ImageSource::Resource(Resource::Uri(SharedUri::from(
            destination.to_string(),
        ))));
    }
    // Markdown links percent-encode spaces; the filesystem stores them raw.
    let decoded = urlencoding::decode(destination)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| destination.to_string());
    let path = std::path::Path::new(&decoded);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_directory?.join(path)
    };
    path.exists()
        .then(|| ImageSource::Resource(Resource::Path(Arc::from(path.as_path()))))
}

fn block_markdown_style(window: &Window, cx: &App) -> MarkdownStyle {
    let mut style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
    let heading = |size: f32, weight: FontWeight| {
        Some(TextStyleRefinement {
            font_size: Some(rems(size).into()),
            font_weight: Some(weight),
            ..Default::default()
        })
    };
    style.heading_level_styles = Some(HeadingLevelStyles {
        h1: heading(1.6, FontWeight::BOLD),
        h2: heading(1.4, FontWeight::BOLD),
        h3: heading(1.2, FontWeight::SEMIBOLD),
        h4: heading(1.1, FontWeight::SEMIBOLD),
        h5: heading(1.0, FontWeight::SEMIBOLD),
        h6: heading(0.9, FontWeight::SEMIBOLD),
    });
    style
}

// --- Marker extraction ---

fn extract_markers(editor: &Editor, cx: &App) -> Option<MarkerSet> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let buffer = buffer.read(cx);
    let language = buffer.language()?;
    if language.name() != LanguageName::new(MARKDOWN) {
        return None;
    }
    let buffer_snapshot = buffer.snapshot();
    let multibuffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let text = buffer_snapshot.text();

    let mut extraction = Extraction {
        text: &text,
        snapshot: &multibuffer_snapshot,
        prose_regions: Vec::new(),
        code_spans: Vec::new(),
        inline: Vec::new(),
        blocks: Vec::new(),
        strikethrough: Vec::new(),
        italic: Vec::new(),
        bold: Vec::new(),
        link_text: Vec::new(),
        definitions: Vec::new(),
        definition_ranges: Vec::new(),
        ordered_markers: Vec::new(),
    };

    for layer in buffer_snapshot.syntax_layers() {
        let root = layer.node();
        match layer.language.name().as_ref() {
            MARKDOWN => extraction.walk_block_layer(root),
            MARKDOWN_INLINE => extraction.walk_inline_layer(root),
            _ => {}
        }
    }

    extraction.scan_wikilinks();

    let Extraction {
        inline,
        mut blocks,
        strikethrough,
        italic,
        bold,
        link_text,
        definitions,
        definition_ranges,
        ordered_markers,
        ..
    } = extraction;

    // Blocks from different layers can overlap (e.g. an image inside a table
    // row); keep the outermost region and drop any block nested in or
    // overlapping a previous one.
    blocks.sort_by(|a, b| {
        let a_start = a.range.start.to_offset(&multibuffer_snapshot);
        let b_start = b.range.start.to_offset(&multibuffer_snapshot);
        a_start.cmp(&b_start).then_with(|| {
            let a_end = a.range.end.to_offset(&multibuffer_snapshot);
            let b_end = b.range.end.to_offset(&multibuffer_snapshot);
            b_end.cmp(&a_end)
        })
    });
    let mut last_end = 0;
    blocks.retain(|block| {
        let start = block.range.start.to_offset(&multibuffer_snapshot).0;
        let end = block.range.end.to_offset(&multibuffer_snapshot).0;
        if start < last_end {
            false
        } else {
            last_end = end;
            true
        }
    });

    Some(MarkerSet {
        inline,
        blocks,
        strikethrough,
        italic,
        bold,
        link_text,
        definitions: definitions.join("\n"),
        definition_ranges,
        ordered_markers,
    })
}

struct Extraction<'a> {
    text: &'a str,
    snapshot: &'a MultiBufferSnapshot,
    /// Prose regions (the block grammar's `inline` nodes) and code spans,
    /// used to scan for wikilinks only where they can occur.
    prose_regions: Vec<Range<usize>>,
    code_spans: Vec<Range<usize>>,
    inline: Vec<InlineMarker>,
    blocks: Vec<BlockMarker>,
    strikethrough: Vec<Range<Anchor>>,
    italic: Vec<Range<Anchor>>,
    bold: Vec<Range<Anchor>>,
    link_text: Vec<Range<Anchor>>,
    definitions: Vec<String>,
    definition_ranges: Vec<Range<Anchor>>,
    ordered_markers: Vec<Range<Anchor>>,
}

impl Extraction<'_> {
    fn anchor_range(&self, range: Range<usize>) -> Range<Anchor> {
        // Bias the anchors inward so text inserted at the boundaries falls
        // outside the hidden range rather than growing it.
        self.snapshot
            .anchor_after(MultiBufferOffset(range.start))
            ..self.snapshot.anchor_before(MultiBufferOffset(range.end))
    }

    fn hide(&mut self, range: Range<usize>, reveal_span: Range<usize>) {
        if range.start < range.end {
            self.inline.push(InlineMarker {
                range: self.anchor_range(range),
                kind: InlineKind::Hide {
                    reveal_span: self.anchor_range(reveal_span),
                },
            });
        }
    }

    /// The row extent of a node, excluding a trailing newline that tree-sitter
    /// includes in block constructs.
    fn node_rows(&self, node: tree_sitter::Node) -> (u32, u32) {
        let start_row = node.start_position().row as u32;
        let mut end_row = node.end_position().row as u32;
        if node.end_position().column == 0 && end_row > start_row {
            end_row -= 1;
        }
        (start_row, end_row)
    }

    fn push_block_rows(
        &mut self,
        start_row: u32,
        end_row: u32,
        height_estimate: u32,
        kind: BlockRenderKind,
    ) {
        let start = Point::new(start_row, 0);
        let end = Point::new(end_row, self.snapshot.line_len(MultiBufferRow(end_row)));
        let line_start = self.snapshot.point_to_offset(start);
        let line_end = self.snapshot.point_to_offset(end);
        let first_line: String = self.snapshot.text_for_range(line_start..line_end).collect();
        let indent_columns = first_line
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(|character| if character == '\t' { 4 } else { 1 })
            .sum();
        let range = self.snapshot.anchor_before(start)..self.snapshot.anchor_after(end);
        self.blocks.push(BlockMarker {
            range,
            height_estimate,
            kind,
            indent_columns,
        });
    }

    fn walk_block_layer(&mut self, root: tree_sitter::Node) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "atx_heading" => {
                    let (start_row, end_row) = self.node_rows(node);
                    let level = heading_level(node);
                    let height = if level <= 2 { 2 } else { 1 };
                    self.push_block_rows(start_row, end_row, height, BlockRenderKind::Markdown);
                }
                "setext_heading" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(start_row, end_row, 2, BlockRenderKind::Markdown);
                }
                "thematic_break" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(start_row, end_row, 1, BlockRenderKind::Rule);
                }
                "pipe_table" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(
                        start_row,
                        end_row,
                        end_row - start_row + 2,
                        BlockRenderKind::Markdown,
                    );
                }
                "fenced_code_block" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(
                        start_row,
                        end_row,
                        end_row - start_row + 2,
                        BlockRenderKind::Markdown,
                    );
                }
                "minus_metadata" | "plus_metadata" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(
                        start_row,
                        end_row,
                        end_row - start_row,
                        BlockRenderKind::Frontmatter,
                    );
                }
                "html_block" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(
                        start_row,
                        end_row,
                        end_row - start_row + 1,
                        BlockRenderKind::Markdown,
                    );
                }
                "block_quote" => {
                    let (start_row, end_row) = self.node_rows(node);
                    self.push_block_rows(
                        start_row,
                        end_row,
                        end_row - start_row + 1,
                        BlockRenderKind::Markdown,
                    );
                    push_children(node, &mut stack);
                }
                "link_reference_definition" => {
                    if let Some(text) = self.text.get(node.byte_range()) {
                        self.definitions.push(text.trim_end().to_string());
                    }
                    let trimmed_len = self
                        .text
                        .get(node.byte_range())
                        .map_or(0, |text| text.trim_end().len());
                    if trimmed_len > 0 {
                        let start = node.start_byte();
                        let range = self.anchor_range(start..start + trimmed_len);
                        self.definition_ranges.push(range);
                    }
                }
                "list_item" => {
                    self.list_item_markers(node);
                    push_children(node, &mut stack);
                }
                "inline" => self.prose_regions.push(node.byte_range()),
                _ => push_children(node, &mut stack),
            }
        }
    }

    fn list_item_markers(&mut self, node: tree_sitter::Node) {
        let mut list_marker = None;
        let mut task_marker = None;
        for index in 0..node.child_count() as u32 {
            let Some(child) = node.child(index) else {
                continue;
            };
            match child.kind() {
                "list_marker_minus" | "list_marker_plus" | "list_marker_star" => {
                    list_marker = Some(child);
                }
                "list_marker_dot" | "list_marker_parenthesis" => {
                    let Some(marker_text) = self.text.get(child.byte_range()) else {
                        continue;
                    };
                    let trimmed_len = marker_text.trim_end().len();
                    if trimmed_len > 0 {
                        let start = child.start_byte();
                        let range = self.anchor_range(start..start + trimmed_len);
                        self.ordered_markers.push(range);
                    }
                }
                "task_list_marker_checked" => task_marker = Some((child, true)),
                "task_list_marker_unchecked" => task_marker = Some((child, false)),
                _ => {}
            }
        }

        let Some(list_marker) = list_marker else {
            return;
        };

        if let Some((task_node, checked)) = task_marker {
            let range = list_marker.start_byte()..task_node.end_byte();
            let marker_range = self.anchor_range(task_node.byte_range());
            self.inline.push(InlineMarker {
                range: self.anchor_range(range),
                kind: InlineKind::Checkbox {
                    checked,
                    marker_range,
                },
            });
        } else {
            let Some(marker_text) = self.text.get(list_marker.byte_range()) else {
                return;
            };
            let trimmed_len = marker_text.trim_end().len();
            if trimmed_len == 0 {
                return;
            }
            let start = list_marker.start_byte();
            self.inline.push(InlineMarker {
                range: self.anchor_range(start..start + trimmed_len),
                kind: InlineKind::Bullet,
            });
        }
    }

    fn walk_inline_layer(&mut self, root: tree_sitter::Node) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "emphasis" | "strong_emphasis" | "strikethrough" => {
                    let range = self.anchor_range(node.byte_range());
                    match node.kind() {
                        "strikethrough" => self.strikethrough.push(range),
                        "emphasis" => self.italic.push(range),
                        _ => self.bold.push(range),
                    }
                    for index in 0..node.child_count() as u32 {
                        let Some(child) = node.child(index) else {
                            continue;
                        };
                        if child.kind() == "emphasis_delimiter" {
                            self.hide(child.byte_range(), node.byte_range());
                        }
                    }
                    push_children(node, &mut stack);
                }
                "code_span" => {
                    self.code_spans.push(node.byte_range());
                    for index in 0..node.child_count() as u32 {
                        let Some(child) = node.child(index) else {
                            continue;
                        };
                        if child.kind() == "code_span_delimiter" {
                            self.hide(child.byte_range(), node.byte_range());
                        }
                    }
                }
                "inline_link" | "full_reference_link" | "collapsed_reference_link" => {
                    let mut open_bracket = None;
                    let mut close_bracket = None;
                    for index in 0..node.child_count() as u32 {
                        let Some(child) = node.child(index) else {
                            continue;
                        };
                        match child.kind() {
                            "[" if open_bracket.is_none() => open_bracket = Some(child),
                            "]" => close_bracket = Some(child),
                            _ => {}
                        }
                    }
                    if let Some(open) = open_bracket {
                        self.hide(open.byte_range(), node.byte_range());
                    }
                    if let Some(close) = close_bracket {
                        self.hide(close.start_byte()..node.end_byte(), node.byte_range());
                    }
                    if let (Some(open), Some(close)) = (open_bracket, close_bracket)
                        && open.end_byte() < close.start_byte()
                    {
                        let range = self.anchor_range(open.end_byte()..close.start_byte());
                        self.link_text.push(range);
                    }
                    // A standalone link wrapping an image renders as an image
                    // widget built from just the inner image markdown: the
                    // markdown renderer degrades a link-wrapped image to
                    // literal text (the preview pane has the same limit). The
                    // image sits under a `link_text` node, not directly under
                    // the link.
                    let wrapped_image = (0..node.child_count() as u32)
                        .filter_map(|index| node.child(index))
                        .find_map(|child| {
                            if child.kind() == "image" {
                                Some(child)
                            } else if child.kind() == "link_text" {
                                (0..child.child_count() as u32)
                                    .filter_map(|index| child.child(index))
                                    .find(|grandchild| grandchild.kind() == "image")
                            } else {
                                None
                            }
                        });
                    if let Some(image_node) = wrapped_image
                        && self.is_alone_on_line(node)
                    {
                        let image_range = image_node.byte_range();
                        let range = self
                            .snapshot
                            .anchor_before(MultiBufferOffset(image_range.start))
                            ..self
                                .snapshot
                                .anchor_after(MultiBufferOffset(image_range.end));
                        let kind = self.image_kind(image_node);
                        self.blocks.push(BlockMarker {
                            range,
                            height_estimate: 8,
                            kind,
                            indent_columns: 0,
                        });
                    }
                    push_children(node, &mut stack);
                }
                "uri_autolink" | "email_autolink" => {
                    let range = node.byte_range();
                    if range.len() >= 2 {
                        self.hide(range.start..range.start + 1, range.clone());
                        self.hide(range.end - 1..range.end, range.clone());
                    }
                }
                "image" => {
                    // `![[...]]` is an Obsidian embed, not a markdown image;
                    // leave it raw rather than concealing it half-way.
                    if self
                        .text
                        .get(node.byte_range())
                        .is_some_and(|text| text.starts_with("![["))
                    {
                        continue;
                    }
                    if self.is_alone_on_line(node) {
                        self.image_block(node);
                    } else if let Some(description) = (0..node.child_count() as u32)
                        .filter_map(|index| node.child(index))
                        .find(|child| child.kind() == "image_description")
                    {
                        // The image itself cannot render mid-line, but the
                        // alt text can: conceal `![` and `](url)` like links.
                        self.hide(
                            node.start_byte()..description.start_byte(),
                            node.byte_range(),
                        );
                        self.hide(description.end_byte()..node.end_byte(), node.byte_range());
                    }
                }
                _ => push_children(node, &mut stack),
            }
        }
    }

    /// Conceal Obsidian-style wikilinks: `[[Note]]`, `[[Note|alias]]`, and
    /// `[[Note#heading]]`. Zed's markdown grammar has no wikilink nodes, so
    /// this scans the prose regions directly, skipping code spans; embeds
    /// (`![[...]]`) are left raw.
    fn scan_wikilinks(&mut self) {
        let regions = std::mem::take(&mut self.prose_regions);
        for region in &regions {
            let Some(region_text) = self.text.get(region.clone()) else {
                continue;
            };
            let mut search_from = 0;
            while let Some(open_offset) = region_text[search_from..].find("[[") {
                let open = search_from + open_offset;
                let Some(close_offset) = region_text[open + 2..].find("]]") else {
                    break;
                };
                let close = open + 2 + close_offset;
                search_from = close + 2;

                let inner = &region_text[open + 2..close];
                if inner.is_empty() || inner.contains('\n') || inner.contains("[[") {
                    continue;
                }
                let is_embed = region_text[..open].ends_with('!');
                let start = region.start + open;
                let end = region.start + close + 2;
                if is_embed
                    || self
                        .code_spans
                        .iter()
                        .any(|span| span.start < end && start < span.end)
                {
                    continue;
                }

                let reveal = start..end;
                if let Some(pipe) = inner.find('|') {
                    // `[[target|alias]]`: show only the alias.
                    self.hide(start..start + 2 + pipe + 1, reveal.clone());
                    self.hide(end - 2..end, reveal.clone());
                    let alias_start = start + 2 + pipe + 1;
                    let range = self.anchor_range(alias_start..end - 2);
                    self.link_text.push(range);
                } else {
                    self.hide(start..start + 2, reveal.clone());
                    self.hide(end - 2..end, reveal.clone());
                    let range = self.anchor_range(start + 2..end - 2);
                    self.link_text.push(range);
                }
            }
        }
        self.prose_regions = regions;
    }

    /// Whether this single-line node is the only content on its line.
    fn is_alone_on_line(&self, node: tree_sitter::Node) -> bool {
        if node.start_position().row != node.end_position().row {
            return false;
        }
        let row = node.start_position().row as u32;
        let line_start = self.snapshot.point_to_offset(Point::new(row, 0));
        let line_end = self
            .snapshot
            .point_to_offset(Point::new(row, self.snapshot.line_len(MultiBufferRow(row))));
        let line_text: String = self.snapshot.text_for_range(line_start..line_end).collect();
        self.text
            .get(node.byte_range())
            .is_some_and(|node_text| line_text.trim() == node_text.trim())
    }

    /// Obsidian's image size syntax: `![alt|640](p)` or `![alt|640x480](p)`.
    fn image_display_width(&self, image_node: tree_sitter::Node) -> Option<f32> {
        let (_, size) = self.image_alt(image_node)?.rsplit_once('|')?;
        let width: String = size.chars().take_while(|c| c.is_ascii_digit()).collect();
        width.parse::<f32>().ok().filter(|width| *width > 0.)
    }

    fn image_alt(&self, image_node: tree_sitter::Node) -> Option<&str> {
        let description = (0..image_node.child_count() as u32)
            .filter_map(|index| image_node.child(index))
            .find(|child| child.kind() == "image_description")?;
        self.text.get(description.byte_range())
    }

    fn image_destination(&self, image_node: tree_sitter::Node) -> Option<String> {
        let destination = (0..image_node.child_count() as u32)
            .filter_map(|index| image_node.child(index))
            .find(|child| child.kind() == "link_destination")?;
        self.text
            .get(destination.byte_range())
            .map(|text| text.to_string())
    }

    fn image_kind(&self, image_node: tree_sitter::Node) -> BlockRenderKind {
        let alt = self
            .image_alt(image_node)
            .map(|alt| alt.rsplit_once('|').map_or(alt, |(base, _)| base))
            .unwrap_or_default()
            .to_string();
        BlockRenderKind::Image {
            display_width: self.image_display_width(image_node),
            destination: self.image_destination(image_node),
            alt,
        }
    }

    /// Renders an image as a block widget when it is the only content on its
    /// line; inline images are left as raw markdown.
    fn image_block(&mut self, node: tree_sitter::Node) {
        if self.is_alone_on_line(node) {
            let row = node.start_position().row as u32;
            let kind = self.image_kind(node);
            self.push_block_rows(row, row, 8, kind);
        }
    }
}

fn push_children<'a>(node: tree_sitter::Node<'a>, stack: &mut Vec<tree_sitter::Node<'a>>) {
    for index in (0..node.child_count() as u32).rev() {
        if let Some(child) = node.child(index) {
            stack.push(child);
        }
    }
}

fn heading_level(node: tree_sitter::Node) -> u32 {
    for index in 0..node.child_count() as u32 {
        if let Some(child) = node.child(index) {
            match child.kind() {
                "atx_h1_marker" => return 1,
                "atx_h2_marker" => return 2,
                "atx_h3_marker" => return 3,
                "atx_h4_marker" => return 4,
                "atx_h5_marker" => return 5,
                "atx_h6_marker" => return 6,
                _ => {}
            }
        }
    }
    6
}

#[cfg(test)]
mod tests;
