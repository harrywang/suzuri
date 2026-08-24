//! Obsidian-style live preview for markdown buffers.
//!
//! When enabled, markdown syntax markers (`**`, `*`, `~~`, backticks, link
//! targets, list bullets) are hidden and rendered inline, and block elements
//! (headings, tables, images, mermaid diagrams, horizontal rules) are replaced
//! with rendered widgets. Raw markdown is revealed for editing per token: an
//! inline construct reveals when the selection touches it, and a block
//! reveals when the selection reaches its lines, mirroring Obsidian's Live
//! Preview mode. Tables and images are the exception: they are edited
//! through their widgets and only reveal source via their `</>` button.

use std::{any::TypeId, borrow::Cow, ops::Range, path::PathBuf, sync::Arc};

use collections::{HashMap, HashSet};
use editor::{
    Addon, Editor, EditorEvent, FoldPlaceholder, HighlightKey,
    display_map::{
        BlockPlacement, BlockProperties, BlockStyle, Concealment, CustomBlockId, RenderBlock,
    },
};
use gpui::{
    App, AppContext as _, Context, Empty, Entity, Focusable as _, FontWeight, HighlightStyle, Hsla,
    ImageSource, IntoElement, MouseButton, MouseDownEvent, Resource, RetainAllImageCache,
    SharedString, SharedUri, StrikethroughStyle, Subscription, TextStyleRefinement, WeakEntity,
    Window, actions, img, rems,
};
use language::LanguageName;
use markdown::{HeadingLevelStyles, Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use math_render::{MathStyle, MathTheme};
use multi_buffer::{
    Anchor, MultiBufferOffset, MultiBufferRow, MultiBufferSnapshot, ToOffset as _, ToPoint as _,
};
use project::PathChange;
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
    subscriptions.push(
        cx.subscribe_self(|editor, event: &EditorEvent, cx| match event {
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
        }),
    );

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
                        let enabled = addon
                            .enabled_override
                            .unwrap_or_else(|| MarkdownLivePreviewSettings::get_global(cx).enabled);
                        addon.enabled_override = Some(!enabled);
                    }
                    recompute(editor, cx);
                })
                .log_err();
        }),
    );

    // Pasting an image saves it into the attachments folder and inserts an
    // embed link (Obsidian-style); text pastes pass through untouched.
    let weak_editor = cx.weak_entity();
    subscriptions.push(
        editor.register_action::<editor::actions::Paste>(move |_, _window, cx| {
            let handled = weak_editor
                .update(cx, |editor, cx| {
                    editor::items::paste_clipboard_image(editor, cx)
                })
                .unwrap_or(false);
            if !handled {
                cx.propagate();
            }
        }),
    );

    // Backspace/Delete removes a selected table row/column (Obsidian-style)
    // instead of editing text; without a selection they pass through.
    let weak_editor = cx.weak_entity();
    subscriptions.push(editor.register_action::<editor::actions::Backspace>(
        move |_, _window, cx| {
            if !delete_selected_table_unit(&weak_editor, cx) {
                cx.propagate();
            }
        },
    ));
    let weak_editor = cx.weak_entity();
    subscriptions.push(
        editor.register_action::<editor::actions::Delete>(move |_, _window, cx| {
            if !delete_selected_table_unit(&weak_editor, cx) {
                cx.propagate();
            }
        }),
    );

    // Local images are cached by path, so overwriting a file in place (say,
    // re-cropping a screenshot) would otherwise keep serving the bitmap
    // decoded on first render for the life of the process: gpui's app-level
    // asset cache is never evicted, and outlives the document. Owning the
    // cache lets a change on disk drop just that entry, and lets closing the
    // editor release every image it decoded.
    let image_cache = RetainAllImageCache::new(cx);
    if let Some(project) = editor.project().cloned() {
        subscriptions.push(cx.subscribe_in(&project, window, {
            let image_cache = image_cache.clone();
            move |_editor, project, event, window, cx| {
                let project::Event::WorktreeUpdatedEntries(worktree_id, changes) = event else {
                    return;
                };
                let Some(worktree) = project.read(cx).worktree_for_id(*worktree_id, cx) else {
                    return;
                };
                let changed_images: Vec<PathBuf> = {
                    let worktree = worktree.read(cx);
                    changes
                        .iter()
                        // `Loaded` is the initial scan reporting what was
                        // already there, not a change; acting on it would
                        // re-decode every image in the project on open.
                        .filter(|(_, _, change)| *change != PathChange::Loaded)
                        .filter(|(path, _, _)| path.extension().is_some_and(is_image_extension))
                        .map(|(path, _, _)| {
                            let absolute = worktree.absolutize(path);
                            // Matches how `resolve_image_source` keys the
                            // cache. A just-deleted file cannot be
                            // canonicalized; fall back to the literal path so
                            // an exactly-spelled reference still evicts.
                            std::fs::canonicalize(&absolute).unwrap_or(absolute)
                        })
                        .collect()
                };
                if changed_images.is_empty() {
                    return;
                }
                image_cache.update(cx, |image_cache, cx| {
                    for path in changed_images {
                        image_cache.remove(&Resource::Path(Arc::from(path.as_path())), window, cx);
                    }
                });
                // The widgets keep their blocks; they just need to redraw so
                // the evicted images are fetched again.
                cx.notify();
            }
        }));
    }

    editor.register_addon(LivePreviewAddon {
        enabled_override: None,
        image_cache,
        markers: None,
        applied_blocks: Vec::new(),
        selected_image: None,
        active_cell: None,
        selected_table_unit: None,
        drag_source: None,
        drop_boundary: None,
        handle_press: None,
        source_revealed: None,
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
    /// Cache backing every local image this editor renders, so a file changed
    /// on disk can be evicted from it. See `register_editor`.
    image_cache: Entity<RetainAllImageCache>,
    markers: Option<Arc<MarkerSet>>,
    applied_blocks: Vec<AppliedBlock>,
    /// The image widget currently selected (Obsidian-style click state),
    /// identified by its marker range.
    selected_image: Option<Range<Anchor>>,
    /// The table cell currently being edited in place.
    active_cell: Option<ActiveTableCell>,
    selected_table_unit: Option<TableUnitSelection>,
    /// Unit being dragged (outlined in place, Obsidian-style) and the unit
    /// the pointer is currently over (gets the insertion line). Rendered
    /// only while a drag is active.
    drag_source: Option<TableUnitSelection>,
    /// Insertion point the pointer currently indicates, with half-cell
    /// precision: hovering the near half of a unit targets the boundary
    /// before it, the far half the boundary after it.
    drop_boundary: Option<(Range<Anchor>, TableBoundary)>,
    /// Position of the last mouse-down on a table handle. Selection commits
    /// on mouse-up only if the pointer barely moved; recording this must NOT
    /// notify — a re-render between press and move would drop gpui's
    /// per-frame drag-arming listeners and kill the drag gesture.
    handle_press: Option<gpui::Point<gpui::Pixels>>,
    /// Block explicitly revealed via its `</>` button. Tables and images
    /// only show source through this, never from cursor overlap alone.
    source_revealed: Option<Range<Anchor>>,
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
    /// Pandoc-style citation keys (`@key` including the `@`), styled as
    /// reference chips once the surrounding brackets are concealed.
    citations: Vec<Range<Anchor>>,
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
    /// A LaTeX formula (`$x$` or `$$x$$`), rendered as a typeset image.
    ///
    /// Rendering is asynchronous, so the placeholder reads whatever the shared
    /// [`MathCache`] holds for `source`; until that resolves it falls back to
    /// the LaTeX source, which is also what a formula that fails to parse keeps
    /// showing.
    Math {
        source: SharedString,
        style: MathStyle,
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
    /// A pipe table rendered as an editable grid: clicking a cell mounts a
    /// single-line editor over it, and structural buttons add rows/columns.
    Table(TableStructure),
    /// A standalone image, width-capped and honoring Obsidian's
    /// `![alt|640](path)` size syntax. When the destination is known the
    /// widget renders a bare image element (which the selection border hugs
    /// exactly); reference-style images fall back to the markdown renderer.
    Image {
        display_width: Option<f32>,
        destination: Option<String>,
        alt: String,
    },
    /// Display math (`$$...$$` alone on its lines), rendered as a centered
    /// typeset formula. Unlike other blocks, revealing its source does not
    /// remove the widget: the rendering stays below the source lines and
    /// live-updates while the formula is edited, following Obsidian.
    Math {
        /// The LaTeX between the delimiters.
        source: String,
    },
}

#[derive(Clone, PartialEq)]
struct TableStructure {
    header: Vec<Range<Anchor>>,
    alignments: Vec<CellAlignment>,
    rows: Vec<Vec<Range<Anchor>>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CellAlignment {
    None,
    Left,
    Center,
    Right,
}

impl TableStructure {
    /// All cell ranges in reading order, for Tab navigation.
    fn cells_in_order(&self) -> Vec<Range<Anchor>> {
        self.header
            .iter()
            .chain(self.rows.iter().flatten())
            .cloned()
            .collect()
    }
}

/// Payload for dragging a row handle to reorder rows within one table.
/// Table identity is an anchor resolved at drop time: the payload and the
/// drop target are captured on different frames, so raw offsets diverge as
/// soon as anything edits the buffer.
struct TableRowDrag {
    table_start: Anchor,
    row: usize,
}

/// Payload for dragging a column handle to reorder columns within one table.
struct TableColumnDrag {
    table_start: Anchor,
    column: usize,
}

/// An insertion point between rows or columns: `Row(i)`/`Column(i)` means
/// "insert before index i", with i == count meaning after the last.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TableBoundary {
    Row(usize),
    Column(usize),
}

/// A whole row or column selected via its hover handle, Obsidian-style.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TableUnit {
    /// Data row index (the header is not selectable).
    Row(usize),
    Column(usize),
}

struct TableUnitSelection {
    table_range: Range<Anchor>,
    unit: TableUnit,
}

/// The single-line editor mounted over a table cell.
struct ActiveTableCell {
    cell_range: Range<Anchor>,
    editor: Entity<Editor>,
    _subscriptions: Vec<Subscription>,
}

struct AppliedBlock {
    range: Range<Anchor>,
    source: String,
    /// True when the widget is placed below its source lines instead of
    /// replacing them — display math while its source is revealed.
    below: bool,
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

/// Namespaces within `HighlightKey::MarkdownLivePreview`, one per decoration
/// kind so each can be written and cleared independently of the others.
const STRIKE: usize = 0;
const ITALIC: usize = 1;
const BOLD: usize = 2;
const LINK: usize = 3;
const DEFINITION: usize = 4;
const ORDERED_MARKER: usize = 5;
const CITATION: usize = 6;

/// Emphasis spans get preview-like typography: the plain text color with true
/// bold/italic styling, overriding the theme's source-mode markup colors
/// (e.g. blue non-slanted italics, orange bold), plus a real line-through for
/// strikethrough, which themes color but never strike.
fn apply_emphasis_highlights(
    editor: &mut Editor,
    markers: Option<&MarkerSet>,
    cx: &mut Context<Editor>,
) {
    let text_color = cx.theme().colors().text;
    let accent_color = cx.theme().colors().text_accent;
    let muted_color = cx.theme().colors().text_muted;
    let citation_background = cx
        .theme()
        .colors()
        .editor_document_highlight_read_background;
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
        (
            CITATION,
            markers.map(|markers| markers.citations.clone()),
            HighlightStyle {
                color: Some(accent_color),
                background_color: Some(citation_background),
                font_style: Some(gpui::FontStyle::Normal),
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
    let image_cache = addon.image_cache.clone();
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
    let source_revealed = editor
        .addon::<LivePreviewAddon>()
        .and_then(|addon| addon.source_revealed.clone())
        .filter(|revealed| {
            let start = revealed.start.to_point(&snapshot).row;
            let end = revealed.end.to_point(&snapshot).row;
            rows_intersect(&selection_rows, start, end)
        });
    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
        addon.source_revealed = source_revealed.clone();
    }
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
            // Math reveals when the selection touches the formula, so putting
            // the cursor in it hands back the LaTeX to edit.
            InlineKind::Bullet | InlineKind::Checkbox { .. } | InlineKind::Math { .. } => {
                &marker.range
            }
        };
        let span = reveal_span.start.to_offset(&snapshot).0..reveal_span.end.to_offset(&snapshot).0;
        let revealed = selection_offsets
            .iter()
            .any(|selection| selection.start <= span.end && span.start <= selection.end);
        if revealed {
            continue;
        }
        // Start the render here rather than in the placeholder closure, which
        // runs every frame; by the time it draws, the cache holds at least a
        // pending entry.
        if let InlineKind::Math { source, style } = &marker.kind {
            let text_color = cx.theme().colors().editor_foreground;
            request_math_render(
                MathKey {
                    source: source.clone(),
                    style: *style,
                    color: u32::from(gpui::Rgba::from(text_color)),
                },
                text_color,
                cx,
            );
        }
        concealments.push(Concealment {
            range: marker.range.clone(),
            placeholder: fold_placeholder(marker, weak_editor.clone()),
            content_key: marker_content_key(&marker.kind),
        });
    }
    editor.set_concealments(TypeId::of::<LivePreviewFoldTag>(), concealments, cx);

    // --- Block widgets ---

    let mut desired_blocks: HashMap<(usize, usize), (&BlockMarker, String, bool)> =
        HashMap::default();
    for marker in &markers.blocks {
        let start = marker.range.start.to_point(&snapshot);
        let end = marker.range.end.to_point(&snapshot);
        if start > end {
            continue;
        }
        let mut below = false;
        if rows_intersect(&selection_rows, start.row, end.row) {
            // Casual clicks land the cursor on widget rows constantly, which
            // made tables and images explode into source; those two reveal
            // only via their explicit `</>` button.
            let needs_explicit_reveal = matches!(
                marker.kind,
                BlockRenderKind::Table(_) | BlockRenderKind::Image { .. }
            );
            let explicitly_revealed = source_revealed.as_ref().is_some_and(|revealed| {
                let revealed_start = revealed.start.to_point(&snapshot).row;
                let revealed_end = revealed.end.to_point(&snapshot).row;
                revealed_start <= end.row && start.row <= revealed_end
            });
            if matches!(marker.kind, BlockRenderKind::Math { .. }) {
                // Editing display math keeps the rendering on screen: the
                // widget moves below the revealed source and live-updates
                // as the formula is typed, following Obsidian.
                below = true;
            } else if !needs_explicit_reveal || explicitly_revealed {
                continue;
            }
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
        desired_blocks.insert((start_offset.0, end_offset.0), (marker, source, below));
    }

    let mut new_applied_blocks = Vec::new();
    let mut block_ids_to_remove = HashSet::default();
    for applied in applied_blocks {
        let start = applied.range.start.to_offset(&snapshot).0;
        let end = applied.range.end.to_offset(&snapshot).0;
        let keep = desired_blocks
            .get(&(start, end))
            .is_some_and(|(_, source, below)| *source == applied.source && *below == applied.below);
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
    for (marker, source, below) in desired_blocks.into_values() {
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
                    image_cache.clone(),
                )
            }
            BlockRenderKind::Table(structure) => {
                let cell_markdown = |range: &Range<Anchor>, cx: &mut Context<Editor>| {
                    let start = range.start.to_offset(&snapshot);
                    let end = range.end.to_offset(&snapshot);
                    let text: String = snapshot.text_for_range(start..end).collect();
                    let text = wikilink_display_text(text.trim()).into_owned();
                    cx.new(|cx| {
                        // A pipe table row cannot contain a newline, so `<br>`
                        // is the only way to break a line inside a cell; that
                        // needs the HTML parser other blocks already enable.
                        Markdown::new_with_options(
                            SharedString::from(text),
                            language_registry.clone(),
                            None,
                            markdown::MarkdownOptions {
                                parse_html: true,
                                ..Default::default()
                            },
                            cx,
                        )
                    })
                };
                let header_markdown: Vec<Entity<Markdown>> = structure
                    .header
                    .iter()
                    .map(|range| cell_markdown(range, cx))
                    .collect();
                let rows_markdown: Vec<Vec<Entity<Markdown>>> = structure
                    .rows
                    .iter()
                    .map(|row| row.iter().map(|range| cell_markdown(range, cx)).collect())
                    .collect();
                let column_weights = table_column_weights(structure, &snapshot);
                render_table_block(
                    structure.clone(),
                    header_markdown,
                    rows_markdown,
                    column_weights,
                    weak_editor.clone(),
                    marker.range.clone(),
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
                    image_cache.clone(),
                )
            }
            BlockRenderKind::Rule => render_rule_block(
                weak_editor.clone(),
                marker.range.clone(),
                marker.indent_columns,
            ),
            BlockRenderKind::Frontmatter => {
                render_frontmatter_block(weak_editor.clone(), marker.range.clone(), source.clone())
            }
            BlockRenderKind::Math { source } => {
                let text_color = cx.theme().colors().editor_foreground;
                request_math_render(
                    MathKey {
                        source: SharedString::from(source.clone()),
                        style: MathStyle::Display,
                        color: u32::from(gpui::Rgba::from(text_color)),
                    },
                    text_color,
                    cx,
                );
                render_math_block(
                    weak_editor.clone(),
                    marker.range.clone(),
                    SharedString::from(source.clone()),
                    below,
                )
            }
        };
        let placement = if below {
            BlockPlacement::Below(marker.range.end)
        } else {
            BlockPlacement::Replace(marker.range.start..=marker.range.end)
        };
        blocks_to_insert.push(BlockProperties {
            placement,
            height: Some(marker.height_estimate),
            style: BlockStyle::Flex,
            render,
            priority: 0,
        });
        pending_applied.push((marker.range.clone(), source, below));
    }

    if !block_ids_to_remove.is_empty() {
        editor.remove_blocks(block_ids_to_remove, None, cx);
    }
    if !blocks_to_insert.is_empty() {
        let block_ids = editor.insert_blocks(blocks_to_insert, None, cx);
        for ((range, source, below), block_id) in pending_applied.into_iter().zip(block_ids) {
            new_applied_blocks.push(AppliedBlock {
                range,
                source,
                below,
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

/// Identity of a rendered formula. Two formulas that agree on every field
/// rasterize identically, so the cache is shared across editors: the same
/// equation in two panes is typeset once.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MathKey {
    source: SharedString,
    style: MathStyle,
    /// Packed RGBA, since `Hsla` is not hashable and the color is baked into
    /// the SVG's fill.
    color: u32,
}

enum MathEntry {
    /// A background render is in flight; callers show the LaTeX source.
    Pending,
    Ready {
        image: Arc<gpui::RenderImage>,
        /// Fraction of the image's height that sits below the text baseline.
        baseline_fraction: f32,
        /// Width of the image in ems, used to size the inline element so it
        /// takes exactly the space the glyphs occupy.
        width_em: f32,
        height_em: f32,
    },
    /// The formula does not parse. Callers keep showing the source, which is
    /// also what the user needs to see in order to fix it.
    Failed,
}

/// Rasterized formulas, keyed by content.
///
/// This is a global rather than per-editor state because rendering depends
/// only on [`MathKey`], and because the placeholder closure that reads it
/// receives just an `&mut App`.
#[derive(Default)]
struct MathCache {
    entries: HashMap<MathKey, MathEntry>,
}

impl gpui::Global for MathCache {}

/// Em size the SVG is rasterized at. Larger than any realistic buffer font so
/// the outlines stay crisp when the element scales them down to the line's
/// actual size.
const MATH_RASTER_EM: f32 = 64.0;

/// Formulas render at this multiple of the buffer font size. The KaTeX fonts
/// have a visibly smaller x-height than code fonts, so at 1:1 math looks
/// shrunken next to prose; this is the same ratio KaTeX's own stylesheet
/// applies (`.katex { font-size: 1.21em }`).
const MATH_FONT_SCALE: f32 = 1.21;

/// Ensures a render for `key` is in flight or complete, and returns whether
/// the cache already holds a finished image.
///
/// Called from `apply_decorations` rather than from the placeholder closure:
/// kicking off work during render would spawn a task on every frame.
fn request_math_render(key: MathKey, text_color: Hsla, cx: &mut App) {
    if cx.default_global::<MathCache>().entries.contains_key(&key) {
        return;
    }
    cx.global_mut::<MathCache>()
        .entries
        .insert(key.clone(), MathEntry::Pending);

    let svg_renderer = cx.svg_renderer();
    let theme = MathTheme {
        text_color,
        font_size: MATH_RASTER_EM,
    };
    cx.spawn(async move |cx| {
        let rendered = cx
            .background_spawn({
                let key = key.clone();
                async move {
                    let rendered = math_render::render_to_svg(&key.source, key.style, &theme)?;
                    let image = svg_renderer
                        .render_single_frame(rendered.svg.as_bytes(), 1.0)
                        .map_err(|error| anyhow::anyhow!("{error}"))?;
                    anyhow::Ok((rendered, image))
                }
            })
            .await;

        cx.update(|cx| {
            let entry = match rendered {
                Ok((rendered, image)) => {
                    let total_em = rendered.height_em + rendered.depth_em;
                    let size = image.size(0);
                    let width_em = if total_em > 0.0 && size.height.0 > 0 {
                        size.width.0 as f32 / size.height.0 as f32 * total_em
                    } else {
                        0.0
                    };
                    MathEntry::Ready {
                        image,
                        baseline_fraction: rendered.baseline_fraction(),
                        width_em,
                        height_em: total_em,
                    }
                }
                Err(_) => MathEntry::Failed,
            };
            cx.global_mut::<MathCache>().entries.insert(key, entry);
            // The placeholder closures read the cache during render, so every
            // open editor needs a repaint to pick the new image up.
            cx.refresh_windows();
        });
    })
    .detach();
}

fn fold_placeholder(marker: &InlineMarker, editor: WeakEntity<Editor>) -> FoldPlaceholder {
    // Pure hides collapse to zero-width text; bullets and checkboxes keep the
    // default placeholder text, whose visual is replaced by the rendered
    // element at its measured width.
    let collapsed_text = match &marker.kind {
        InlineKind::Hide { .. } => Some(SharedString::new_static("")),
        InlineKind::Bullet | InlineKind::Checkbox { .. } | InlineKind::Math { .. } => None,
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
        InlineKind::Math { source, style } => {
            let source = source.clone();
            let style = *style;
            Arc::new(move |_, _, cx: &mut App| {
                let theme_settings = theme_settings::ThemeSettings::get_global(cx);
                let font_size = theme_settings.buffer_font_size(cx);
                let buffer_font = theme_settings.buffer_font.clone();
                let text_color = cx.theme().colors().editor_foreground;
                let key = MathKey {
                    source: source.clone(),
                    style,
                    color: u32::from(gpui::Rgba::from(text_color)),
                };
                let line_height = font_size * theme_settings.line_height();
                let text_system = cx.text_system().clone();
                let font_id = text_system.resolve_font(&buffer_font);
                let ascent = text_system.ascent(font_id, font_size);
                // `TextSystem::descent` is negative on macOS (a signed offset
                // below the baseline), while the painted baseline's formula
                // (`gpui::paint_line`) works in magnitudes — feeding the
                // signed value into `baseline_offset` lands 2×descent too
                // low, so compute the baseline from magnitudes directly.
                let descent = text_system.descent(font_id, font_size).abs();
                let text_baseline = (line_height - ascent - descent) / 2.0 + ascent;

                match cx.default_global::<MathCache>().entries.get(&key) {
                    Some(MathEntry::Ready {
                        image,
                        baseline_fraction,
                        width_em,
                        height_em,
                    }) => {
                        let math_em = font_size * MATH_FONT_SCALE;
                        let height = math_em * *height_em;
                        let width = math_em * *width_em;
                        // The editor centers inline elements in the line
                        // (element top lands at `(line_height - height) / 2`),
                        // while text baselines sit at `baseline_offset`. Shift
                        // the image by the difference so the formula's
                        // baseline lands exactly on the text's.
                        let formula_ascent = height * (1.0 - *baseline_fraction);
                        let shift = text_baseline - (line_height - height) / 2.0 - formula_ascent;
                        // The centering offsets a padded element by half its
                        // padding, so doubling the needed shift as one-sided
                        // padding moves the image by exactly `shift` without
                        // relying on inset positioning.
                        let (pad_top, pad_bottom) = if shift >= gpui::px(0.) {
                            (shift * 2.0, gpui::px(0.))
                        } else {
                            (gpui::px(0.), shift * -2.0)
                        };
                        div()
                            .h(height + pad_top + pad_bottom)
                            .w(width)
                            .pt(pad_top)
                            .pb(pad_bottom)
                            .child(
                                img(ImageSource::Render(image.clone()))
                                    .h(height)
                                    .w(width),
                            )
                            .into_any_element()
                    }
                    // While a render is in flight — and permanently for a
                    // formula that does not parse — fall back to the LaTeX
                    // source, so the text never disappears out from under
                    // the user.
                    Some(MathEntry::Pending) | Some(MathEntry::Failed) | None => div()
                        .font(buffer_font)
                        .text_size(font_size)
                        .text_color(text_color)
                        .child(source.clone())
                        .into_any_element(),
                }
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
        // Editing a formula changes what its placeholder must draw even though
        // its range is unchanged, so the source has to take part in the key or
        // the concealment is reused with a stale image.
        InlineKind::Math { source, style } => {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = collections::FxHasher::default();
            source.hash(&mut hasher);
            style.hash(&mut hasher);
            // Keep clear of the discriminants above.
            4 + hasher.finish()
        }
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

/// Click-to-reveal for a block widget: puts the cursor at the block's first
/// offset, which makes `recompute` drop the widget and show the raw markdown.
///
/// Clicks that a child button already claimed are skipped. `ButtonLike` calls
/// `window.prevent_default()` on left mouse down for exactly this reason, and
/// the editor's own `mouse_left_down` honors it — without the same check here,
/// pressing a rendered code block's copy button tore the widget down before
/// the mouse up could complete the click, so it revealed source instead of
/// copying.
fn reveal_source_on_mouse_down(
    editor: WeakEntity<Editor>,
    start: Anchor,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        if window.default_prevented() {
            return;
        }
        editor
            .update(cx, |editor, cx| {
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let offset = start.to_offset(&snapshot);
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([offset..offset]);
                });
            })
            .log_err();
    }
}

fn render_markdown_block(
    markdown: Entity<Markdown>,
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    base_directory: Option<PathBuf>,
    indent_columns: u32,
    image_cache: Entity<RetainAllImageCache>,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let style = block_markdown_style(block_cx.window, block_cx.app);
        let editor = editor.clone();
        let start = range.start;
        let base_directory = base_directory.clone();
        let image_cache = image_cache.clone();
        let gutter_width =
            block_cx.margins.gutter.full_width() + block_cx.em_width * indent_columns as f32;
        let max_width = block_cx.max_width;
        div()
            .pl(gutter_width)
            .w(max_width)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                reveal_source_on_mouse_down(editor, start),
            )
            .child(
                MarkdownElement::new(markdown.clone(), style).image_resolver(
                    move |destination, _cx| {
                        resolve_image_source(destination, base_directory.as_deref(), &image_cache)
                    },
                ),
            )
            .into_any_element()
    })
}

/// Per-column flex weights approximating content-based column sizing.
/// Byte ranges of CommonMark code spans: a run of N backticks opens one and the
/// next run of exactly N backticks closes it. An unclosed run opens nothing.
fn code_span_ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let open_start = index;
        while index < bytes.len() && bytes[index] == b'`' {
            index += 1;
        }
        let run = index - open_start;
        let mut search = index;
        while let Some(next) = text[search..].find('`') {
            let close_start = search + next;
            let mut close_end = close_start;
            while close_end < bytes.len() && bytes[close_end] == b'`' {
                close_end += 1;
            }
            if close_end - close_start == run {
                spans.push(open_start..close_end);
                index = close_end;
                break;
            }
            search = close_end;
        }
    }
    spans
}

/// Wikilinks are concealed on the buffer text by `scan_wikilinks`, but a table
/// cell is a replace block rendered from extracted text by the markdown crate,
/// which has no wikilink support — so the raw `[[..]]` would reach the screen.
/// Rewrite them to the same display text concealment shows. Embeds (`![[..]]`)
/// and code spans are left raw, matching `scan_wikilinks`.
fn wikilink_display_text(text: &str) -> Cow<'_, str> {
    if !text.contains("[[") {
        return Cow::Borrowed(text);
    }
    let code_spans = code_span_ranges(text);
    let mut out = String::new();
    let mut copied = 0;
    let mut search_from = 0;
    while let Some(open_offset) = text[search_from..].find("[[") {
        let open = search_from + open_offset;
        let Some(close_offset) = text[open + 2..].find("]]") else {
            break;
        };
        let close = open + 2 + close_offset;
        search_from = close + 2;

        let inner = &text[open + 2..close];
        if inner.is_empty() || inner.contains('\n') || inner.contains("[[") {
            continue;
        }
        if text[..open].ends_with('!') {
            continue;
        }
        if code_spans
            .iter()
            .any(|span| span.start < close + 2 && open < span.end)
        {
            continue;
        }

        // `[[target|alias]]` shows the alias; everything else shows the inner
        // text verbatim, so `[[Note#heading]]` keeps its heading.
        let display = match inner.find('|') {
            Some(pipe) => &inner[pipe + 1..],
            None => inner,
        };
        out.push_str(&text[copied..open]);
        out.push_str(display);
        copied = close + 2;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

fn table_column_weights(structure: &TableStructure, snapshot: &MultiBufferSnapshot) -> Vec<f32> {
    let columns = structure.header.len().max(
        structure
            .rows
            .iter()
            .map(|row| row.len())
            .max()
            .unwrap_or(0),
    );
    let mut weights = vec![3.0_f32; columns];
    let mut measure = |cells: &[Range<Anchor>]| {
        for (index, range) in cells.iter().enumerate() {
            let start = range.start.to_offset(snapshot);
            let end = range.end.to_offset(snapshot);
            let length: usize = snapshot
                .text_for_range(start..end)
                .map(|chunk| chunk.trim().chars().count())
                .sum();
            if let Some(weight) = weights.get_mut(index) {
                *weight = weight.max((length as f32).clamp(3., 60.));
            }
        }
    };
    measure(&structure.header);
    for row in &structure.rows {
        measure(row);
    }
    weights
}

/// Starts editing a table cell: mounts a focused single-line editor over it,
/// committing on enter/tab/blur and cancelling on escape.
fn start_cell_edit(
    weak_editor: WeakEntity<Editor>,
    cell_range: Range<Anchor>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(main_editor) = weak_editor.upgrade() else {
        return;
    };
    // Commit any cell already being edited.
    main_editor.update(cx, |editor, cx| commit_active_cell(editor, cx));

    // Re-resolve the clicked cell from live text: the widget's captured
    // ranges go stale whenever an edit lands before the widget refreshes.
    let snapshot = main_editor.read(cx).buffer().read(cx).snapshot(cx);
    let stale_start = cell_range.start.to_offset(&snapshot);
    let Some((_, structure)) = parse_table_at(&snapshot, stale_start) else {
        return;
    };
    let all_cells = structure.cells_in_order();
    let Some(cell_range) = all_cells
        .iter()
        .find(|candidate| {
            let start = candidate.start.to_offset(&snapshot);
            let end = candidate.end.to_offset(&snapshot);
            start <= stale_start && stale_start <= end
        })
        .cloned()
    else {
        return;
    };
    let start = cell_range.start.to_offset(&snapshot);
    let end = cell_range.end.to_offset(&snapshot);
    let text: String = snapshot.text_for_range(start..end).collect();

    let cell_editor = cx.new(|cx| {
        let mut editor = Editor::single_line(window, cx);
        editor.set_text(text.trim(), window, cx);
        editor
    });

    let mut subscriptions = Vec::new();
    main_editor.update(cx, |_, cx| {
        subscriptions.push(cx.subscribe(
            &cell_editor,
            |editor, blurred, event: &EditorEvent, cx| {
                // Only commit if this editor is still the active cell: when
                // Tab moves editing to the next cell, the old editor's blur
                // must not commit (and clear) the new one.
                if matches!(event, EditorEvent::Blurred)
                    && editor
                        .addon::<LivePreviewAddon>()
                        .and_then(|addon| addon.active_cell.as_ref())
                        .is_some_and(|cell| cell.editor == blurred)
                {
                    commit_active_cell(editor, cx);
                }
            },
        ));
    });

    let next_cell = all_cells
        .iter()
        .position(|candidate| {
            candidate.start.to_offset(&snapshot) == start
                && candidate.end.to_offset(&snapshot) == end
        })
        .and_then(|index| all_cells.get(index + 1).cloned());

    cell_editor.update(cx, |editor, _| {
        let weak = weak_editor.clone();
        subscriptions.push(editor.register_action::<editor::actions::Newline>(
            move |_, window, cx| {
                weak.update(cx, |editor, cx| {
                    commit_active_cell(editor, cx);
                    refocus_main_editor(editor, window, cx);
                })
                .log_err();
            },
        ));
        let weak = weak_editor.clone();
        subscriptions.push(
            editor.register_action::<editor::actions::Tab>(move |_, window, cx| {
                let next = next_cell.clone();
                weak.update(cx, |editor, cx| {
                    commit_active_cell(editor, cx);
                })
                .log_err();
                match next {
                    Some(next) => start_cell_edit(weak.clone(), next, window, cx),
                    None => {
                        weak.update(cx, |editor, cx| refocus_main_editor(editor, window, cx))
                            .log_err();
                    }
                }
            }),
        );
        let weak = weak_editor.clone();
        subscriptions.push(editor.register_action::<editor::actions::Cancel>(
            move |_, window, cx| {
                weak.update(cx, |editor, cx| {
                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                        addon.active_cell = None;
                    }
                    refocus_main_editor(editor, window, cx);
                    cx.notify();
                })
                .log_err();
            },
        ));
    });

    let focus_handle = cell_editor.read(cx).focus_handle(cx);
    window.focus(&focus_handle, cx);

    main_editor.update(cx, |editor, cx| {
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.selected_table_unit = None;
            addon.active_cell = Some(ActiveTableCell {
                cell_range,
                editor: cell_editor,
                _subscriptions: subscriptions,
            });
        }
        cx.notify();
    });
}

