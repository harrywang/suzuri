use super::*;
use editor::test::editor_test_context::EditorTestContext;
use gpui::{Modifiers, TestAppContext};
use language::{Language, LanguageConfig};
use settings::SettingsStore;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Mutex},
};

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

    // Cursor inside the frontmatter does NOT reveal the raw YAML: the
    // Properties card is edited through its widget (like tables and images),
    // so the cursor landing at the top of a freshly opened file keeps the
    // card rendered.
    cx.set_state(indoc::indoc! {"
        ---
        title: Some Noteˇ
        parent: lectures
        ---

        body text
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // Marking the block explicitly revealed (what the card's `</>` button
    // does) removes the widget while the cursor stays inside.
    cx.update_editor(|editor, _, cx| {
        let range = extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Frontmatter => Some(block.range.clone()),
                _ => None,
            })
            .expect("frontmatter");
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.source_revealed = Some(range);
        }
        apply_decorations(editor, cx);
    });
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 0);
}

#[test]
fn test_parse_frontmatter_properties() {
    let source = indoc::indoc! {r#"
        ---
        title: "Some Note"
        date: 2026-05-31T00:00:00.000Z
        rating: 4.5
        published: false
        empty:
        tags:
          - alpha
          - "beta"
        aliases: [one, two]
        ---"#};

    let properties = parse_frontmatter_properties(source);
    let by_key: Vec<(&str, &FrontmatterValue)> = properties
        .iter()
        .map(|property| (property.key.as_str(), &property.value))
        .collect();
    assert_eq!(properties.len(), 7);

    let scalar = |value: &FrontmatterValue| match value {
        FrontmatterValue::Scalar(text) => text.clone(),
        FrontmatterValue::List(_) => panic!("expected scalar"),
    };
    let list = |value: &FrontmatterValue| match value {
        FrontmatterValue::List(items) => items.clone(),
        FrontmatterValue::Scalar(_) => panic!("expected list"),
    };

    assert_eq!(by_key[0].0, "title");
    assert_eq!(scalar(by_key[0].1), "Some Note");
    assert_eq!(scalar(by_key[1].1), "2026-05-31T00:00:00.000Z");
    assert_eq!(scalar(by_key[2].1), "4.5");
    assert_eq!(scalar(by_key[3].1), "false");
    assert_eq!(scalar(by_key[4].1), "");
    assert_eq!(list(by_key[5].1), vec!["alpha", "beta"]);
    assert_eq!(list(by_key[6].1), vec!["one", "two"]);

    // The value span starts right after the separator and covers the raw
    // (quoted) text, so an in-place rewrite replaces exactly the value.
    let title = &properties[0];
    assert_eq!(&source[title.value_span.clone()], " \"Some Note\"");
}

#[test]
fn test_parse_frontmatter_properties_toml() {
    let source = indoc::indoc! {r#"
        +++
        title = "TOML Note"
        draft = true
        +++"#};

    let properties = parse_frontmatter_properties(source);
    assert_eq!(properties.len(), 2);
    assert_eq!(properties[0].key, "title");
    assert!(matches!(&properties[0].value, FrontmatterValue::Scalar(text) if text == "TOML Note"));
    assert!(matches!(&properties[1].value, FrontmatterValue::Scalar(text) if text == "true"));
}

#[gpui::test]
async fn test_frontmatter_property_edit_round_trip(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ---
        title: Old Title
        ---

        body ˇtext
    "});
    cx.executor().run_until_parked();

    let frontmatter_range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Frontmatter => Some(block.range.clone()),
                _ => None,
            })
            .expect("frontmatter")
    });
    let editor = cx.editor.clone();

    // Clicking a value mounts a single-line editor seeded with the raw value.
    let weak = editor.downgrade();
    let edit_range = frontmatter_range.clone();
    cx.update(|window, cx| start_property_edit(weak, edit_range, "title".into(), window, cx));
    let property_editor = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.active_property.as_ref())
            .map(|active| active.editor.clone())
            .expect("active property editor")
    });
    cx.update(|window, cx| {
        property_editor.update(cx, |editor, cx| {
            assert_eq!(editor.text(cx), "Old Title");
            editor.set_text("New Title", window, cx);
        });
    });

    // Committing writes the new value back into the frontmatter line.
    cx.update_editor(|editor, _, cx| {
        assert_eq!(commit_active_property(editor, cx), None);
    });
    cx.executor().run_until_parked();
    let text = cx.update_editor(|editor, _, cx| editor.text(cx));
    assert!(text.contains("title: New Title"), "{text:?}");

    // "Add property" commits a `<key>: ` line before the closing delimiter
    // and hands back the key so Enter can chain into editing its value.
    let weak = editor.downgrade();
    let add_range = frontmatter_range.clone();
    cx.update(|window, cx| start_add_property(weak, add_range, window, cx));
    let key_editor = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.active_property.as_ref())
            .map(|active| active.editor.clone())
            .expect("active key editor")
    });
    cx.update(|window, cx| {
        key_editor.update(cx, |editor, cx| editor.set_text("tags", window, cx));
    });
    let created = cx.update_editor(|editor, _, cx| commit_active_property(editor, cx));
    assert_eq!(created.as_deref(), Some("tags"));
    cx.executor().run_until_parked();
    let text = cx.update_editor(|editor, _, cx| editor.text(cx));
    assert!(text.contains("title: New Title\ntags: \n---"), "{text:?}");
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
    assert!(
        rows.contains(&(12, 12)),
        "linked image row missing: {rows:?}"
    );
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

#[gpui::test]
fn test_image_source_resolution_decodes_percent_encoding(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join("mdlp-resolver-test");
    std::fs::create_dir_all(&dir).unwrap();
    // macOS screenshot names mix ASCII spaces (percent-encoded in links)
    // with a raw narrow no-break space before "AM".
    let file_name = "Screenshot 2026-08-09 at 11.38.37\u{202f}AM.png";
    std::fs::write(dir.join(file_name), b"png").unwrap();

    let image_cache = cx.update(RetainAllImageCache::new);
    let destination = "Screenshot%202026-08-09%20at%2011.38.37\u{202f}AM.png";
    assert!(
        resolve_image_source(destination, Some(&dir), &image_cache).is_some(),
        "percent-encoded path failed to resolve"
    );
    assert!(resolve_image_source("missing%20file.png", Some(&dir), &image_cache).is_none());
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
    assert!(
        display.contains("see CLAUDE and the plan here"),
        "{display}"
    );
    assert!(
        display.contains("embed stays raw: ![[image.png]]"),
        "{display}"
    );
    assert!(
        display.contains("code stays raw: [[not a link]]"),
        "{display}"
    );
}

