use super::*;
use editor::test::editor_test_context::EditorTestContext;
use gpui::TestAppContext;
use language::{Language, LanguageConfig};
use settings::SettingsStore;
use std::sync::Arc;

fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zlog::init_test();
        let settings = SettingsStore::test(cx);
        cx.set_global(settings);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        crate::init(cx);
    });
}

fn markdown_inline_lang() -> Arc<Language> {
    Arc::new(Language::new(
        LanguageConfig {
            name: "Markdown-Inline".into(),
            hidden: true,
            ..LanguageConfig::default()
        },
        Some(tree_sitter_md::INLINE_LANGUAGE.into()),
    ))
}

async fn markdown_test_context(cx: &mut TestAppContext) -> EditorTestContext {
    init_test(cx);
    let mut cx = EditorTestContext::new(cx).await;
    let registry = cx.language_registry();
    let markdown = language::markdown_lang();
    registry.add(markdown.clone());
    registry.add(markdown_inline_lang());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(markdown), cx);
    });
    cx.executor().run_until_parked();
    cx
}

#[gpui::test]
async fn test_inline_markers_hidden_off_cursor_line(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain line
        some **bold** and *italic* and `code` here
        a [label](https://example.com) link
    "});
    cx.executor().run_until_parked();

    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some bold and italic and code here
            a label link
        "}
    );
}