fn refocus_main_editor(editor: &Editor, window: &mut Window, cx: &mut Context<Editor>) {
    let focus_handle = editor.focus_handle(cx);
    window.focus(&focus_handle, cx);
}

/// Records the insertion boundary the pointer indicates, notifying only on
/// change.
fn set_drop_boundary(
    weak_editor: &WeakEntity<Editor>,
    table_range: &Range<Anchor>,
    boundary: TableBoundary,
    cx: &mut App,
) {
    weak_editor
        .update(cx, |editor, cx| {
            if let Some(addon) = editor.addon_mut::<LivePreviewAddon>()
                && addon.drop_boundary.as_ref().map(|(_, current)| *current) != Some(boundary)
            {
                addon.drop_boundary = Some((table_range.clone(), boundary));
                cx.notify();
            }
        })
        .log_err();
}

/// Marks the unit a handle drag just started from; the widget outlines it
/// in place while the drag is active.
fn record_drag_source(
    weak_editor: &WeakEntity<Editor>,
    table_range: &Range<Anchor>,
    unit: TableUnit,
    cx: &mut App,
) {
    weak_editor
        .update(cx, |editor, cx| {
            if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                addon.selected_table_unit = None;
                addon.drag_source = Some(TableUnitSelection {
                    table_range: table_range.clone(),
                    unit,
                });
                addon.drop_boundary = None;
            }
            cx.notify();
        })
        .log_err();
}