#[gpui::test]
async fn test_obsidian_image_embed_renders_as_block(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        ![[photo.png]]
        ![[photo.png|640]]
        inline stays raw: ![[photo.png]]
        ![[Some Note]]
    "});
    cx.executor().run_until_parked();

    // The two standalone image embeds render as image blocks; the inline
    // embed and the non-image note embed stay raw.
    assert_eq!(applied_block_count(&mut cx), 2);
    let display = cx.display_text();
    assert!(
        display.contains("inline stays raw: ![[photo.png]]"),
        "{display}"
    );
    assert!(display.contains("![[Some Note]]"), "{display}");
}

#[gpui::test]
async fn test_citations_conceal(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        a claim [@doe2020] here
        grouped [see @smith2019, p. 33; also @lee2021] here
        a [label](https://example.com) link stays a link
        code stays raw: `[@nope]`
        not citations: [plain] and [me@example.com]
    "});
    cx.executor().run_until_parked();
    let display = cx.display_text();
    assert!(display.contains("a claim @doe2020 here"), "{display}");
    assert!(
        display.contains("grouped see @smith2019, p. 33; also @lee2021 here"),
        "{display}"
    );
    assert!(display.contains("a label link stays a link"), "{display}");
    assert!(display.contains("code stays raw: [@nope]"), "{display}");
    assert!(
        display.contains("not citations: [plain] and [me@example.com]"),
        "{display}"
    );

    // The three keys across the two citation groups carry the citation
    // highlight; the email and the link do not.
    let citation_ranges = cx.update_editor(|editor, _, cx| {
        editor
            .text_highlights(HighlightKey::MarkdownLivePreview(CITATION), cx)
            .map_or(0, |(_, ranges)| ranges.len())
    });
    assert_eq!(citation_ranges, 3);
}

#[gpui::test]
async fn test_citation_reveals_on_touch(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        plain line
        a claim [@doe2020] with **bold** afterˇ
    "});
    cx.executor().run_until_parked();
    assert!(
        cx.display_text()
            .contains("a claim @doe2020 with bold after"),
        "{}",
        cx.display_text()
    );

    // Cursor inside the citation reveals its brackets, per-token: the bold
    // further along the line stays rendered.
    cx.set_state(indoc::indoc! {"
        plain line
        a claim [@doeˇ2020] with **bold** after
    "});
    cx.executor().run_until_parked();
    assert!(
        cx.display_text()
            .contains("a claim [@doe2020] with bold after"),
        "{}",
        cx.display_text()
    );
}

#[gpui::test]
async fn test_table_structure_extraction(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | Left | Center | Right |
        |:-----|:------:|------:|
        | a    |   b    |     c |
        | d    |   e    |     f |
    "});
    cx.executor().run_until_parked();
    let (structure, cell_texts) = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let markers = extract_markers(editor, cx).unwrap();
        let structure = markers
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(structure) => Some(structure.clone()),
                _ => None,
            })
            .expect("table structure");
        let texts: Vec<String> = structure
            .cells_in_order()
            .iter()
            .map(|range| {
                let start = range.start.to_offset(&snapshot);
                let end = range.end.to_offset(&snapshot);
                snapshot
                    .text_for_range(start..end)
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .collect();
        (structure, texts)
    });
    assert_eq!(structure.header.len(), 3);
    assert_eq!(structure.rows.len(), 2);
    assert_eq!(
        structure.alignments,
        vec![
            CellAlignment::Left,
            CellAlignment::Center,
            CellAlignment::Right
        ]
    );
    assert_eq!(
        cell_texts,
        vec!["Left", "Center", "Right", "a", "b", "c", "d", "e", "f"]
    );

    // Empty cells omitted from the syntax tree are padded to a rectangle.
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B | C |
        |---|---|---|
        | 1 |   | 3 |
        |   | 2 |   |
    "});
    cx.executor().run_until_parked();
    let structure = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(structure) => Some(structure.clone()),
                _ => None,
            })
            .expect("table structure")
    });
    assert_eq!(structure.header.len(), 3);
    assert!(structure.rows.iter().all(|row| row.len() == 3));
    // Empty cells sit between pipes, so they are real editable ranges with
    // correct column identity — never sentinels shifted to the row's end.
    let row_texts: Vec<Vec<String>> = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        structure
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| {
                        assert!(cell.start != Anchor::Min, "unexpected sentinel cell");
                        let start = cell.start.to_offset(&snapshot);
                        let end = cell.end.to_offset(&snapshot);
                        snapshot
                            .text_for_range(start..end)
                            .collect::<String>()
                            .trim()
                            .to_string()
                    })
                    .collect()
            })
            .collect()
    });
    assert_eq!(row_texts, vec![vec!["1", "", "3"], vec!["", "2", ""]]);
}

#[gpui::test]
async fn test_table_structural_changes(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | 1 | 2 |
    "});
    cx.executor().run_until_parked();
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table")
    });
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::AddColumn, cx);
    });
    cx.executor().run_until_parked();
    assert!(
        cx.buffer_text().contains("| A | B |   |"),
        "{}",
        cx.buffer_text()
    );

    // Reuse the now-stale range on purpose: structural ops must re-resolve the
    // table from the live tree, since widget closures outlive edits.
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::AddRow, cx);
    });
    cx.executor().run_until_parked();
    let text = cx.buffer_text();
    let table_lines: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(table_lines.len(), 4, "{text}");
    assert!(
        text.lines()
            .filter(|line| line.starts_with('|'))
            .all(|line| line.matches('|').count() == 4),
        "every table line should have 3 columns after the changes: {text}"
    );
    assert_eq!(table_lines.len(), 4, "{text}");
}

#[gpui::test]
async fn test_add_row_no_outer_pipes(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        Name | Value
        --- | ---
        alpha | 1
        beta | 2

        after
    "});
    cx.executor().run_until_parked();
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table")
    });
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::AddRow, cx);
    });
    cx.executor().run_until_parked();
    let text = cx.buffer_text();
    let table_lines: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(
        table_lines,
        vec![
            "| Name | Value |",
            "| --- | --- |",
            "| alpha | 1 |",
            "| beta | 2 |",
            "|   |   |",
        ],
        "full text: {text:?}"
    );
    assert!(
        text.contains("\n\nafter"),
        "must not eat the blank line: {text:?}"
    );
}