#[gpui::test]
async fn test_per_token_reveal(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain line
        some **bold** and *italic* text
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some bold and italic text
        "}
    );

    // Cursor inside the bold span reveals only that span's markers; the
    // italic further along the same line stays rendered.
    cx.set_state(indoc::indoc! {"
        plain line
        some **boˇld** and *italic* text
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some **bold** and italic text
        "}
    );

    // Cursor in plain text on the same line reveals nothing.
    cx.set_state(indoc::indoc! {"
        plain line
        some **bold** anˇd *italic* text
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some bold and italic text
        "}
    );

    // A selection sweeping the line reveals everything it touches.
    cx.set_state(indoc::indoc! {"
        plain line
        «some **bold** and *italic* textˇ»
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some **bold** and *italic* text
        "}
    );

    // A list marker stays rendered while editing elsewhere on its line,
    // and reveals only when the cursor touches it.
    cx.set_state(indoc::indoc! {"
        plain line
        - bullet with **bold** insideˇ
    "});
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("⋯ bullet with bold inside"));
    cx.set_state(indoc::indoc! {"
        plain line
        ˇ- bullet with **bold** inside
    "});
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("- bullet with bold inside"));

    // The boundary just past the marker's trailing space does not reveal it.
    cx.set_state(indoc::indoc! {"
        plain line
        - ˇbullet with **bold** inside
    "});
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("⋯ bullet with bold inside"));

    // A checkbox stays rendered while editing the task text.
    cx.set_state(indoc::indoc! {"
        plain line
        - [ ] task textˇ
    "});
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("⋯ task text"));

    // Moving to another line hides everything again.
    cx.set_state(indoc::indoc! {"
        plainˇ line
        some **bold** and *italic* text
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            some bold and italic text
        "}
    );
}

#[gpui::test]
async fn test_list_markers(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain line
        - bullet item
        - [ ] open task
        - [x] done task
    "});
    cx.executor().run_until_parked();

    // Bullet and checkbox folds keep the default `⋯` placeholder text; the
    // rendered element replaces it visually.
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            ⋯ bullet item
            ⋯ open task
            ⋯ done task
        "}
    );

    let markers = cx.update_editor(|editor, _, cx| {
        let markers = extract_markers(editor, cx).expect("markdown buffer should produce markers");
        markers
            .inline
            .iter()
            .filter(|marker| matches!(marker.kind, InlineKind::Checkbox { .. }))
            .cloned()
            .collect::<Vec<_>>()
    });
    assert_eq!(markers.len(), 2);

    // Toggling the open task checks it.
    let InlineKind::Checkbox {
        checked,
        marker_range,
    } = markers[0].kind.clone()
    else {
        panic!("expected checkbox marker");
    };
    assert!(!checked);
    let editor = cx.editor.clone();
    cx.update(|_, cx| {
        toggle_task_marker(&editor.downgrade(), &marker_range, checked, cx);
    });
    cx.executor().run_until_parked();
    assert!(cx.buffer_text().contains("- [x] open task"));

    // Toggling the done task unchecks it.
    let InlineKind::Checkbox {
        checked,
        marker_range,
    } = markers[1].kind.clone()
    else {
        panic!("expected checkbox marker");
    };
    assert!(checked);
    let editor = cx.editor.clone();
    cx.update(|_, cx| {
        toggle_task_marker(&editor.downgrade(), &marker_range, checked, cx);
    });
    cx.executor().run_until_parked();
    assert!(cx.buffer_text().contains("- [ ] done task"));
}

#[gpui::test]
async fn test_block_widgets(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain first line
        extra spacing line

        # Heading

        | a | b |
        | - | - |
        | 1 | 2 |

        ---

        ```python
        import pandas as pd
        ```

        > a quote

        last line
    "});
    cx.executor().run_until_parked();

    // Heading, table, horizontal rule, code block, and blockquote.
    assert_eq!(applied_block_count(&mut cx), 5);

    // Moving the cursor onto the heading's row reveals it (removes its
    // block), while everything else stays rendered; an adjacent row does not
    // reveal it.
    cx.set_state(indoc::indoc! {"
        plain first line
        extra spacing line
        ˇ
        # Heading

        | a | b |
        | - | - |
        | 1 | 2 |

        ---

        ```python
        import pandas as pd
        ```

        > a quote

        last line
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 5);

    cx.set_state(indoc::indoc! {"
        plain first line
        extra spacing line

        # Headingˇ

        | a | b |
        | - | - |
        | 1 | 2 |

        ---

        ```python
        import pandas as pd
        ```

        > a quote

        last line
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 4);
}

#[gpui::test]
async fn test_frontmatter_renders_as_block(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ---
        title: Some Note
        parent: lectures
        ---

        body ˇtext
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // Cursor inside the frontmatter reveals the raw YAML.
    cx.set_state(indoc::indoc! {"
        ---
        title: Some Noteˇ
        parent: lectures
        ---

        body text
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 0);
}

fn applied_block_count(cx: &mut EditorTestContext) -> usize {
    cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .unwrap()
            .applied_blocks
            .len()
    })
}

#[gpui::test]
async fn test_keyboard_navigation_reaches_blocks(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇalpha

        # Heading

        omega
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // Arrow down until the cursor reaches the heading's buffer row; the
    // rendered block must dissolve so the heading is editable with the
    // keyboard alone.
    let mut reached = false;
    for _ in 0..4 {
        cx.update_editor(|editor, window, cx| {
            editor.move_down(&Default::default(), window, cx);
        });
        cx.executor().run_until_parked();
        let row = cx.update_editor(|editor, _, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            editor
                .selections
                .newest_anchor()
                .head()
                .to_point(&snapshot)
                .row
        });
        if row == 2 {
            reached = true;
            break;
        }
    }
    assert!(reached, "cursor never reached the heading row");
    assert_eq!(applied_block_count(&mut cx), 0);
    assert!(cx.display_text().contains("# Heading"));
}

#[gpui::test]
async fn test_heading_renders_after_typing(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    // Simulate typing a heading then pressing enter: the cursor ends up on
    // the line below, and the heading should render immediately.
    cx.set_state("# helloˇ");
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 0);

    cx.update_editor(|editor, window, cx| {
        editor.newline(&Default::default(), window, cx);
    });
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
}

#[gpui::test]
async fn test_disabling_restores_raw_markdown(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    let source = indoc::indoc! {"
        ˇplain line
        some **bold** text

        # Heading
    "};
    cx.set_state(source);
    cx.executor().run_until_parked();
    assert_ne!(cx.display_text(), cx.buffer_text());

    cx.update_editor(|editor, _window, cx| {
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.enabled_override = Some(false);
        }
        recompute(editor, cx);
    });
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(cx.display_text(), cx.buffer_text());

    cx.update_editor(|editor, _window, cx| {
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.enabled_override = Some(true);
        }
        recompute(editor, cx);
    });
    cx.executor().run_until_parked();
    assert_ne!(cx.display_text(), cx.buffer_text());
}

#[gpui::test]
async fn test_extended_markdown_coverage(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain line
        visit <https://example.com> or [text][ref] or [collapsed][]

        Setext Title
        ============

        <div>html block</div>
    "});
    cx.executor().run_until_parked();

    // Autolink angle brackets and reference-link syntax are concealed.
    assert!(
        cx.display_text()
            .contains("visit https://example.com or text or collapsed")
    );
    // Setext heading and HTML block render as widgets.
    assert_eq!(applied_block_count(&mut cx), 2);

    // Plain bracketed prose is NOT treated as a link: tree-sitter cannot
    // resolve reference definitions, so `[TODO]` must keep its brackets.
    cx.set_state("ˇplain line\nthis is [TODO] for later\n");
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("this is [TODO] for later"));

    // Mid-sentence images conceal their syntax, leaving the alt text.
    cx.set_state("ˇplain line\nbroken: ![missing image](nonexistent.png) here\n");
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("broken: missing image here"));
}

#[gpui::test]
async fn test_restored_generic_folds_are_removed(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇplain\nsome **bold** text\n");
    cx.executor().run_until_parked();

    // Simulate a fold restored from a session saved before concealment folds
    // were excluded from persistence: a default `⋯` placeholder sitting
    // exactly on the `**` marker before "bold".
    cx.update_editor(|editor, window, cx| {
        editor.fold_ranges(
            vec![MultiBufferOffset(11)..MultiBufferOffset(13)],
            false,
            window,
            cx,
        );
    });
    assert!(cx.display_text().contains('⋯'));

    // The next reparse heals it: the stale fold is removed and the marker is
    // concealed again.
    cx.set_state("editedˇ\nsome **bold** text\n");
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(cx.display_text(), "edited\nsome bold text\n");
}

#[gpui::test]
async fn test_non_markdown_buffers_untouched(cx: &mut TestAppContext) {
    init_test(cx);
    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state("ˇsome **text** that is not markdown\n");
    cx.executor().run_until_parked();

    pretty_assertions::assert_eq!(cx.display_text(), cx.buffer_text());
    let has_decorations = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .is_some_and(|addon| !addon.applied_blocks.is_empty())
    });
    assert!(!has_decorations);
}