/// Deletes the row/column currently selected via its handle, if any.
/// Returns false when there is no selection so the caller can propagate.
fn delete_selected_table_unit(weak_editor: &WeakEntity<Editor>, cx: &mut App) -> bool {
    let Some(editor) = weak_editor.upgrade() else {
        return false;
    };
    let Some(selection) = editor.read_with(cx, |editor, _| {
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.selected_table_unit.as_ref())
            .map(|selection| (selection.table_range.clone(), selection.unit))
    }) else {
        return false;
    };
    let (table_range, unit) = selection;
    editor.update(cx, |editor, cx| {
        let change = match unit {
            TableUnit::Row(row) => TableStructuralChange::DeleteRow(row),
            TableUnit::Column(column) => TableStructuralChange::DeleteColumn(column),
        };
        apply_table_structural_change(editor, &table_range, change, cx);
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.selected_table_unit = None;
        }
        cx.notify();
    });
    true
}

/// Writes the active cell editor's text back into the table source.
fn commit_active_cell(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(active) = editor
        .addon_mut::<LivePreviewAddon>()
        .and_then(|addon| addon.active_cell.take())
    else {
        return;
    };
    let text = active
        .editor
        .read(cx)
        .text(cx)
        .replace('\n', " ")
        .replace('|', "\\|");
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let start = active.cell_range.start.to_offset(&snapshot);
    let end = active.cell_range.end.to_offset(&snapshot);
    if start > end {
        return;
    }
    let current: String = snapshot.text_for_range(start..end).collect();
    if current.trim() == text.trim() {
        cx.notify();
        return;
    }
    let replacement = format!(" {} ", text.trim());
    editor.buffer().update(cx, |multibuffer, cx| {
        multibuffer.edit([(start..end, replacement)], None, cx);
    });
    cx.notify();
}