#[gpui::test]
async fn test_rapid_structural_changes_between_reparses(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | 1 | 2 |
    "});
    cx.executor().run_until_parked();
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table")
    });
    // Two clicks in quick succession: no run_until_parked between them, so
    // the second acts before any reparse — it must still resolve the table
    // from live text instead of the stale tree.
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::AddRow, cx);
        apply_table_structural_change(editor, &range, TableStructuralChange::AddRow, cx);
        apply_table_structural_change(editor, &range, TableStructuralChange::AddColumn, cx);
    });
    cx.executor().run_until_parked();
    let text = cx.buffer_text();
    let table_lines: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(
        table_lines,
        vec![
            "| A | B |   |",
            "| --- | --- | --- |",
            "| 1 | 2 |   |",
            "|   |   |   |",
            "|   |   |   |",
        ],
        "full text: {text:?}"
    );
}

#[gpui::test]
async fn test_add_row_extends_widget_over_empty_row(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇMinimal table without outer pipes:

        Name | Value
        --- | ---
        alpha | 1
        beta | 2

        Table with empty cells:
    "});
    cx.executor().run_until_parked();
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table")
    });
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::AddRow, cx);
    });
    cx.executor().run_until_parked();
    // tree-sitter-md errors on the all-empty row and ends its table node
    // early; the widget must still span the full textual table.
    let tables: Vec<(u32, u32, usize)> = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockRenderKind::Table(structure) => Some((
                    block.range.start.to_point(&snapshot).row,
                    block.range.end.to_point(&snapshot).row,
                    structure.rows.len(),
                )),
                _ => None,
            })
            .collect()
    });
    assert_eq!(tables, vec![(2, 6, 3)], "text: {:?}", cx.buffer_text());
}

#[gpui::test]
async fn test_table_reveals_only_via_button(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | 1 | 2 |
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // Cursor inside the table does NOT reveal its source.
    cx.set_state(indoc::indoc! {"
        plain line

        | A | B |
        | --- | --- |
        | 1 |ˇ 2 |
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // Marking the block explicitly revealed (what the `</>` button does)
    // removes the widget while the cursor stays inside.
    cx.update_editor(|editor, _, cx| {
        let range = extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table");
        if let Some(addon) = editor.addon_mut::<LivePreviewAddon>() {
            addon.source_revealed = Some(range);
        }
        apply_decorations(editor, cx);
    });
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 0);

    // Moving the cursor out re-renders the widget and clears the reveal.
    cx.set_state(indoc::indoc! {"
        plain lineˇ

        | A | B |
        | --- | --- |
        | 1 | 2 |
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
    let cleared = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.source_revealed.clone())
            .is_none()
    });
    assert!(
        cleared,
        "reveal must clear when the selection leaves the block"
    );
}

#[gpui::test]
async fn test_move_and_delete_rows_columns(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B | C |
        | --- | --- | --- |
        | 1 | 2 | 3 |
        | x | y | z |
    "});
    cx.executor().run_until_parked();
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Table(_) => Some(block.range.clone()),
                _ => None,
            })
            .expect("table")
    });
    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(
            editor,
            &range,
            TableStructuralChange::MoveRow { from: 0, to: 1 },
            cx,
        );
    });
    cx.executor().run_until_parked();
    assert!(
        cx.buffer_text().contains("| x | y | z |\n| 1 | 2 | 3 |"),
        "{}",
        cx.buffer_text()
    );

    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(
            editor,
            &range,
            TableStructuralChange::MoveColumn { from: 0, to: 2 },
            cx,
        );
    });
    cx.executor().run_until_parked();
    // Move semantics: remove+insert (drag across positions), not swap.
    assert!(
        cx.buffer_text().contains("| B | C | A |"),
        "{}",
        cx.buffer_text()
    );

    cx.update_editor(|editor, _, cx| {
        apply_table_structural_change(editor, &range, TableStructuralChange::DeleteRow(1), cx);
        apply_table_structural_change(editor, &range, TableStructuralChange::DeleteColumn(1), cx);
    });
    cx.executor().run_until_parked();
    let text = cx.buffer_text();
    assert!(text.contains("| B | A |"), "{text}");
    assert!(text.contains("| y | x |"), "{text}");
    assert!(
        !text.contains("| 1 |"),
        "deleted row should be gone: {text}"
    );
}