#[gpui::test]
async fn test_linked_image_renders_as_block(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(
        "ˇplain line\n\n[![Clickable image](https://example.com/a.png)](https://example.com)\n",
    );
    cx.executor().run_until_parked();
    let block_count = applied_block_count(&mut cx);
    if block_count != 1 {
        let debug: Vec<String> = cx.update_editor(|editor, _, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            let snapshot = buffer.read(cx).snapshot();
            snapshot
                .syntax_layers()
                .flat_map(|layer| {
                    let mut nodes = Vec::new();
                    let mut stack = vec![layer.node()];
                    while let Some(node) = stack.pop() {
                        nodes.push(format!("{} {:?}", node.kind(), node.byte_range()));
                        for index in (0..node.child_count() as u32).rev() {
                            if let Some(child) = node.child(index) {
                                stack.push(child);
                            }
                        }
                    }
                    nodes
                })
                .collect()
        });
        panic!(
            "expected 1 linked-image block, got {block_count}; tree was:\n{}",
            debug.join("\n")
        );
    }
}


#[gpui::test]
async fn test_images_section_context(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {r#"
        ˇ## 9. Images

        Inline image:

        ![Placeholder image](https://placehold.co/300x150.png "A placeholder")

        Reference-style image:

        ![Placeholder ref][img]

        Image as a link:

        [![Clickable image](https://placehold.co/150x60.png)](https://example.com)

        Broken image (alt text should show): ![missing image](nonexistent.png)

        [img]: https://placehold.co/200x100.png
    "#});
    cx.executor().run_until_parked();
    let (blocks, rows): (usize, Vec<(u32, u32)>) = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let addon = editor.addon::<LivePreviewAddon>().unwrap();
        let rows = addon
            .applied_blocks
            .iter()
            .map(|block| {
                (
                    block.range.start.to_point(&snapshot).row,
                    block.range.end.to_point(&snapshot).row,
                )
            })
            .collect();
        (addon.applied_blocks.len(), rows)
    });
    // Inline image, reference image, and linked image; the heading is
    // revealed because the cursor sits on it.
    assert_eq!(blocks, 3, "applied block rows: {rows:?}");
    assert!(rows.contains(&(12, 12)), "linked image row missing: {rows:?}");
}

#[gpui::test]
async fn test_concealments_invisible_to_fold_machinery(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇplain line\nsome **bold** text\n");
    cx.executor().run_until_parked();
    assert!(cx.display_text().contains("some bold text"));

    // "Unfold All" must not reveal concealments: they are not folds.
    cx.update_editor(|editor, window, cx| {
        editor.unfold_all(&Default::default(), window, cx);
    });
    cx.executor().run_until_parked();
    assert!(
        cx.display_text().contains("some bold text"),
        "unfold-all revealed concealments: {}",
        cx.display_text()
    );

    // Fold queries (what the gutter and fold persistence read) see nothing.
    let fold_count = cx.update_editor(|editor, window, cx| {
        let _ = &window;
        let snapshot = editor.snapshot(window, cx);
        let len = snapshot.buffer_snapshot().len();
        snapshot.folds_in_range(MultiBufferOffset(0)..len).count()
    });
    assert_eq!(fold_count, 0);

    // No row reads as folded (the gutter's chevron predicate), across
    // concealed inline markers, bullets, headings, and rules.
    cx.set_state(indoc::indoc! {"
        ˇplain line

        # Heading

        ---

        - bullet with **bold** text
        - [ ] a task

        [a link](https://example.com)
    "});
    cx.executor().run_until_parked();
    // The gutter's chevron predicate: fold-map folds only.
    let folded_rows: Vec<u32> = cx.update_editor(|editor, window, cx| {
        let _ = &window;
        let snapshot = editor.snapshot(window, cx);
        let max_row = snapshot.buffer_snapshot().max_point().row;
        (0..=max_row)
            .filter(|row| {
                snapshot
                    .fold_snapshot()
                    .is_line_folded(multi_buffer::MultiBufferRow(*row))
            })
            .collect()
    });
    assert_eq!(folded_rows, Vec::<u32>::new());
}

#[test]
fn test_image_source_resolution_decodes_percent_encoding() {
    let dir = std::env::temp_dir().join("mdlp-resolver-test");
    std::fs::create_dir_all(&dir).unwrap();
    // macOS screenshot names mix ASCII spaces (percent-encoded in links)
    // with a raw narrow no-break space before "AM".
    let file_name = "Screenshot 2026-08-09 at 11.38.37\u{202f}AM.png";
    std::fs::write(dir.join(file_name), b"png").unwrap();

    let destination = "Screenshot%202026-08-09%20at%2011.38.37\u{202f}AM.png";
    assert!(
        resolve_image_source(destination, Some(&dir)).is_some(),
        "percent-encoded path failed to resolve"
    );
    assert!(resolve_image_source("missing%20file.png", Some(&dir)).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
async fn test_image_size_syntax(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        ![sized|640](a.png)

        ![sized dims|320x200](b.png)

        ![unsized](c.png)
    "});
    cx.executor().run_until_parked();
    let widths: Vec<Option<f32>> = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                BlockRenderKind::Image { display_width, .. } => Some(display_width),
                _ => None,
            })
            .collect()
    });
    assert_eq!(widths, vec![Some(640.), Some(320.), None]);
}

#[test]
fn test_with_image_width_rewrites_alt() {
    assert_eq!(
        with_image_width("![alt](a.png)", 500).as_deref(),
        Some("![alt|500](a.png)")
    );
    assert_eq!(
        with_image_width("![alt|300](a.png)", 500).as_deref(),
        Some("![alt|500](a.png)")
    );
    assert_eq!(
        with_image_width("![alt|320x200](a.png)", 500).as_deref(),
        Some("![alt|500](a.png)")
    );
    assert_eq!(
        with_image_width("![](a.png)", 500).as_deref(),
        Some("![|500](a.png)")
    );
    assert_eq!(
        with_image_width("![ref style|300][img]", 500).as_deref(),
        Some("![ref style|500][img]")
    );
    // A pipe that is not a size suffix stays intact.
    assert_eq!(
        with_image_width("![a|b](a.png)", 500).as_deref(),
        Some("![a|b|500](a.png)")
    );
}

#[gpui::test]
async fn test_wikilinks_conceal(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        see [[CLAUDE]] and [[notes/plan|the plan]] here
        embed stays raw: ![[image.png]]
        code stays raw: `[[not a link]]`
    "});
    cx.executor().run_until_parked();
    let display = cx.display_text();
    assert!(display.contains("see CLAUDE and the plan here"), "{display}");
    assert!(display.contains("embed stays raw: ![[image.png]]"), "{display}");
    assert!(display.contains("code stays raw: [[not a link]]"), "{display}");
}