/// Splits one table row line into cell ranges at unescaped pipes (the GFM
/// rule). Each range spans the full text between two pipes, padding included,
/// so a cell edit can rewrite it in place. Segments outside the outer pipes
/// are kept only when non-blank (tables without outer pipes).
fn split_table_row_text(
    text: &str,
    base_offset: usize,
    snapshot: &MultiBufferSnapshot,
) -> Vec<Range<Anchor>> {
    let mut boundaries = Vec::new();
    let mut escaped = false;
    for (offset, byte) in text.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'|' {
            boundaries.push(base_offset + offset);
        }
    }
    let mut segments = Vec::new();
    let mut segment_start = base_offset;
    for boundary in boundaries {
        segments.push(segment_start..boundary);
        segment_start = boundary + 1;
    }
    segments.push(segment_start..base_offset + text.len());
    let is_blank = |range: &Range<usize>| {
        text.get(range.start - base_offset..range.end - base_offset)
            .is_none_or(|segment| segment.trim().is_empty())
    };
    if segments.len() > 1 && is_blank(&segments[0]) {
        segments.remove(0);
    }
    if segments.len() > 1 && segments.last().is_some_and(is_blank) {
        segments.pop();
    }
    segments
        .into_iter()
        .map(|range| {
            // Outward bias: the range must survive its own rewrite when a
            // cell edit commits.
            snapshot.anchor_before(MultiBufferOffset(range.start))
                ..snapshot.anchor_after(MultiBufferOffset(range.end))
        })
        .collect()
}

fn is_delimiter_row(line: &str) -> bool {
    let mut cells = line
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .peekable();
    cells.peek().is_some()
        && cells.all(|cell| {
            let cell = cell.trim();
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            !dashes.is_empty() && dashes.bytes().all(|byte| byte == b'-')
        })
}

/// Parses the pipe table containing `near` straight from buffer text. Widget
/// click handlers must use this instead of the syntax tree: reparses are
/// asynchronous, so right after an edit the tree describes text that no
/// longer exists.
fn parse_table_at(
    snapshot: &MultiBufferSnapshot,
    near: MultiBufferOffset,
) -> Option<(Range<MultiBufferOffset>, TableStructure)> {
    let near = near.min(snapshot.len());
    let click_row = near.to_point(snapshot).row;
    let max_row = snapshot.max_point().row;
    let line_at = |row: u32| -> (String, MultiBufferOffset, MultiBufferOffset) {
        let start = Point::new(row, 0).to_offset(snapshot);
        let end = Point::new(row, snapshot.line_len(MultiBufferRow(row))).to_offset(snapshot);
        (snapshot.text_for_range(start..end).collect(), start, end)
    };
    let is_table_line = |row: u32| -> bool {
        let (text, ..) = line_at(row);
        !text.trim().is_empty() && text.contains('|')
    };

    if !is_table_line(click_row) {
        return None;
    }
    let mut first_row = click_row;
    while first_row > 0 && is_table_line(first_row - 1) {
        first_row -= 1;
    }
    let mut last_row = click_row;
    while last_row < max_row && is_table_line(last_row + 1) {
        last_row += 1;
    }
    // The block may over-extend into adjacent prose that happens to contain a
    // pipe; anchor on the delimiter row and take the line above as header.
    let delimiter_row =
        (first_row + 1..=last_row).find(|row| is_delimiter_row(&line_at(*row).0))?;
    let header_row = delimiter_row - 1;

    let split_row = |row: u32| -> Vec<Range<Anchor>> {
        let (text, start, _) = line_at(row);
        split_table_row_text(&text, start.0, snapshot)
    };
    let header = split_row(header_row);
    if header.is_empty() {
        return None;
    }
    let alignments = split_row(delimiter_row)
        .into_iter()
        .map(|range| {
            let start = range.start.to_offset(snapshot);
            let end = range.end.to_offset(snapshot);
            let text: String = snapshot.text_for_range(start..end).collect();
            let text = text.trim();
            match (text.starts_with(':'), text.ends_with(':')) {
                (true, true) => CellAlignment::Center,
                (true, false) => CellAlignment::Left,
                (false, true) => CellAlignment::Right,
                (false, false) => CellAlignment::None,
            }
        })
        .collect();
    let mut rows: Vec<Vec<Range<Anchor>>> = (delimiter_row + 1..=last_row).map(split_row).collect();

    let mut header = header;
    let columns = header
        .len()
        .max(rows.iter().map(|row| row.len()).max().unwrap_or(0));
    let sentinel = || Anchor::Min..Anchor::Min;
    header.resize_with(columns, sentinel);
    for row in &mut rows {
        row.resize_with(columns, sentinel);
    }

    let table_start = Point::new(header_row, 0).to_offset(snapshot);
    let table_end =
        Point::new(last_row, snapshot.line_len(MultiBufferRow(last_row))).to_offset(snapshot);
    Some((
        table_start..table_end,
        TableStructure {
            header,
            alignments,
            rows,
        },
    ))
}

#[derive(Clone, Copy)]
enum TableStructuralChange {
    AddColumn,
    AddRow,
    MoveRow {
        from: usize,
        to: usize,
    },
    MoveColumn {
        from: usize,
        to: usize,
    },
    DeleteRow(usize),
    DeleteColumn(usize),
    /// Rewrites the table without structural additions, materializing any
    /// cells the source omitted.
    Normalize,
}

/// Finds the table currently at `table_range` by parsing live buffer text.
fn fresh_table_at(
    editor: &Editor,
    table_range: &Range<Anchor>,
    cx: &App,
) -> Option<(Range<MultiBufferOffset>, TableStructure)> {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    parse_table_at(&snapshot, table_range.start.to_offset(&snapshot))
        .or_else(|| parse_table_at(&snapshot, table_range.end.to_offset(&snapshot)))
}

