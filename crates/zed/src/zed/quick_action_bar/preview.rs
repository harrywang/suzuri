use editor::{Editor, MultiBuffer};
use gpui::{AnyElement, Entity, Focusable as _, Modifiers};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use svg_preview::svg_preview_view::SvgPreviewView;
use tabular_data_preview::TabularDataPreviewPane;
use ui::{Tooltip, prelude::*, text_for_keystroke};

use super::QuickActionBar;

enum PreviewTarget {
    Markdown(Entity<Editor>),
    Svg(Entity<MultiBuffer>),
    TabularData(Entity<Editor>),
    /// Carries its own label: Typst and LaTeX preview as a PDF in a split,
    /// HTML opens in the browser.
    Typeset(Entity<Editor>, &'static str),
}

impl QuickActionBar {
    pub fn render_preview_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Resolve against this toolbar's own pane item rather than the
        // workspace's focused item, so each pane's button reflects and
        // targets the content of the pane it belongs to.
        let active_item = self.active_item.as_ref()?;
        let editor = active_item.act_as::<Editor>(cx);

        let preview_target = if let Some(editor) = &editor
            && MarkdownPreviewView::is_markdown_file(editor, cx)
        {
            PreviewTarget::Markdown(editor.clone())
        } else if let Some(buffer) = active_item.act_as::<MultiBuffer>(cx)
            && SvgPreviewView::is_svg_file(&buffer, cx)
        {
            PreviewTarget::Svg(buffer)
        } else if let Some(editor) = &editor
            && TabularDataPreviewPane::is_tabular_data_file(editor, cx)
        {
            PreviewTarget::TabularData(editor.clone())
        } else if let Some(editor) = editor
            && let Some(label) = typeset_preview::preview_label_for_editor(&editor, cx)
        {
            PreviewTarget::Typeset(editor, label)
        } else {
            return None;
        };

        let (button_id, tooltip_text, open_action_for_tooltip) = match &preview_target {
            PreviewTarget::Markdown(_) => (
                "toggle-markdown-preview",
                "Preview Markdown",
                &markdown_preview::OpenPreview as &dyn gpui::Action,
            ),
            PreviewTarget::Svg(_) => (
                "toggle-svg-preview",
                "Preview SVG",
                &svg_preview::OpenPreview as &dyn gpui::Action,
            ),
            PreviewTarget::TabularData(_) => (
                "toggle-tabular-preview",
                "Preview Tabular Data",
                &tabular_data_preview::OpenPreview as &dyn gpui::Action,
            ),
            PreviewTarget::Typeset(_, label) => (
                "open-typeset-preview",
                *label,
                &typeset_preview::OpenLivePreview as &dyn gpui::Action,
            ),
        };

        let alt_click = gpui::Keystroke {
            key: "click".into(),
            modifiers: Modifiers::alt(),
            ..Default::default()
        };

        // Obsidian-style source-mode toggle for markdown buffers: flips the
        // per-editor live preview off so the raw markdown shows.
        let source_mode_button = match &preview_target {
            PreviewTarget::Markdown(editor) => {
                let source_mode =
                    !markdown_live_preview::is_live_preview_enabled(editor.read(cx), cx);
                let editor = editor.clone();
                Some(
                    IconButton::new("toggle-markdown-source-mode", IconName::Code)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .toggle_state(source_mode)
                        .tooltip(move |_window, cx| {
                            Tooltip::for_action(
                                if source_mode {
                                    "Live Preview"
                                } else {
                                    "Source Mode"
                                },
                                &markdown_live_preview::ToggleLivePreview,
                                cx,
                            )
                        })
                        .on_click(move |_, window, cx| {
                            editor.read(cx).focus_handle(cx).dispatch_action(
                                &markdown_live_preview::ToggleLivePreview,
                                window,
                                cx,
                            );
                        }),
                )
            }
            _ => None,
        };

        let button = IconButton::new(button_id, IconName::Eye)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .tooltip(move |_window, cx| {
                Tooltip::with_meta(
                    tooltip_text,
                    Some(open_action_for_tooltip),
                    format!(
                        "{} to open in a split",
                        text_for_keystroke(&alt_click.modifiers, &alt_click.key, cx)
                    ),
                    cx,
                )
            })
            .on_click({
                let workspace_handle = self.workspace.clone();
                let active_item = active_item.boxed_clone();
                move |_, window, cx| {
                    let Some(workspace) = workspace_handle.upgrade() else {
                        return;
                    };
                    workspace.update(cx, |workspace, cx| {
                        let Some(pane) = workspace.pane_for(active_item.as_ref()) else {
                            return;
                        };
                        let open_to_the_side = window.modifiers().alt;
                        match &preview_target {
                            PreviewTarget::Markdown(editor) => {
                                let editor = editor.clone();
                                if open_to_the_side {
                                    MarkdownPreviewView::open_preview_to_the_side_of_pane(
                                        workspace, editor, pane, window, cx,
                                    );
                                } else {
                                    MarkdownPreviewView::open_preview_in_pane(
                                        workspace, editor, pane, window, cx,
                                    );
                                }
                            }
                            PreviewTarget::Svg(buffer) => {
                                let buffer = buffer.clone();
                                if open_to_the_side {
                                    SvgPreviewView::open_preview_to_the_side_of_pane(
                                        workspace, buffer, pane, window, cx,
                                    );
                                } else {
                                    SvgPreviewView::open_preview_in_pane(
                                        workspace, buffer, pane, window, cx,
                                    );
                                }
                            }
                            PreviewTarget::TabularData(editor) => {
                                let editor = editor.clone();
                                if open_to_the_side {
                                    TabularDataPreviewPane::open_preview_to_the_side_of_pane(
                                        workspace, editor, pane, window, cx,
                                    );
                                } else {
                                    TabularDataPreviewPane::open_preview_in_pane(
                                        editor, pane, window, cx,
                                    );
                                }
                            }
                            PreviewTarget::Typeset(editor, _) => {
                                typeset_preview::open_live_preview_for_editor(
                                    workspace,
                                    editor.clone(),
                                    window,
                                    cx,
                                );
                            }
                        }
                    });
                }
            });

        Some(
            h_flex()
                .children(source_mode_button)
                .child(button)
                .into_any_element(),
        )
    }
}