#[gpui::test]
async fn test_drag_row_reorders_via_mouse(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | one | 1 |
        | two | 2 |
    "});
    cx.executor().run_until_parked();

    let handle = cx
        .cx
        .debug_bounds("mdlp-row-handle-0")
        .expect("row handle rendered");
    let target = cx
        .cx
        .debug_bounds("mdlp-cell-1-0")
        .expect("target cell rendered");

    let start = handle.center();
    cx.cx
        .simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
    // Cross the drag threshold, then hover the target row.
    cx.cx.simulate_mouse_move(
        start + gpui::point(gpui::px(0.), gpui::px(6.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let below_target = target.center() + gpui::point(gpui::px(0.), target.size.height * 0.3);
    cx.cx.simulate_mouse_move(
        below_target,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();

    let (drag_active, source_set, boundary_set) = cx.update_editor(|editor, _, cx| {
        let addon = editor.addon::<LivePreviewAddon>().expect("addon");
        (
            cx.has_active_drag(),
            addon.drag_source.as_ref().map(|s| s.unit),
            addon.drop_boundary.as_ref().map(|(_, boundary)| *boundary),
        )
    });
    assert!(
        drag_active,
        "drag should be active after moving past threshold"
    );
    assert_eq!(
        source_set,
        Some(TableUnit::Row(0)),
        "source should be recorded"
    );
    assert_eq!(
        boundary_set,
        Some(TableBoundary::Row(2)),
        "lower half of row 1 should target the boundary below it"
    );

    cx.cx.simulate_mouse_up(
        below_target,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();

    let text = cx.buffer_text();
    let rows: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(
        rows,
        vec!["| A | B |", "| --- | --- |", "| two | 2 |", "| one | 1 |"],
        "rows should be reordered by the drop: {text:?}"
    );
}

#[gpui::test]
async fn test_drag_row_drop_anywhere_on_table(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | one | 1 |
        | two | 2 |
    "});
    cx.executor().run_until_parked();

    let handle = cx
        .cx
        .debug_bounds("mdlp-row-handle-0")
        .expect("row handle rendered");
    let target_cell = cx
        .cx
        .debug_bounds("mdlp-cell-1-0")
        .expect("target cell rendered");
    let target_handle = cx
        .cx
        .debug_bounds("mdlp-row-handle-1")
        .expect("target row handle rendered");

    let start = handle.center();
    cx.cx
        .simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.cx.simulate_mouse_move(
        start + gpui::point(gpui::px(0.), gpui::px(6.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    // Hover the target row's lower half (tracks the below-boundary), then
    // drift onto the row HANDLE and release there — like a user following
    // the pill column.
    cx.cx.simulate_mouse_move(
        target_cell.center() + gpui::point(gpui::px(0.), target_cell.size.height * 0.3),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let handle_lower =
        target_handle.center() + gpui::point(gpui::px(0.), target_handle.size.height * 0.3);
    cx.cx.simulate_mouse_move(
        handle_lower,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.cx.simulate_mouse_up(
        handle_lower,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();

    let text = cx.buffer_text();
    let rows: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(
        rows,
        vec!["| A | B |", "| --- | --- |", "| two | 2 |", "| one | 1 |"],
        "release anywhere over the table should still apply the tracked drop: {text:?}"
    );
}

#[gpui::test]
async fn test_drag_column_release_on_handle_strip(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | one | 1 |
    "});
    cx.executor().run_until_parked();

    let source = cx
        .cx
        .debug_bounds("mdlp-column-handle-0")
        .expect("column handle rendered");
    let target = cx
        .cx
        .debug_bounds("mdlp-column-handle-1")
        .expect("second column handle rendered");

    let start = source.center();
    cx.cx
        .simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.cx.simulate_mouse_move(
        start + gpui::point(gpui::px(6.), gpui::px(0.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let right_half = target.center() + gpui::point(target.size.width * 0.3, gpui::px(0.));
    cx.cx
        .simulate_mouse_move(right_half, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.cx
        .simulate_mouse_up(right_half, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.executor().run_until_parked();

    let text = cx.buffer_text();
    assert!(
        text.contains("| B | A |"),
        "dragging a column along the handle strip should reorder: {text:?}"
    );
}

#[gpui::test]
async fn test_drag_survives_mid_gesture_repaint(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B |
        | --- | --- |
        | one | 1 |
        | two | 2 |
    "});
    cx.executor().run_until_parked();

    let handle = cx
        .cx
        .debug_bounds("mdlp-row-handle-0")
        .expect("row handle rendered");
    let target = cx
        .cx
        .debug_bounds("mdlp-cell-1-0")
        .expect("target cell rendered");

    let start = handle.center();
    cx.cx
        .simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
    // A repaint lands between press and first movement (cursor blink, agent
    // panel updates, etc.) — the armed gesture must survive it.
    cx.update_editor(|_, _, cx| cx.notify());
    cx.executor().run_until_parked();
    cx.cx.simulate_mouse_move(
        start + gpui::point(gpui::px(0.), gpui::px(6.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update_editor(|_, _, cx| cx.notify());
    cx.executor().run_until_parked();
    let below_target = target.center() + gpui::point(gpui::px(0.), target.size.height * 0.3);
    cx.cx.simulate_mouse_move(
        below_target,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.cx.simulate_mouse_up(
        below_target,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();

    let text = cx.buffer_text();
    let rows: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
    assert_eq!(
        rows,
        vec!["| A | B |", "| --- | --- |", "| two | 2 |", "| one | 1 |"],
        "drag should survive repaints mid-gesture: {text:?}"
    );
}

#[gpui::test]
async fn test_drag_column_between_others(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | A | B | C |
        | --- | --- | --- |
        | 1 |   | 3 |
    "});
    cx.executor().run_until_parked();

    let source = cx
        .cx
        .debug_bounds("mdlp-column-handle-0")
        .expect("column handle rendered");
    let target = cx
        .cx
        .debug_bounds("mdlp-cell-h-2")
        .expect("header C rendered");

    let start = source.center();
    cx.cx
        .simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.cx.simulate_mouse_move(
        start + gpui::point(gpui::px(6.), gpui::px(0.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    // Left half of column C targets the boundary between B and C.
    let left_half = target.center() - gpui::point(target.size.width * 0.3, gpui::px(0.));
    cx.cx
        .simulate_mouse_move(left_half, gpui::MouseButton::Left, gpui::Modifiers::none());
    let boundary = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.drop_boundary.as_ref().map(|(_, boundary)| *boundary))
    });
    assert_eq!(
        boundary,
        Some(TableBoundary::Column(2)),
        "left half of C should target the B|C boundary"
    );
    cx.cx
        .simulate_mouse_up(left_half, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.executor().run_until_parked();

    let text = cx.buffer_text();
    assert!(
        text.contains("| B | A | C |"),
        "A dropped between B and C: {text:?}"
    );
}

#[gpui::test]
async fn test_table_cell_wraps_long_content(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | Field | Path |
        | --- | --- |
        | Curriculum vitae | `~/Documents/CV/HarryWang-CV-Oct-2025.pdf` or enter `https://harrywang.me` in the website field, then attach the same file again so the archive copy stays in sync |
        | Report | x |
    "});
    cx.executor().run_until_parked();

    let long = cx
        .cx
        .debug_bounds("mdlp-cell-0-1")
        .expect("long cell rendered");
    let short = cx
        .cx
        .debug_bounds("mdlp-cell-1-1")
        .expect("short cell rendered");

    assert!(
        long.size.height > short.size.height,
        "long cell content must wrap inside its column and grow the row, got long={:?} short={:?}",
        long.size,
        short.size
    );
}

#[gpui::test]
async fn test_inline_math_conceals_and_reveals(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    // Off-cursor, the whole `$...$` construct is concealed behind one widget.
    cx.set_state(indoc::indoc! {"
        ˇplain line
        energy is $E = mc^2$ here
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            energy is ⋯ here
        "}
    );

    // Cursor inside the formula reveals the LaTeX for editing.
    cx.set_state(indoc::indoc! {"
        plain line
        energy is $E = mˇc^2$ here
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            energy is $E = mc^2$ here
        "}
    );

    // Cursor elsewhere on the same line does not reveal it.
    cx.set_state(indoc::indoc! {"
        plain line
        energy iˇs $E = mc^2$ here
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            energy is ⋯ here
        "}
    );
}

#[gpui::test]
async fn test_inline_math_requires_tight_delimiters(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    // Following Obsidian, `$` with whitespace immediately inside is not math,
    // so dollar amounts in prose stay prose.
    cx.set_state(indoc::indoc! {"
        ˇplain line
        $ a = b $
        it costs $5 and $10 today
    "});
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            $ a = b $
            it costs $5 and $10 today
        "}
    );
}

#[gpui::test]
async fn test_display_math_renders_as_block(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        ˇplain line

        $$ a = b $$
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // The delimiters on their own lines — the common research-note format.
    cx.set_state(indoc::indoc! {"
        ˇplain line

        $$
        a = b
        $$
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
    assert!(
        !cx.display_text().contains("a = b"),
        "multi-line display math should be replaced by its widget: {}",
        cx.display_text()
    );

    // Mid-line display math cannot replace its lines without swallowing the
    // surrounding text, so it renders inline instead.
    cx.set_state(indoc::indoc! {"
        ˇplain line
        before $$ a = b $$ after
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 0);
    pretty_assertions::assert_eq!(
        cx.display_text(),
        indoc::indoc! {"
            plain line
            before ⋯ after
        "}
    );
}

#[gpui::test]
async fn test_display_math_keeps_rendering_while_editing(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    // Cursor inside the formula: the source is revealed, but the rendered
    // widget stays as a block below it instead of disappearing, so the
    // typeset result remains visible while editing (Obsidian behavior).
    cx.set_state(indoc::indoc! {"
        plain line

        $$ a =ˇ b $$
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
    assert!(
        cx.display_text().contains("$$ a = b $$"),
        "revealed display math must show its source: {}",
        cx.display_text()
    );

    // Cursor back outside: the block replaces the source lines again.
    cx.set_state(indoc::indoc! {"
        plainˇ line

        $$ a = b $$
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
    assert!(
        !cx.display_text().contains("$$ a = b $$"),
        "unrevealed display math must not show its source: {}",
        cx.display_text()
    );
}

#[gpui::test]
async fn test_table_cell_br_breaks_the_line(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | Degree | Year |
        | --- | --- |
        | **Ph.D.**<br>University of Arizona | 2006 |
        | **B.S.** | 2001 |
    "});
    cx.executor().run_until_parked();

    let broken = cx
        .cx
        .debug_bounds("mdlp-cell-0-0")
        .expect("cell with <br> rendered");
    let single_line = cx
        .cx
        .debug_bounds("mdlp-cell-1-0")
        .expect("single-line cell rendered");

    assert!(
        broken.size.height > single_line.size.height,
        "<br> must render as a line break rather than literal text, got broken={:?} single={:?}",
        broken.size,
        single_line.size
    );
}

#[gpui::test]
async fn test_table_cells_fill_their_row(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | Stone | Provenance |
        | --- | --- |
        | Duan | Lower rock, purple-brown with banded eyes<br>Zhaoqing, Guangdong, quarried 1782 |
    "});
    cx.executor().run_until_parked();

    let short = cx
        .cx
        .debug_bounds("mdlp-cell-0-0")
        .expect("short cell rendered");
    let tall = cx
        .cx
        .debug_bounds("mdlp-cell-0-1")
        .expect("tall cell rendered");

    // A short cell must stretch to its row rather than sit centered at its own
    // content height, which would leave the grid lines ragged.
    assert_eq!(
        short.size.height, tall.size.height,
        "cells in one row must share a height, got short={:?} tall={:?}",
        short.size, tall.size
    );
}

#[test]
fn test_wikilink_display_text_in_cells() {
    // Matches what `scan_wikilinks` conceals to outside a table.
    assert_eq!(wikilink_display_text("[[reading-list]]"), "reading-list");
    assert_eq!(wikilink_display_text("[[note|alias]]"), "alias");
    assert_eq!(wikilink_display_text("[[Note#heading]]"), "Note#heading");
    // Inside a pipe table the alias separator must be escaped or it splits the
    // cell, so this is the form that actually reaches a cell renderer.
    assert_eq!(wikilink_display_text(r"[[note\|alias]]"), "alias");
    assert_eq!(
        wikilink_display_text("see [[duan-ratios]] and [[a|b]] here"),
        "see duan-ratios and b here"
    );

    // Left raw, same as the concealment path.
    assert_eq!(wikilink_display_text("![[embed.png]]"), "![[embed.png]]");
    assert_eq!(wikilink_display_text("`[[literal]]`"), "`[[literal]]`");
    assert_eq!(wikilink_display_text("``a `[[x]]` b``"), "``a `[[x]]` b``");
    assert_eq!(wikilink_display_text("[[]]"), "[[]]");
    assert_eq!(wikilink_display_text("[[unclosed"), "[[unclosed");

    // Untouched text is borrowed rather than rebuilt.
    assert!(matches!(
        wikilink_display_text("no links here"),
        std::borrow::Cow::Borrowed(_)
    ));

    // A code span must not shield a wikilink that sits outside it.
    assert_eq!(
        wikilink_display_text("`code` then [[note]]"),
        "`code` then note"
    );
}

#[gpui::test]
async fn test_highlight_marks_conceal(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        a ==marked phrase== here
        code stays raw: `==nope==`
        loose delimiters: a == b == c
        empty stays raw: ====
    "});
    cx.executor().run_until_parked();
    let display = cx.display_text();
    assert!(display.contains("a marked phrase here"), "{display}");
    assert!(display.contains("code stays raw: ==nope=="), "{display}");
    assert!(display.contains("loose delimiters: a == b == c"), "{display}");
    assert!(display.contains("empty stays raw: ===="), "{display}");

    // Only the one real mark carries the highlight.
    let highlighted = cx.update_editor(|editor, _, cx| {
        editor
            .text_highlights(HighlightKey::MarkdownLivePreview(HIGHLIGHT), cx)
            .map_or(0, |(_, ranges)| ranges.len())
    });
    assert_eq!(highlighted, 1);

    // Touching the mark hands back its source, like every inline construct.
    cx.set_state(indoc::indoc! {"
        plain line
        a ==markeˇd phrase== here
    "});
    cx.executor().run_until_parked();
    assert!(
        cx.display_text().contains("a ==marked phrase== here"),
        "{}",
        cx.display_text()
    );
}

#[gpui::test]
async fn test_tags_are_styled_without_concealment(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        filed under #research and #area/methods today
        not a tag: example.com/#top or [[Note#section]] or #1
        code stays raw: `#nope`
    "});
    cx.executor().run_until_parked();

    // A tag keeps its `#`: it is styled in place, never concealed.
    let display = cx.display_text();
    assert!(
        display.contains("filed under #research and #area/methods today"),
        "{display}"
    );

    let tags = cx.update_editor(|editor, _, cx| {
        editor
            .text_highlights(HighlightKey::MarkdownLivePreview(TAG), cx)
            .map_or(0, |(_, ranges)| ranges.len())
    });
    assert_eq!(tags, 2, "only the two real tags should be styled");
}

#[gpui::test]
async fn test_headings_are_never_read_as_tags(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        # Heading One

        ### Heading Three
    "});
    cx.executor().run_until_parked();

    let tags = cx.update_editor(|editor, _, cx| {
        editor
            .text_highlights(HighlightKey::MarkdownLivePreview(TAG), cx)
            .map_or(0, |(_, ranges)| ranges.len())
    });
    assert_eq!(tags, 0);
}

#[gpui::test]
async fn test_footnotes_conceal_references_and_mute_definitions(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line
        a claim[^1] and another[^note] here
        code stays raw: `[^2]`
        not a footnote: [^ spaced]

        [^1]: The first note.
        [^note]: The second note.
    "});
    cx.executor().run_until_parked();

    let display = cx.display_text();
    assert!(display.contains("a claim⋯ and another⋯ here"), "{display}");
    assert!(display.contains("code stays raw: [^2]"), "{display}");
    assert!(display.contains("not a footnote: [^ spaced]"), "{display}");

    // Definitions keep their marker on screen, muted rather than concealed:
    // a bare paragraph would not say which footnote it defines.
    assert!(display.contains("[^1]: The first note."), "{display}");

    let markers = cx.update_editor(|editor, _, cx| {
        let markers = extract_markers(editor, cx).expect("markdown buffer should produce markers");
        let references = markers
            .inline
            .iter()
            .filter(|marker| matches!(marker.kind, InlineKind::Footnote { .. }))
            .count();
        (references, markers.definition_ranges.len())
    });
    assert_eq!(markers.0, 2, "two references");
    assert_eq!(markers.1, 2, "two definition markers muted");

    // Touching a reference reveals its source.
    cx.set_state(indoc::indoc! {"
        plain line
        a claim[^ˇ1] and another[^note] here
    "});
    cx.executor().run_until_parked();
    assert!(
        cx.display_text().contains("a claim[^1] and another⋯ here"),
        "{}",
        cx.display_text()
    );
}

// --- Contract tests ---
//
// These pin behavior this crate relies on from `editor` and `gpui` rather than
// behavior of its own. They exist so an upstream merge that changes those
// semantics fails here, loudly, instead of silently degrading live preview.

/// Live preview's text decorations all hang off `HighlightKey::MarkdownLivePreview`,
/// keyed per decoration kind. Highlights written under one key must not be
/// disturbed when another key is cleared, or disabling one decoration would
/// wipe the others.
#[gpui::test]
async fn test_highlight_key_namespaces_are_independent(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇone **bold** and *italic* and ~~struck~~ here\n");
    cx.executor().run_until_parked();

    let highlighted = |cx: &mut EditorTestContext, key: usize| {
        cx.update_editor(|editor, _, cx| {
            editor
                .text_highlights(HighlightKey::MarkdownLivePreview(key), cx)
                .map_or(0, |(_, ranges)| ranges.len())
        })
    };

    assert!(highlighted(&mut cx, BOLD) > 0);
    assert!(highlighted(&mut cx, ITALIC) > 0);

    cx.update_editor(|editor, _, cx| {
        editor.clear_highlights(HighlightKey::MarkdownLivePreview(BOLD), cx);
    });

    assert_eq!(highlighted(&mut cx, BOLD), 0);
    assert!(
        highlighted(&mut cx, ITALIC) > 0,
        "clearing one highlight key dropped another key's ranges"
    );
}

/// Math markers come from the `latex_block` node that tree-sitter-md's inline
/// grammar only emits when it was generated with its latex extension enabled.
/// An upstream bump to a build of the grammar without that extension would
/// silently turn all math back into plain text; this fails instead.
#[gpui::test]
async fn test_markdown_inline_grammar_emits_latex_block(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇsome $E = mc^2$ math\n");
    cx.executor().run_until_parked();
    pretty_assertions::assert_eq!(cx.display_text(), "some ⋯ math\n");
}

/// A block widget's click-to-reveal defers to buttons rendered inside it by
/// checking `window.default_prevented()`, which `ButtonLike` sets on left mouse
/// down. If upstream stopped setting it, pressing a rendered code block's copy
/// button would tear the widget down mid-click and reveal source instead of
/// copying.
#[gpui::test]
fn test_buttons_prevent_default_on_mouse_down(cx: &mut TestAppContext) {
    init_test(cx);

    struct ButtonInsideWidget(Rc<Cell<Option<bool>>>);

    impl Render for ButtonInsideWidget {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let prevented = self.0.clone();
            div()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, window, _| {
                    prevented.set(Some(window.default_prevented()));
                })
                .child(
                    div().debug_selector(|| "BUTTON".into()).child(
                        ui::Button::new("copy", "Copy")
                            .full_width()
                            .on_click(|_, _, _| {}),
                    ),
                )
        }
    }

    let prevented = Rc::new(Cell::new(None));
    let (_view, cx) = cx.add_window_view({
        let prevented = prevented.clone();
        move |_window, _cx| ButtonInsideWidget(prevented)
    });
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds("BUTTON")
        .expect("the button should have been laid out");
    cx.simulate_event(MouseDownEvent {
        position: bounds.center(),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });

    pretty_assertions::assert_eq!(
        prevented.get(),
        Some(true),
        "a button no longer prevents the default mouse-down action, so a block \
         widget's click-to-reveal will fire on button presses"
    );
}

/// The whole reason local images do not use `ImageSource::Resource`: gpui's
/// app-level asset cache is keyed on the path alone and has no eviction, so a
/// file rewritten in place — re-cropping a screenshot over its own name — keeps
/// serving the bitmap decoded on first render for the life of the process, and
/// reopening the document cannot help because that cache outlives the document.
///
/// This pins the whole chain: that the staleness is real, that live preview
/// therefore routes local files through a cache it owns, and that evicting the
/// rewritten path from it is what brings the new bitmap back.
// The probe below wraps the resolved `ImageSource`, and `ImageSource::Custom`
// holds an `Arc<dyn Fn>` with no `Send`/`Sync` bound, so any wrapper of one
// trips this lint by construction.
#[allow(clippy::arc_with_non_send_sync)]
async fn image_sizes_across_eviction(
    cx: &mut TestAppContext,
    destination: &str,
    base_directory: &std::path::Path,
    image_path: &std::path::Path,
    evict_path: &std::path::Path,
) -> [Option<(i32, i32)>; 3] {
    let write_image = |width: u32, height: u32| {
        image::save_buffer(
            image_path,
            &vec![0xff; (width * height * 4) as usize],
            width,
            height,
            image::ColorType::Rgba8,
        )
        .expect("failed to write the test image");
    };
    write_image(4, 2);

    let image_cache = cx.update(RetainAllImageCache::new);
    let source = resolve_image_source(destination, Some(base_directory), &image_cache)
        .expect("an existing image file should resolve");
    assert!(
        matches!(source, ImageSource::Custom(_)),
        "a local image resolved to an app-level asset source, which cannot be \
         evicted when the file changes on disk"
    );

    // Record what each draw actually decoded, by wrapping the resolved source.
    let drawn = Arc::new(Mutex::new(None));
    let probe = ImageSource::Custom(Arc::new({
        let drawn = drawn.clone();
        move |window, cx| {
            let ImageSource::Custom(load) = &source else {
                unreachable!("checked above");
            };
            let loaded = load(window, cx);
            if let Some(Ok(image)) = &loaded {
                let size = image.size(0);
                *drawn.lock().unwrap() = Some((size.width.0, size.height.0));
            }
            loaded
        }
    }));

    struct ImageProbe(ImageSource);
    impl Render for ImageProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            img(self.0.clone())
        }
    }

    let (view, mut cx) = cx.add_window_view(|_window, _cx| ImageProbe(probe));
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.run_until_parked();
        cx.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(200.), gpui::px(200.)),
            |_window, _cx| view.clone().into_any_element(),
        );
        cx.run_until_parked();
    };

    draw(&mut cx);
    draw(&mut cx);
    let initial = *drawn.lock().unwrap();

    write_image(8, 3);
    draw(&mut cx);
    let after_rewrite = *drawn.lock().unwrap();

    cx.update(|window, cx| {
        image_cache.update(cx, |image_cache, cx| {
            image_cache.remove(&Resource::Path(Arc::from(evict_path)), window, cx);
        });
    });
    draw(&mut cx);
    draw(&mut cx);
    let after_eviction = *drawn.lock().unwrap();

    [initial, after_rewrite, after_eviction]
}

#[gpui::test]
async fn test_rewriting_an_image_in_place_reloads_it(cx: &mut TestAppContext) {
    init_test(cx);

    let directory = tempfile::tempdir().expect("failed to create a temp dir");
    let path = directory.path().join("screenshot.png");
    // What the fs-change handler would evict: the file's real, canonical path.
    let evict_path = std::fs::canonicalize(directory.path())
        .expect("temp dir should canonicalize")
        .join("screenshot.png");

    let [initial, after_rewrite, after_eviction] =
        image_sizes_across_eviction(cx, "screenshot.png", directory.path(), &path, &evict_path)
            .await;

    pretty_assertions::assert_eq!(initial, Some((4, 2)), "the image never finished loading");
    pretty_assertions::assert_eq!(
        after_rewrite,
        Some((4, 2)),
        "an image cache that reloads on its own would make the eviction below \
         dead code"
    );
    pretty_assertions::assert_eq!(
        after_eviction,
        Some((8, 3)),
        "evicting the rewritten path did not bring back the new bitmap, so a \
         re-cropped screenshot still renders at its old size"
    );
}

/// A vault that keeps its images in a folder beside its notes spells every
/// reference `../images/x.png`. Joining that onto the note's directory keeps the
/// `..` literally, while the worktree reports the file under its collapsed path
/// — so unless both sides canonicalize, the two hash differently and eviction
/// silently matches nothing. That layout is ordinary, not exotic: this pins it.
#[gpui::test]
async fn test_image_reference_climbing_out_of_its_folder_reloads(cx: &mut TestAppContext) {
    init_test(cx);

    let vault = tempfile::tempdir().expect("failed to create a temp dir");
    let notes = vault.path().join("lectures");
    let images = vault.path().join("images");
    std::fs::create_dir_all(&notes).expect("failed to create the notes dir");
    std::fs::create_dir_all(&images).expect("failed to create the images dir");
    let path = images.join("screenshot.png");
    // The worktree reports a changed file by its collapsed path, never with `..`.
    let evict_path = std::fs::canonicalize(&images)
        .expect("images dir should canonicalize")
        .join("screenshot.png");

    let [initial, after_rewrite, after_eviction] =
        image_sizes_across_eviction(cx, "../images/screenshot.png", &notes, &path, &evict_path)
            .await;

    pretty_assertions::assert_eq!(initial, Some((4, 2)), "the image never finished loading");
    pretty_assertions::assert_eq!(after_rewrite, Some((4, 2)), "the cache reloaded unprompted");
    pretty_assertions::assert_eq!(
        after_eviction,
        Some((8, 3)),
        "a `../images/x.png` reference did not pick up the rewritten file, so \
         re-cropping a screenshot still needs the document reopened"
    );
}

/// Remote images keep going through gpui's shared asset cache: nothing on disk
/// changes under them, so the eviction path has nothing to do for them.
#[gpui::test]
fn test_remote_images_still_use_the_shared_asset_cache(cx: &mut TestAppContext) {
    let image_cache = cx.update(RetainAllImageCache::new);
    let source = resolve_image_source("https://example.com/a.png", None, &image_cache)
        .expect("an http destination should resolve");
    assert!(matches!(source, ImageSource::Resource(Resource::Uri(_))));
}

/// The image widget draws its selection border, its `</>` button and its resize
/// handle on a container that is supposed to hug the image exactly. gpui's
/// `div()` defaults to `display: block` (`Style::default`), and a block box with
/// `width: auto` fills its containing block — so that container stretches to its
/// own `max_w` cap and the border floats out to the right of any image narrower
/// than the cap. Wrapping it in a flex parent makes it shrink-wrap instead.
///
/// Both halves are pinned here: an upstream change that made `div()` shrink-wrap
/// by default, or that stopped flex items sizing to content, would silently move
/// the border off the image again.
#[gpui::test]
async fn test_a_block_child_fills_its_parent_but_a_flex_child_hugs(cx: &mut TestAppContext) {
    init_test(cx);

    const PARENT: gpui::Pixels = gpui::px(400.);
    const CHILD: gpui::Pixels = gpui::px(80.);

    struct Frames;
    impl Render for Frames {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(PARENT)
                // A block parent: the bordered child fills the whole width.
                .child(
                    div()
                        .debug_selector(|| "BLOCK-PARENT".into())
                        .child(div().w(CHILD).h(gpui::px(20.))),
                )
                // A flex parent: the same bordered child hugs its content.
                .child(
                    div().flex().child(
                        div()
                            .debug_selector(|| "FLEX-PARENT".into())
                            .child(div().w(CHILD).h(gpui::px(20.))),
                    ),
                )
        }
    }

    let (view, cx) = cx.add_window_view(|_window, _cx| Frames);
    cx.run_until_parked();
    cx.draw(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(600.), gpui::px(200.)),
        |_window, _cx| view.clone().into_any_element(),
    );
    cx.run_until_parked();

    let in_block = cx
        .debug_bounds("BLOCK-PARENT")
        .expect("the block-parented frame should have been laid out");
    let in_flex = cx
        .debug_bounds("FLEX-PARENT")
        .expect("the flex-parented frame should have been laid out");

    pretty_assertions::assert_eq!(
        in_block.size.width,
        PARENT,
        "a block child no longer fills its parent, so the image widget's container \
         may not need a flex parent to hug the image any more"
    );
    pretty_assertions::assert_eq!(
        in_flex.size.width,
        CHILD,
        "a flex child no longer hugs its content, so the image widget's selection \
         border will float out to the right of the image"
    );
}

/// The image widget puts an explicit `|width` on the `img` element itself as an
/// absolute length and lets the bordered container shrink-wrap it. That relies
/// on gpui's `img` deriving its height from an absolute width via the file's
/// aspect ratio — including widths *larger* than the file's natural size, the
/// shape an in-place crop leaves behind. The tempting alternative — size the
/// container and give the image `w_full()` — is broken: a fraction width with
/// auto height makes `img` fall back to the file's natural pixel height, so
/// the container sizes to that while the image itself lays out aspect-scaled,
/// painting everything past the natural height over the lines below the block.
#[gpui::test]
fn test_an_absolute_width_image_keeps_its_aspect_ratio(cx: &mut TestAppContext) {
    init_test(cx);

    // Natural size deliberately smaller than the displayed width.
    const NATURAL_WIDTH: u32 = 200;
    const NATURAL_HEIGHT: u32 = 100;
    const DISPLAY_WIDTH: gpui::Pixels = gpui::px(800.);
    const BORDER: gpui::Pixels = gpui::px(2.);

    struct Widget(Arc<gpui::RenderImage>);
    impl Render for Widget {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(gpui::px(1000.)).child(
                div().flex().max_w(gpui::px(1000.)).child(
                    div()
                        .debug_selector(|| "IMAGE-CONTAINER".into())
                        .border_2()
                        .child(
                            gpui::img(ImageSource::Render(self.0.clone()))
                                .id("contract-img")
                                .debug_selector(|| "IMAGE".into())
                                .max_w_full()
                                .rounded_sm()
                                .w(DISPLAY_WIDTH),
                        ),
                ),
            )
        }
    }

    let image = Arc::new(gpui::RenderImage::new([image::Frame::new(
        image::ImageBuffer::from_pixel(NATURAL_WIDTH, NATURAL_HEIGHT, image::Rgba([0, 0, 0, 255])),
    )]));
    let (view, cx) = cx.add_window_view(|_window, _cx| Widget(image));
    cx.run_until_parked();
    cx.draw(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(1200.), gpui::px(900.)),
        |_window, _cx| view.clone().into_any_element(),
    );
    cx.run_until_parked();

    let image_bounds = cx
        .debug_bounds("IMAGE")
        .expect("the image should have been laid out");
    let container_bounds = cx
        .debug_bounds("IMAGE-CONTAINER")
        .expect("the bordered container should have been laid out");

    let expected_height = DISPLAY_WIDTH * (NATURAL_HEIGHT as f32 / NATURAL_WIDTH as f32);
    pretty_assertions::assert_eq!(
        image_bounds.size,
        gpui::size(DISPLAY_WIDTH, expected_height),
        "an image with an absolute width no longer lays out at its aspect-scaled \
         height, so explicit `|width` sizes will render distorted or misplaced"
    );
    pretty_assertions::assert_eq!(
        container_bounds.size,
        gpui::size(DISPLAY_WIDTH + BORDER * 2., expected_height + BORDER * 2.),
        "the bordered container no longer hugs the aspect-scaled image, so an \
         image widened past its file's natural size will paint over the lines \
         below its block"
    );
}
#[gpui::test]
async fn test_heading_on_first_buffer_row(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    // A heading on row 0 renders like any other.
    cx.set_state(indoc::indoc! {"
        # Suzuri

        bodyˇ
    "});
    cx.executor().run_until_parked();
    assert_eq!(
        applied_block_count(&mut cx),
        1,
        "row-0 heading should render"
    );

    // Deleting a leading empty line from the heading's start (where the
    // cursor lands when the rendered heading is clicked) must merge the
    // empty line away, not eat the heading: the editor's own backspace
    // computes its range through the display map, whose movement clips
    // through replace blocks.
    cx.set_state(indoc::indoc! {"

        ˇ# Suzuri

        body
    "});
    cx.executor().run_until_parked();
    cx.dispatch_action(editor::actions::Backspace);
    cx.executor().run_until_parked();
    assert_eq!(cx.buffer_text(), "# Suzuri\n\nbody\n");
    // The cursor now sits on the heading row, so it stays revealed.
    assert_eq!(applied_block_count(&mut cx), 0);

    // Moving the cursor off the heading re-renders it, row 0 included.
    cx.update_editor(|editor, window, cx| {
        editor.move_down(&Default::default(), window, cx);
        editor.move_down(&Default::default(), window, cx);
    });
    cx.executor().run_until_parked();
    assert_eq!(
        applied_block_count(&mut cx),
        1,
        "row-0 heading should re-render after the cursor leaves"
    );

    // Forward-delete from the empty first line, heading still rendered:
    // this deleted the newline plus the heading's whole replaced range
    // before deletions bordering a block were taken over by the addon.
    cx.set_state(indoc::indoc! {"
        ˇ
        # Suzuri

        body
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);
    cx.dispatch_action(editor::actions::Delete);
    cx.executor().run_until_parked();
    assert_eq!(cx.buffer_text(), "# Suzuri\n\nbody\n");
}

#[gpui::test]
async fn test_click_on_widget_text_reveals_at_that_character(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        # Suzuri

        bodyˇ
    "});
    cx.executor().run_until_parked();
    assert_eq!(applied_block_count(&mut cx), 1);

    // What `on_source_click` runs when the click lands on the rendered
    // heading's own text (the wrapper's mouse-down never fires there —
    // `MarkdownElement` claims the click and prevents default): the source
    // reveals with the cursor at the clicked character.
    let range = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                BlockRenderKind::Markdown => Some(block.range.clone()),
                _ => None,
            })
            .expect("heading block")
    });
    let weak = cx.editor.downgrade();
    let handled =
        cx.update(|window, cx| reveal_at_source_index(&weak, &range, "# Su".len(), window, cx));
    assert!(handled);
    cx.executor().run_until_parked();

    assert_eq!(applied_block_count(&mut cx), 0, "source should reveal");
    let head = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        editor
            .selections
            .newest_anchor()
            .head()
            .to_offset(&snapshot)
            .0
    });
    assert_eq!(head, "# Su".len(), "cursor lands on the clicked character");

    // An index past the block (the widget's mini-document carries appended
    // reference definitions) clamps to the block's end.
    let handled = cx.update(|window, cx| reveal_at_source_index(&weak, &range, 10_000, window, cx));
    assert!(handled);
    cx.executor().run_until_parked();
    let head = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        editor
            .selections
            .newest_anchor()
            .head()
            .to_offset(&snapshot)
            .0
    });
    assert_eq!(head, "# Suzuri".len());
}