/// Rebuilds the whole table source with an added row or column, normalized.
/// Resolves the table fresh at call time rather than trusting the caller's
/// captured structure.
fn apply_table_structural_change(
    editor: &mut Editor,
    stale_table_range: &Range<Anchor>,
    change: TableStructuralChange,
    cx: &mut Context<Editor>,
) {
    commit_active_cell(editor, cx);
    let Some((table_offsets, structure)) = fresh_table_at(editor, stale_table_range, cx) else {
        log::warn!(
            "markdown live preview: no table found at click position; ignoring structural change"
        );
        return;
    };
    let structure = &structure;
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let cell_text = |range: &Range<Anchor>| -> String {
        let start = range.start.to_offset(&snapshot);
        let end = range.end.to_offset(&snapshot);
        let text: String = snapshot.text_for_range(start..end).collect();
        text.trim().to_string()
    };

    let mut header: Vec<String> = structure.header.iter().map(&cell_text).collect();
    let mut alignments = structure.alignments.clone();
    let mut rows: Vec<Vec<String>> = structure
        .rows
        .iter()
        .map(|row| row.iter().map(&cell_text).collect())
        .collect();

    match change {
        TableStructuralChange::AddColumn => {
            header.push(String::new());
            alignments.push(CellAlignment::None);
            for row in &mut rows {
                row.push(String::new());
            }
        }
        TableStructuralChange::AddRow => {
            rows.push(vec![String::new(); header.len()]);
        }
        TableStructuralChange::MoveRow { from, to } => {
            if from < rows.len() && to < rows.len() {
                let row = rows.remove(from);
                rows.insert(to, row);
            }
        }
        TableStructuralChange::MoveColumn { from, to } => {
            if from < header.len() && to < header.len() {
                let cell = header.remove(from);
                header.insert(to, cell);
                if from < alignments.len() && to < alignments.len() {
                    let alignment = alignments.remove(from);
                    alignments.insert(to, alignment);
                }
                for row in &mut rows {
                    if from < row.len() && to < row.len() {
                        let cell = row.remove(from);
                        row.insert(to, cell);
                    }
                }
            }
        }
        TableStructuralChange::DeleteRow(index) => {
            if index < rows.len() {
                rows.remove(index);
            }
        }
        TableStructuralChange::DeleteColumn(index) => {
            if header.len() > 1 && index < header.len() {
                header.remove(index);
                if index < alignments.len() {
                    alignments.remove(index);
                }
                for row in &mut rows {
                    if index < row.len() {
                        row.remove(index);
                    }
                }
            }
        }
        TableStructuralChange::Normalize => {}
    }

    let columns = header.len();
    alignments.resize(columns, CellAlignment::None);
    for row in &mut rows {
        row.resize(columns, String::new());
    }

    let format_row = |cells: &[String]| -> String {
        let mut line = String::from("|");
        for cell in cells {
            line.push(' ');
            if cell.is_empty() {
                line.push(' ');
            } else {
                line.push_str(cell);
            }
            line.push_str(" |");
        }
        line
    };
    let delimiter: String = {
        let mut line = String::from("|");
        for alignment in &alignments {
            let marker = match alignment {
                CellAlignment::None => " --- ",
                CellAlignment::Left => " :-- ",
                CellAlignment::Center => " :-: ",
                CellAlignment::Right => " --: ",
            };
            line.push_str(marker);
            line.push('|');
        }
        line
    };

    let mut source = format_row(&header);
    source.push('\n');
    source.push_str(&delimiter);
    for row in &rows {
        source.push('\n');
        source.push_str(&format_row(row));
    }

    editor.buffer().update(cx, |multibuffer, cx| {
        multibuffer.edit([(table_offsets.start..table_offsets.end, source)], None, cx);
    });
}

/// The editable table widget: rendered grid, click-to-edit cells, and
/// add-row/add-column affordances.
#[allow(clippy::too_many_arguments)]
fn render_table_block(
    structure: TableStructure,
    header_markdown: Vec<Entity<Markdown>>,
    rows_markdown: Vec<Vec<Entity<Markdown>>>,
    column_weights: Vec<f32>,
    editor: WeakEntity<Editor>,
    table_range: Range<Anchor>,
    indent_columns: u32,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let style = {
            let mut style = block_markdown_style(block_cx.window, block_cx.app);
            style.height_is_multiple_of_line_height = true;
            style
        };
        let gutter_width =
            block_cx.margins.gutter.full_width() + block_cx.em_width * indent_columns as f32;
        let max_width = block_cx.max_width;
        let colors = block_cx.app.theme().colors().clone();

        let (active_range, active_editor) = editor
            .upgrade()
            .and_then(|entity| {
                let editor_ref = entity.read(block_cx.app);
                let snapshot = editor_ref
                    .buffer()
                    .read(block_cx.app)
                    .snapshot(block_cx.app);
                editor_ref
                    .addon::<LivePreviewAddon>()
                    .and_then(|addon| addon.active_cell.as_ref())
                    .map(|cell| {
                        (
                            Some(
                                cell.cell_range.start.to_offset(&snapshot).0
                                    ..cell.cell_range.end.to_offset(&snapshot).0,
                            ),
                            Some(cell.editor.clone()),
                        )
                    })
            })
            .unwrap_or((None, None));
        let resolved = editor.upgrade().map(|entity| {
            entity
                .read(block_cx.app)
                .buffer()
                .read(block_cx.app)
                .snapshot(block_cx.app)
        });
        let (selected_unit, drag_source_unit, drop_boundary) = editor
            .upgrade()
            .zip(resolved.as_ref())
            .and_then(|(entity, snapshot)| {
                let addon = entity.read(block_cx.app).addon::<LivePreviewAddon>()?;
                let table_start = table_range.start.to_offset(snapshot);
                let unit_in_this_table = |selection: &Option<TableUnitSelection>| {
                    selection
                        .as_ref()
                        .filter(|selection| {
                            selection.table_range.start.to_offset(snapshot) == table_start
                        })
                        .map(|selection| selection.unit)
                };
                let dragging = block_cx.app.has_active_drag();
                let boundary = addon
                    .drop_boundary
                    .as_ref()
                    .filter(|(range, _)| range.start.to_offset(snapshot) == table_start)
                    .map(|(_, boundary)| *boundary)
                    .filter(|_| dragging);
                Some((
                    unit_in_this_table(&addon.selected_table_unit),
                    unit_in_this_table(&addon.drag_source).filter(|_| dragging),
                    boundary,
                ))
            })
            .unwrap_or((None, None, None));

        let render_cell = |cell_range: &Range<Anchor>,
                           markdown: &Entity<Markdown>,
                           column: usize,
                           data_row: Option<usize>| {
            let is_header = data_row.is_none();
            let unit_covers_cell = |unit: Option<TableUnit>| match unit {
                Some(TableUnit::Row(row)) => data_row == Some(row),
                Some(TableUnit::Column(unit_column)) => column == unit_column,
                None => false,
            };
            let in_selected_unit = unit_covers_cell(selected_unit);
            // The dragged unit gets the tint only: recoloring its borders
            // reads as a second insertion line.
            let is_drag_source_cell = unit_covers_cell(drag_source_unit);
            // Full-height line exactly on the insertion boundary. Boundary b
            // draws on the left edge of column b; the end boundary draws on
            // the right edge of the last column.
            let column_insertion = match (drag_source_unit, drop_boundary) {
                (Some(TableUnit::Column(_)), Some(TableBoundary::Column(boundary))) => {
                    if boundary == column {
                        Some(false)
                    } else if boundary == structure.header.len()
                        && column + 1 == structure.header.len()
                    {
                        Some(true)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let weight = column_weights.get(column).copied().unwrap_or(8.);
            let is_active = match (&resolved, &active_range) {
                (Some(snapshot), Some(active)) => {
                    let start = cell_range.start.to_offset(snapshot).0;
                    let end = cell_range.end.to_offset(snapshot).0;
                    *active == (start..end)
                }
                _ => false,
            };
            let alignment = structure
                .alignments
                .get(column)
                .copied()
                .unwrap_or(CellAlignment::None);

            let is_sentinel = cell_range.start == Anchor::Min && cell_range.end == Anchor::Min;
            let mut cell = div()
                .debug_selector(|| {
                    format!(
                        "mdlp-cell-{}-{column}",
                        data_row
                            .map(|row| row.to_string())
                            .unwrap_or_else(|| "h".into())
                    )
                })
                .flex_grow(1.)
                .flex_basis(gpui::px(weight * 8.))
                .min_w(gpui::px(48.))
                .px_2()
                .py_1()
                .min_h(block_cx.line_height + gpui::px(10.))
                .border_r_1()
                .border_b_1()
                .when(column == 0, |this| this.border_l_1())
                .when(is_header, |this| this.border_t_1())
                .border_color(colors.border_variant)
                .flex()
                .items_center()
                .map(|this| match alignment {
                    CellAlignment::Center => this.justify_center(),
                    CellAlignment::Right => this.justify_end(),
                    _ => this,
                })
                .when(is_header, |this| {
                    this.bg(colors.elevated_surface_background)
                        .font_weight(FontWeight::BOLD)
                })
                .when(in_selected_unit, |this| {
                    this.bg(colors.element_selected)
                        .border_color(colors.border_focused)
                })
                .when(is_drag_source_cell, |this| this.bg(colors.element_selected))
                .when_some(column_insertion, |this, after| {
                    // Overlay, not a border: borders recolor the cell's own
                    // grid lines and shift layout; an absolute bar draws one
                    // continuous line on the boundary.
                    let bar = div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(gpui::px(3.))
                        .bg(colors.border_focused);
                    this.relative().child(if after {
                        bar.right(gpui::px(-2.))
                    } else {
                        bar.left(gpui::px(-2.))
                    })
                })
                .on_drag_move::<TableColumnDrag>({
                    let weak = editor.clone();
                    let cell_table_range = table_range.clone();
                    move |event, _, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        if event.drag(cx).table_start != cell_table_range.start {
                            return;
                        }
                        // Pointer-side precision: the near half of a column
                        // targets the boundary before it, the far half after.
                        let after = event.event.position.x > event.bounds.center().x;
                        let boundary = TableBoundary::Column(column + usize::from(after));
                        set_drop_boundary(&weak, &cell_table_range, boundary, cx);
                    }
                });

            if is_active && let Some(active_editor) = active_editor.clone() {
                cell = cell.child(div().w_full().child(active_editor));
            } else if is_sentinel {
                // Clicking a cell the source omitted first rewrites the table
                // in normalized form so the cell exists to edit.
                let weak = editor.clone();
                let normalize_range = table_range.clone();
                cell =
                    cell.cursor_text()
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                            cx.stop_propagation();
                            weak.update(cx, |editor, cx| {
                                apply_table_structural_change(
                                    editor,
                                    &normalize_range,
                                    TableStructuralChange::Normalize,
                                    cx,
                                );
                            })
                            .log_err();
                        });
            } else {
                let weak = editor.clone();
                let cell_range = cell_range.clone();
                cell = cell
                    // `min_w_0` is load-bearing: as a flex item the markdown
                    // container would otherwise take its min-content width,
                    // which for text measured without a definite width is the
                    // whole unwrapped line. The cell itself still shrinks to
                    // its column share, so long content spills over the border
                    // instead of wrapping.
                    .child(
                        div()
                            .min_w_0()
                            .child(MarkdownElement::new(markdown.clone(), style.clone())),
                    )
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        start_cell_edit(weak.clone(), cell_range.clone(), window, cx);
                    });
            }
            cell
        };

        let handle_width = gpui::px(14.);
        let record_press = {
            let weak = editor.clone();
            move |event: &gpui::MouseDownEvent, _: &mut Window, cx: &mut App| {
                // Record only — no notify. A re-render here would tear down
                // the per-frame listeners that arm the drag gesture.
                let position = event.position;
                weak.update(cx, |editor, _| {
                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                        addon.handle_press = Some(position);
                    }
                })
                .log_err();
            }
        };
        let select_unit = |unit: TableUnit| {
            let weak = editor.clone();
            let unit_table_range = table_range.clone();
            move |event: &gpui::MouseUpEvent, _: &mut Window, cx: &mut App| {
                weak.update(cx, |editor, cx| {
                    let Some(press) = editor
                        .addon_mut::<LivePreviewAddon>()
                        .and_then(|addon| addon.handle_press.take())
                    else {
                        return;
                    };
                    // A real drag ends far from where it started; only a
                    // click (press + release in place) selects.
                    if (event.position - press).magnitude() > 4. {
                        return;
                    }
                    commit_active_cell(editor, cx);
                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                        let already = addon
                            .selected_table_unit
                            .as_ref()
                            .is_some_and(|selection| selection.unit == unit);
                        addon.selected_table_unit = (!already).then(|| TableUnitSelection {
                            table_range: unit_table_range.clone(),
                            unit,
                        });
                    }
                    cx.notify();
                })
                .log_err();
            }
        };
        let handle_pill = |selected: bool| {
            div()
                .rounded_sm()
                .bg(if selected {
                    colors.border_focused
                } else {
                    colors.element_hover
                })
                .when(!selected, |this| {
                    this.opacity(0.)
                        .group_hover("mdlp-table", |this| this.opacity(1.))
                })
        };

        let mut grid = v_flex().flex_grow(1.);
        // Column handles.
        grid = grid.child(h_flex().child(div().w(handle_width)).children(
            structure.header.iter().enumerate().map(|(column, _)| {
                let weight = column_weights.get(column).copied().unwrap_or(8.);
                let selected = selected_unit == Some(TableUnit::Column(column));
                div()
                    .id(("mdlp-column-handle", column))
                    .debug_selector(|| format!("mdlp-column-handle-{column}"))
                    .flex_grow(1.)
                    .flex_basis(gpui::px(weight * 8.))
                    .min_w(gpui::px(48.))
                    .h(gpui::px(10.))
                    .px_2()
                    .cursor_pointer()
                    .child(
                        handle_pill(selected)
                            .w_full()
                            .h(gpui::px(4.))
                            .mt(gpui::px(3.)),
                    )
                    .on_mouse_down(MouseButton::Left, record_press.clone())
                    .on_mouse_up(MouseButton::Left, select_unit(TableUnit::Column(column)))
                    .on_drag_move::<TableColumnDrag>({
                        let weak = editor.clone();
                        let strip_table_range = table_range.clone();
                        move |event, _, cx| {
                            if !event.bounds.contains(&event.event.position) {
                                return;
                            }
                            if event.drag(cx).table_start != strip_table_range.start {
                                return;
                            }
                            let after = event.event.position.x > event.bounds.center().x;
                            let boundary = TableBoundary::Column(column + usize::from(after));
                            set_drop_boundary(&weak, &strip_table_range, boundary, cx);
                        }
                    })
                    .on_drag(
                        TableColumnDrag {
                            table_start: table_range.start,
                            column,
                        },
                        {
                            let weak = editor.clone();
                            let drag_table_range = table_range.clone();
                            move |_, _, _, cx| {
                                record_drag_source(
                                    &weak,
                                    &drag_table_range,
                                    TableUnit::Column(column),
                                    cx,
                                );
                                cx.new(|_| EmptyDragPreview)
                            }
                        },
                    )
            }),
        ));
        let row_handle = |data_row: Option<usize>| {
            let container = div().w(handle_width).py_1().pr(gpui::px(4.)).flex();
            match data_row {
                None => container.into_any_element(),
                Some(row_index) => {
                    let selected = selected_unit == Some(TableUnit::Row(row_index));
                    container
                        .id(("mdlp-row-handle", row_index))
                        .debug_selector(|| format!("mdlp-row-handle-{row_index}"))
                        .cursor_pointer()
                        .child(handle_pill(selected).w(gpui::px(4.)).h_full())
                        .on_mouse_down(MouseButton::Left, record_press.clone())
                        .on_mouse_up(MouseButton::Left, select_unit(TableUnit::Row(row_index)))
                        .on_drag(
                            TableRowDrag {
                                table_start: table_range.start,
                                row: row_index,
                            },
                            {
                                let weak = editor.clone();
                                let drag_table_range = table_range.clone();
                                move |_, _, _, cx| {
                                    record_drag_source(
                                        &weak,
                                        &drag_table_range,
                                        TableUnit::Row(row_index),
                                        cx,
                                    );
                                    cx.new(|_| EmptyDragPreview)
                                }
                            },
                        )
                        .into_any_element()
                }
            }
        };
        let rows_len = structure.rows.len();
        let row_track_drag = |data_row: Option<usize>| {
            let weak = editor.clone();
            let row_table_range = table_range.clone();
            move |event: &gpui::DragMoveEvent<TableRowDrag>, _: &mut Window, cx: &mut App| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                if event.drag(cx).table_start != row_table_range.start {
                    return;
                }
                // Rows can only land below the header, so the header always
                // targets boundary 0; data rows use pointer-side precision.
                let boundary = match data_row {
                    None => 0,
                    Some(row) => {
                        let after = event.event.position.y > event.bounds.center().y;
                        row + usize::from(after)
                    }
                };
                set_drop_boundary(&weak, &row_table_range, TableBoundary::Row(boundary), cx);
            }
        };
        // Boundary b sits above data row b; b == 0 is the header's bottom
        // edge and b == rows_len the last row's bottom edge. `Some(true)`
        // draws at the container's bottom, `Some(false)` at its top.
        let row_insertion = |data_row: Option<usize>| {
            let boundary = match (drag_source_unit, drop_boundary) {
                (Some(TableUnit::Row(_)), Some(TableBoundary::Row(boundary))) => boundary,
                _ => return None,
            };
            match data_row {
                None => (boundary == 0).then_some(true),
                Some(row) => {
                    if boundary == row && row > 0 {
                        Some(false)
                    } else if boundary == rows_len && row + 1 == rows_len {
                        Some(true)
                    } else {
                        None
                    }
                }
            }
        };
        let accent = colors.border_focused;
        grid = grid.child(
            h_flex()
                .items_stretch()
                .when_some(row_insertion(None), |this, after| {
                    let bar = div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .h(gpui::px(3.))
                        .bg(accent);
                    this.relative().child(if after {
                        bar.bottom(gpui::px(-2.))
                    } else {
                        bar.top(gpui::px(-2.))
                    })
                })
                .on_drag_move::<TableRowDrag>(row_track_drag(None))
                .child(row_handle(None))
                .child(
                    h_flex().items_stretch().flex_grow(1.).children(
                        header_markdown
                            .iter()
                            .enumerate()
                            .map(|(column, markdown)| {
                                let empty = Range {
                                    start: Anchor::Min,
                                    end: Anchor::Min,
                                };
                                let range = structure.header.get(column).unwrap_or(&empty);
                                render_cell(range, markdown, column, None)
                            }),
                    ),
                ),
        );
        for (row_index, row_markdown) in rows_markdown.iter().enumerate() {
            grid = grid.child(
                h_flex()
                    .items_stretch()
                    .when_some(row_insertion(Some(row_index)), |this, after| {
                        let bar = div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .h(gpui::px(3.))
                            .bg(accent);
                        this.relative().child(if after {
                            bar.bottom(gpui::px(-2.))
                        } else {
                            bar.top(gpui::px(-2.))
                        })
                    })
                    .on_drag_move::<TableRowDrag>(row_track_drag(Some(row_index)))
                    .child(row_handle(Some(row_index)))
                    .child(h_flex().items_stretch().flex_grow(1.).children(
                        row_markdown.iter().enumerate().map(|(column, markdown)| {
                            let empty = Range {
                                start: Anchor::Min,
                                end: Anchor::Min,
                            };
                            let range = structure
                                .rows
                                .get(row_index)
                                .and_then(|row| row.get(column))
                                .unwrap_or(&empty);
                            render_cell(range, markdown, column, Some(row_index))
                        }),
                    )),
            );
        }

        let add_column_editor = editor.clone();
        let add_column_range = table_range.clone();
        let add_row_editor = editor.clone();
        let add_row_range = table_range.clone();
        let reveal_source_editor = editor.clone();
        let reveal_source_range = table_range.clone();

        let unit_button =
            |id: &'static str,
             icon: IconName,
             action: Option<(TableStructuralChange, Option<TableUnit>)>| {
                let weak = editor.clone();
                let action_range = table_range.clone();
                let enabled = action.is_some();
                div()
                    .id(id)
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .child(Icon::new(icon).size(IconSize::XSmall).color(if enabled {
                        Color::Muted
                    } else {
                        Color::Disabled
                    }))
                    .when_some(action, move |this, (change, new_unit)| {
                        this.cursor_pointer()
                            .hover(|this| this.bg(colors.element_hover))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                weak.update(cx, |editor, cx| {
                                    apply_table_structural_change(
                                        editor,
                                        &action_range,
                                        change,
                                        cx,
                                    );
                                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                                        addon.selected_table_unit =
                                            new_unit.map(|unit| TableUnitSelection {
                                                table_range: action_range.clone(),
                                                unit,
                                            });
                                    }
                                    cx.notify();
                                })
                                .log_err();
                            })
                    })
            };
        let controls = selected_unit.map(|unit| {
            let rows_len = structure.rows.len();
            let columns_len = structure.header.len();
            let delete = match unit {
                TableUnit::Row(row) => {
                    (row < rows_len).then_some((TableStructuralChange::DeleteRow(row), None))
                }
                TableUnit::Column(column) => (columns_len > 1 && column < columns_len)
                    .then_some((TableStructuralChange::DeleteColumn(column), None)),
            };
            h_flex().gap_1().pb_1().pl(handle_width).child(unit_button(
                "mdlp-unit-delete",
                IconName::Trash,
                delete,
            ))
        });

        let apply_boundary_drop = |weak: WeakEntity<Editor>,
                                   container_table_range: Range<Anchor>,
                                   from_unit: TableUnit| {
            move |cx: &mut App| {
                weak.update(cx, |editor, cx| {
                    let Some(boundary) = editor
                        .addon::<LivePreviewAddon>()
                        .and_then(|addon| addon.drop_boundary.as_ref())
                        .filter(|(range, _)| range.start == container_table_range.start)
                        .map(|(_, boundary)| *boundary)
                    else {
                        return;
                    };
                    // Boundary b means "insert before index b". Removing the
                    // source first shifts later indices down by one; the two
                    // boundaries flanking the source are no-ops.
                    let move_to = |from: usize, boundary: usize| {
                        if boundary == from || boundary == from + 1 {
                            None
                        } else if boundary > from {
                            Some(boundary - 1)
                        } else {
                            Some(boundary)
                        }
                    };
                    let (change, unit) = match (from_unit, boundary) {
                        (TableUnit::Row(from), TableBoundary::Row(boundary)) => {
                            match move_to(from, boundary) {
                                Some(to) => (
                                    TableStructuralChange::MoveRow { from, to },
                                    TableUnit::Row(to),
                                ),
                                None => return,
                            }
                        }
                        (TableUnit::Column(from), TableBoundary::Column(boundary)) => {
                            match move_to(from, boundary) {
                                Some(to) => (
                                    TableStructuralChange::MoveColumn { from, to },
                                    TableUnit::Column(to),
                                ),
                                None => return,
                            }
                        }
                        _ => return,
                    };
                    apply_table_structural_change(editor, &container_table_range, change, cx);
                    if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
                        addon.drag_source = None;
                        addon.drop_boundary = None;
                        addon.selected_table_unit = Some(TableUnitSelection {
                            table_range: container_table_range.clone(),
                            unit,
                        });
                    }
                    cx.notify();
                })
                .log_err();
            }
        };
        let container_row_drop = {
            let weak = editor.clone();
            let container_table_range = table_range.clone();
            move |drag: &TableRowDrag, _: &mut Window, cx: &mut App| {
                if drag.table_start != container_table_range.start {
                    return;
                }
                apply_boundary_drop(
                    weak.clone(),
                    container_table_range.clone(),
                    TableUnit::Row(drag.row),
                )(cx);
            }
        };
        let container_column_drop = {
            let weak = editor.clone();
            let container_table_range = table_range.clone();
            move |drag: &TableColumnDrag, _: &mut Window, cx: &mut App| {
                if drag.table_start != container_table_range.start {
                    return;
                }
                apply_boundary_drop(
                    weak.clone(),
                    container_table_range.clone(),
                    TableUnit::Column(drag.column),
                )(cx);
            }
        };

        div()
            .pl(gutter_width)
            .w(max_width)
            .group("mdlp-table")
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_drop::<TableRowDrag>(container_row_drop)
            .on_drop::<TableColumnDrag>(container_column_drop)
            .child(
                v_flex()
                    .max_w(max_width * 0.95)
                    .children(controls)
                    .child(
                        h_flex().items_stretch().child(grid).child(
                            v_flex()
                                .w(gpui::px(22.))
                                .child(
                                    // Reveal the table's markdown source.
                                    div()
                                        .id("mdlp-table-source")
                                        .h(gpui::px(22.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(colors.text_muted)
                                        .opacity(0.)
                                        .group_hover("mdlp-table", |this| this.opacity(0.7))
                                        .hover(|this| this.opacity(1.))
                                        .child(
                                            Icon::new(IconName::Code)
                                                .size(IconSize::XSmall)
                                                .color(Color::Muted),
                                        )
                                        .tooltip(ui::Tooltip::text("Edit table source"))
                                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                            cx.stop_propagation();
                                            reveal_source_editor
                                                .update(cx, |editor, cx| {
                                                    if let Some(addon) =
                                                        editor.addon_mut::<LivePreviewAddon>()
                                                    {
                                                        addon.source_revealed =
                                                            Some(reveal_source_range.clone());
                                                    }
                                                    let snapshot =
                                                        editor.buffer().read(cx).snapshot(cx);
                                                    let offset = reveal_source_range
                                                        .start
                                                        .to_offset(&snapshot);
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
                                )
                                .child(
                                    // Add column to the right.
                                    div()
                                        .id("mdlp-add-column")
                                        .flex_grow(1.)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(colors.text_muted)
                                        .opacity(0.)
                                        .group_hover("mdlp-table", |this| this.opacity(0.7))
                                        .hover(|this| this.opacity(1.))
                                        .child("+")
                                        .tooltip(ui::Tooltip::text("Add column to the right"))
                                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                            cx.stop_propagation();
                                            add_column_editor
                                                .update(cx, |editor, cx| {
                                                    apply_table_structural_change(
                                                        editor,
                                                        &add_column_range,
                                                        TableStructuralChange::AddColumn,
                                                        cx,
                                                    );
                                                })
                                                .log_err();
                                        }),
                                ),
                        ),
                    )
                    .child(
                        // Add row below.
                        div()
                            .id("mdlp-add-row")
                            .h(gpui::px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(colors.text_muted)
                            .opacity(0.)
                            .group_hover("mdlp-table", |this| this.opacity(0.7))
                            .hover(|this| this.opacity(1.))
                            .child("+")
                            .tooltip(ui::Tooltip::text("Add row below"))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                add_row_editor
                                    .update(cx, |editor, cx| {
                                        apply_table_structural_change(
                                            editor,
                                            &add_row_range,
                                            TableStructuralChange::AddRow,
                                            cx,
                                        );
                                    })
                                    .log_err();
                            }),
                    ),
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
    image_cache: Entity<RetainAllImageCache>,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let mut style = block_markdown_style(block_cx.window, block_cx.app);
        // Suppress the paragraph's trailing margin so the selection border
        // hugs the image instead of leaving a gap beneath it.
        style.height_is_multiple_of_line_height = true;
        let start = range.start;
        let base_directory = base_directory.clone();
        let image_cache = image_cache.clone();
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
                let snapshot = editor_ref
                    .buffer()
                    .read(block_cx.app)
                    .snapshot(block_cx.app);
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
        let reveal_range = range.clone();
        let drag_editor = editor.clone();
        let drag_range = range.clone();

        // A direct image element lets the selection border hug the image
        // exactly; reference-style images (no inline destination) fall back
        // to the markdown renderer.
        let resolved = destination.as_deref().and_then(|destination| {
            resolve_image_source(destination, base_directory.as_deref(), &image_cache)
        });
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
                .image_resolver(move |destination, _cx| {
                    resolve_image_source(destination, base_directory.as_deref(), &image_cache)
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
            .when(content_width.is_none(), |this| this.max_w(max_width * 0.66))
            .child(image_content);

        if selected {
            content = content
                .child(
                    // Reveal-source button, top right.
                    div().absolute().top_1().right_1().child(
                        IconButton::new("mdlp-show-source", IconName::Code)
                            .style(ButtonStyle::Filled)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                reveal_editor
                                    .update(cx, |editor, cx| {
                                        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>()
                                        {
                                            addon.source_revealed = Some(reveal_range.clone());
                                        }
                                        let snapshot = editor.buffer().read(cx).snapshot(cx);
                                        let offset = start.to_offset(&snapshot);
                                        editor.change_selections(
                                            Default::default(),
                                            window,
                                            cx,
                                            |selections| {
                                                selections.select_ranges([offset..offset]);
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
                let width =
                    (event.event.position.x - event.bounds.left() - drag.content_left_offset)
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
            .on_mouse_down(
                MouseButton::Left,
                reveal_source_on_mouse_down(editor, start),
            )
            .child(div().flex_1().h(gpui::px(2.)).bg(border_color))
            .into_any_element()
    })
}

/// Renders display math as a centered formula.
///
/// In replace mode (`below == false`) the widget stands in for the source
/// lines, so while the render is pending — or permanently, if the LaTeX does
/// not parse — it shows the source text instead: the buffer content must
/// never disappear. In below mode the source lines are already visible above
/// the widget, so those states render nothing.
fn render_math_block(
    editor: WeakEntity<Editor>,
    range: Range<Anchor>,
    source: SharedString,
    below: bool,
) -> RenderBlock {
    Arc::new(move |block_cx| {
        let editor = editor.clone();
        let start = range.start;
        let cx = &mut *block_cx.app;
        let theme_settings = theme_settings::ThemeSettings::get_global(cx);
        let font_size = theme_settings.buffer_font_size(cx);
        let buffer_font = theme_settings.buffer_font.clone();
        let text_color = cx.theme().colors().editor_foreground;
        let key = MathKey {
            source: source.clone(),
            style: MathStyle::Display,
            color: u32::from(gpui::Rgba::from(text_color)),
        };

        let content = match cx.default_global::<MathCache>().entries.get(&key) {
            Some(MathEntry::Ready {
                image,
                width_em,
                height_em,
                ..
            }) => {
                let math_em = font_size * MATH_FONT_SCALE;
                let height = math_em * *height_em;
                let width = math_em * *width_em;
                img(ImageSource::Render(image.clone()))
                    .h(height)
                    .w(width)
                    .into_any_element()
            }
            _ if below => Empty.into_any_element(),
            _ => div()
                .font(buffer_font)
                .text_size(font_size)
                .text_color(text_color)
                .child(source.clone())
                .into_any_element(),
        };

        div()
            .w(block_cx.max_width)
            .py(block_cx.line_height * 0.25)
            .flex()
            .justify_center()
            .items_center()
            .when(!below, |this| {
                this.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    reveal_source_on_mouse_down(editor, start),
                )
            })
            .child(content)
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
            .on_mouse_down(
                MouseButton::Left,
                reveal_source_on_mouse_down(editor, start),
            )
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

/// Extensions live preview treats as images. Shared by the `![[embed]]`
/// scanner and the on-disk change watcher so the two agree on what counts.
fn is_image_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp"
    )
}

fn resolve_image_source(
    destination: &str,
    base_directory: Option<&std::path::Path>,
    image_cache: &Entity<RetainAllImageCache>,
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
    // Canonicalizing proves the file exists *and* collapses `..` and symlinks,
    // so a note that spells its image `../images/x.png` keys the cache by the
    // same path the worktree reports when that file changes. Without it the two
    // spellings hash differently and the eviction below silently matches
    // nothing — which is the common case, since a shared `images/` folder
    // beside the notes is an ordinary vault layout.
    let path = std::fs::canonicalize(&path).ok()?;
    // Deliberately not `ImageSource::Resource`: that reads through gpui's
    // app-level asset cache, which has no eviction, so a file rewritten in
    // place would keep serving its first-decoded bitmap. Routing every local
    // image through the editor's own cache is what makes the eviction in
    // `register_editor` possible.
    let resource = Resource::Path(Arc::from(path.as_path()));
    let image_cache = image_cache.clone();
    Some(ImageSource::Custom(Arc::new(move |window, cx| {
        image_cache.update(cx, |image_cache, cx| {
            image_cache.load(&resource, window, cx)
        })
    })))
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
        last_table_end_row: None,
        inline: Vec::new(),
        blocks: Vec::new(),
        strikethrough: Vec::new(),
        italic: Vec::new(),
        bold: Vec::new(),
        link_text: Vec::new(),
        definitions: Vec::new(),
        definition_ranges: Vec::new(),
        ordered_markers: Vec::new(),
        citations: Vec::new(),
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
    extraction.scan_citations();

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
        citations,
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
        citations,
    })
}

struct Extraction<'a> {
    text: &'a str,
    snapshot: &'a MultiBufferSnapshot,
    /// Prose regions (the block grammar's `inline` nodes) and code spans,
    /// used to scan for wikilinks only where they can occur.
    prose_regions: Vec<Range<usize>>,
    code_spans: Vec<Range<usize>>,
    /// End row of the last table block pushed, for deduplicating table nodes
    /// that fall inside an already-claimed textual table.
    last_table_end_row: Option<u32>,
    inline: Vec<InlineMarker>,
    blocks: Vec<BlockMarker>,
    strikethrough: Vec<Range<Anchor>>,
    italic: Vec<Range<Anchor>>,
    bold: Vec<Range<Anchor>>,
    link_text: Vec<Range<Anchor>>,
    definitions: Vec<String>,
    definition_ranges: Vec<Range<Anchor>>,
    ordered_markers: Vec<Range<Anchor>>,
    citations: Vec<Range<Anchor>>,
}

impl Extraction<'_> {
    fn anchor_range(&self, range: Range<usize>) -> Range<Anchor> {
        // Bias the anchors inward so text inserted at the boundaries falls
        // outside the hidden range rather than growing it.
        self.snapshot.anchor_after(MultiBufferOffset(range.start))
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

    /// Claims a `latex_block` node (`$x$` or `$$x$$`) as a math marker
    /// spanning the delimiters as well as the body, so concealing it replaces
    /// the whole construct with one typeset image.
    ///
    /// The grammar treats a run of one or more `$` as the delimiter, so the
    /// opening run's length is what separates inline from display math.
    /// Following Obsidian, `$...$` requires a non-space immediately inside
    /// each delimiter — so prose like "cost $5 and $10" stays prose — while
    /// `$$...$$` tolerates padding. Display math that sits alone on its lines
    /// becomes a centered block; mid-line `$$...$$` renders inline (in
    /// compact text style) because a block replacement would swallow its
    /// neighbors and display-style layout cannot fit within a line.
    fn latex(&mut self, node: tree_sitter::Node) {
        let mut delimiters = Vec::new();
        for index in 0..node.child_count() as u32 {
            let Some(child) = node.child(index) else {
                continue;
            };
            if child.kind() == "latex_span_delimiter" {
                delimiters.push(child);
            }
        }
        // An unterminated `$` parses without a closing delimiter; leave it as
        // plain text so typing a lone dollar sign does not flicker.
        let (Some(open), Some(close)) = (delimiters.first(), delimiters.last()) else {
            return;
        };
        if delimiters.len() < 2 || open.end_byte() > close.start_byte() {
            return;
        }

        let Some(source) = self.text.get(open.end_byte()..close.start_byte()) else {
            return;
        };
        if source.trim().is_empty() {
            return;
        }

        let display = open.byte_range().len() >= 2;
        if !display
            && (source.starts_with(|c: char| c.is_whitespace())
                || source.ends_with(|c: char| c.is_whitespace()))
        {
            return;
        }

        if display && self.node_is_alone_on_its_lines(node) {
            let (start_row, end_row) = self.node_rows(node);
            let height_estimate = (end_row - start_row + 1).max(2);
            self.push_block_rows(
                start_row,
                end_row,
                height_estimate,
                BlockRenderKind::Math {
                    source: source.to_string(),
                },
            );
            return;
        }

        self.inline.push(InlineMarker {
            range: self.anchor_range(node.byte_range()),
            kind: InlineKind::Math {
                source: SharedString::from(source.to_string()),
                // Mid-line `$$...$$` also uses inline (text) style: display
                // style stacks limits and full-size fractions, which cannot
                // fit within a fixed-height editor line.
                style: MathStyle::Inline,
            },
        });
    }

    /// Whether only whitespace shares the node's first and last lines with it.
    fn node_is_alone_on_its_lines(&self, node: tree_sitter::Node) -> bool {
        let line_start = self.text[..node.start_byte()]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let line_end = self.text[node.end_byte()..]
            .find('\n')
            .map_or(self.text.len(), |index| node.end_byte() + index);
        let before = self.text.get(line_start..node.start_byte());
        let after = self.text.get(node.end_byte()..line_end);
        match (before, after) {
            (Some(before), Some(after)) => {
                before.chars().all(char::is_whitespace) && after.chars().all(char::is_whitespace)
            }
            _ => false,
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
                    // Extent and structure come from the text parser, not the
                    // tree: tree-sitter-md chokes on valid GFM like a row of
                    // all-empty cells, ending the table node early.
                    let near = MultiBufferOffset(node.start_byte());
                    match parse_table_at(self.snapshot, near) {
                        Some((offsets, structure)) => {
                            let start_row = offsets.start.to_point(self.snapshot).row;
                            let end_row = offsets.end.to_point(self.snapshot).row;
                            // One textual table can contain several table
                            // nodes; only the first one produces the block.
                            let claimed = self
                                .last_table_end_row
                                .is_some_and(|last| start_row <= last);
                            if !claimed {
                                self.last_table_end_row = Some(end_row);
                                self.push_block_rows(
                                    start_row,
                                    end_row,
                                    end_row - start_row + 2,
                                    BlockRenderKind::Table(structure),
                                );
                            }
                        }
                        None => {
                            let (start_row, end_row) = self.node_rows(node);
                            self.push_block_rows(
                                start_row,
                                end_row,
                                end_row - start_row + 2,
                                BlockRenderKind::Markdown,
                            );
                        }
                    }
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
                "latex_block" => {
                    self.latex(node);
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
                    // `embed_image_block` (via `scan_wikilinks`) renders it.
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
                if self
                    .code_spans
                    .iter()
                    .any(|span| span.start < end && start < span.end)
                {
                    continue;
                }
                if is_embed {
                    self.embed_image_block(start - 1, end, inner);
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

    /// Renders an Obsidian image embed (`![[photo.png]]`, with Obsidian's
    /// `![[photo.png|640]]` width syntax) as an image block when it is the
    /// only content on its line. The target resolves like a regular markdown
    /// image destination — relative to the buffer's directory. Non-image
    /// embeds (`![[Some Note]]`) and inline embeds stay raw.
    fn embed_image_block(&mut self, embed_start: usize, embed_end: usize, inner: &str) {
        let (target, size) = match inner.split_once('|') {
            Some((target, size)) => (target.trim(), Some(size)),
            None => (inner.trim(), None),
        };
        let is_image = std::path::Path::new(target)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(is_image_extension);
        if !is_image {
            return;
        }
        let row = self
            .snapshot
            .offset_to_point(MultiBufferOffset(embed_start))
            .row;
        let line_start = self.snapshot.point_to_offset(Point::new(row, 0));
        let line_end = self
            .snapshot
            .point_to_offset(Point::new(row, self.snapshot.line_len(MultiBufferRow(row))));
        let line_text: String = self.snapshot.text_for_range(line_start..line_end).collect();
        let embed_text = self.text.get(embed_start..embed_end).unwrap_or_default();
        if line_text.trim() != embed_text.trim() {
            return;
        }
        let display_width = size
            .map(|size| {
                size.chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>()
            })
            .and_then(|width| width.parse::<f32>().ok())
            .filter(|width| *width > 0.);
        self.push_block_rows(
            row,
            row,
            8,
            BlockRenderKind::Image {
                display_width,
                destination: Some(target.to_string()),
                alt: String::new(),
            },
        );
    }

    /// Conceal pandoc-style citations: `[@key]`, `[-@key]`, and citation
    /// groups like `[see @doe2020, p. 3; also @roe2021]`. The markdown
    /// grammar has no citation nodes (it parses `[@key]` as a shortcut
    /// link), so like wikilinks this scans the prose regions directly,
    /// skipping code spans. Brackets that are really links (`[@key](url)`,
    /// `[@key][label]`) are left to the link machinery, and brackets with no
    /// valid `@key` are left raw.
    fn scan_citations(&mut self) {
        let regions = std::mem::take(&mut self.prose_regions);
        for region in &regions {
            let Some(region_text) = self.text.get(region.clone()) else {
                continue;
            };
            let mut search_from = 0;
            while let Some(open_offset) = region_text[search_from..].find('[') {
                let open = search_from + open_offset;
                search_from = open + 1;
                let Some(close_offset) = region_text[open + 1..].find(']') else {
                    break;
                };
                let close = open + 1 + close_offset;
                let inner = &region_text[open + 1..close];
                if inner.is_empty() || inner.contains('\n') || inner.contains('[') {
                    continue;
                }
                // A preceding `[`, `!`, or `]` makes this bracket part of a
                // wikilink, image, or reference link's label instead.
                if matches!(
                    region_text[..open].chars().last(),
                    Some('[') | Some('!') | Some(']')
                ) {
                    continue;
                }
                // `[...](url)` and `[...][label]` are links, not citations.
                if matches!(
                    region_text[close + 1..].chars().next(),
                    Some('(') | Some('[')
                ) {
                    continue;
                }
                let keys = citation_keys(inner);
                if keys.is_empty() {
                    continue;
                }
                let start = region.start + open;
                let end = region.start + close + 1;
                if self
                    .code_spans
                    .iter()
                    .any(|span| span.start < end && start < span.end)
                {
                    continue;
                }

                let reveal = start..end;
                self.hide(start..start + 1, reveal.clone());
                self.hide(end - 1..end, reveal.clone());
                for key in keys {
                    let range = self.anchor_range(start + 1 + key.start..start + 1 + key.end);
                    self.citations.push(range);
                }
                search_from = close + 1;
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

/// Byte ranges (relative to a citation group's inner text, `@` included) of
/// the valid pandoc citation keys it contains. An `@` only starts a key at an
/// item boundary — the group start, after whitespace, `;`, or a `-` (author
/// suppression) — so an email address in brackets does not read as a
/// citation. Key syntax follows pandoc: a leading alphanumeric or `_`, then
/// alphanumerics with internal punctuation.
fn citation_keys(inner: &str) -> Vec<Range<usize>> {
    let mut keys = Vec::new();
    for (at, _) in inner.match_indices('@') {
        let boundary = match inner[..at].chars().last() {
            None => true,
            Some(character) => character.is_whitespace() || character == ';' || character == '-',
        };
        if !boundary {
            continue;
        }
        let after = &inner[at + 1..];
        let mut length = 0;
        for character in after.chars() {
            let internal_punctuation = matches!(
                character,
                ':' | '.' | '#' | '$' | '%' | '&' | '-' | '+' | '?' | '<' | '>' | '~' | '/'
            );
            if character.is_ascii_alphanumeric() || character == '_' || internal_punctuation {
                length += character.len_utf8();
            } else {
                break;
            }
        }
        // Punctuation is only valid inside a key, not at its edges.
        let key = after[..length].trim_end_matches(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_')
        });
        if key.is_empty()
            || !key
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        keys.push(at..at + 1 + key.len());
    }
    keys
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
