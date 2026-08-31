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
    let add_range = frontmatter_range;
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
                        for index in (0..node.child_count()).rev() {
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

/// Drives the drop half of an image drag: moves the image on `row` so it lands
/// above `target_row`, which is what `finish_image_move` does on mouse up.
fn drop_image_on_row(cx: &mut EditorTestContext, row: u32, target_row: u32) {
    cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let range = snapshot.anchor_before(Point::new(row, 0))
            ..snapshot.anchor_after(Point::new(row, snapshot.line_len(MultiBufferRow(row))));
        move_image_to_row(editor, &range, target_row, cx);
    });
    cx.executor().run_until_parked();
}

/// The whole gesture, end to end, through real event dispatch: press on one
/// of two image widgets, drag past the arming threshold, cross the document,
/// release. This is what catches wiring bugs the unit tests cannot — most
/// importantly that the image that moves is the one that was pressed, which
/// broke in the field when every image block shared one element id and any
/// block's listener could claim the gesture.
#[gpui::test]
async fn test_the_full_drag_gesture_moves_the_pressed_image(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇtop\n\n![first](a.png)\n\n![second](b.png)\n\nbottom");
    cx.executor().run_until_parked();

    let first = cx
        .cx
        .debug_bounds("MDLP-IMAGE-a.png")
        .expect("the first image widget should have been laid out");

    // Press the first image and move past gpui's drag-arming threshold.
    cx.cx.simulate_event(MouseDownEvent {
        position: first.center(),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    cx.cx.simulate_event(gpui::MouseMoveEvent {
        position: first.center() + gpui::point(gpui::px(8.), gpui::px(8.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    cx.executor().run_until_parked();

    // Drag to the upper half of the last line, targeting the boundary above
    // it, and release there.
    let last_display_row =
        cx.update_editor(|editor, _, cx| editor.display_snapshot(cx).max_point().row());
    let last_line = cx.pixel_position_for(editor::DisplayPoint::new(last_display_row, 0));
    let drop_position = gpui::point(last_line.x, last_line.y - gpui::px(2.));
    cx.cx.simulate_event(gpui::MouseMoveEvent {
        position: drop_position,
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    cx.executor().run_until_parked();
    cx.cx.simulate_event(gpui::MouseUpEvent {
        position: drop_position,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    cx.executor().run_until_parked();

    pretty_assertions::assert_eq!(
        cx.buffer_text(),
        "top\n\n![second](b.png)\n\n![first](a.png)\n\nbottom",
        "the image that moves must be the one that was pressed"
    );
}

/// Sweeping a drag across heading widgets and other image widgets must keep
/// the drop cursor tracking the pointer the whole way; in the field the caret
/// froze partway through such a sweep while the pointer kept moving.
#[gpui::test]
async fn test_drop_tracking_survives_a_sweep_across_widgets(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(
        "ˇ# Title\n\nintro paragraph\n\n## Section\n\nfirst\n\n![a](a.png)\n\n![b](b.png)\n\nbottom",
    );
    cx.executor().run_until_parked();

    let second = cx
        .cx
        .debug_bounds("MDLP-IMAGE-b.png")
        .expect("the second image widget should have been laid out");
    let first = cx
        .cx
        .debug_bounds("MDLP-IMAGE-a.png")
        .expect("the first image widget should have been laid out");

    cx.cx.simulate_event(MouseDownEvent {
        position: second.center(),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    cx.cx.simulate_event(gpui::MouseMoveEvent {
        position: second.center() + gpui::point(gpui::px(8.), gpui::px(8.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    cx.executor().run_until_parked();

    // Sweep upward through the first image widget and the headings to the
    // very top of the document, in small steps like a real pointer.
    let x = first.center().x;
    let mut y = second.center().y;
    let top = gpui::px(4.);
    while y > top {
        y -= gpui::px(20.);
        cx.cx.simulate_event(gpui::MouseMoveEvent {
            position: gpui::point(x, y.max(top)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        cx.executor().run_until_parked();
    }

    let cursor_row = cx.update_editor(|editor, _, cx| {
        editor
            .selections
            .newest::<Point>(&editor.display_snapshot(cx))
            .head()
            .row
    });
    assert_eq!(
        cursor_row, 0,
        "after sweeping to the top of the document the drop cursor should \
         have followed the pointer to row 0, not frozen partway"
    );
}

/// The pointer-to-boundary mapping that decides where a dragged image lands,
/// driven against a really painted editor. It is the one piece of the drag
/// built on hand-rolled geometry, and nothing else would catch it drifting.
#[gpui::test]
async fn test_the_pointer_maps_to_the_nearest_row_boundary(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇone\ntwo\nthree\n");
    cx.executor().run_until_parked();

    // `pixel_position_for` reports the middle of the row, not its top.
    let row_center = |cx: &mut EditorTestContext, row: u32| {
        cx.pixel_position_for(editor::DisplayPoint::new(
            editor::display_map::DisplayRow(row),
            0,
        ))
    };
    let line_height = row_center(&mut cx, 1).y - row_center(&mut cx, 0).y;

    let boundary_at = |cx: &mut EditorTestContext, position: gpui::Point<gpui::Pixels>| {
        cx.update_editor(|editor, _, _| {
            editor
                .buffer_row_boundary_at_position(position)
                .map(|row| row.0)
        })
    };

    // The upper half of a line maps to the boundary above it, the lower half
    // to the boundary below.
    for row in 0..4 {
        let center = row_center(&mut cx, row);
        let upper = gpui::point(center.x, center.y - line_height / 4.);
        let lower = gpui::point(center.x, center.y + line_height / 4.);
        assert_eq!(
            boundary_at(&mut cx, upper),
            Some(row),
            "the upper half of row {row} should target the boundary above it"
        );
        assert_eq!(
            boundary_at(&mut cx, lower),
            Some(row + 1),
            "the lower half of row {row} should target the boundary below it"
        );
    }

    // Below every line, so an image can be dropped past the end of the
    // document rather than only ever above some existing line.
    let below = row_center(&mut cx, 3);
    let below = gpui::point(below.x, below.y + line_height * 2.);
    assert_eq!(boundary_at(&mut cx, below), Some(4));
}

#[gpui::test]
async fn test_dragging_an_image_moves_its_whole_line(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇfirst

        ![shot](a.png)

        last
    "});
    cx.executor().run_until_parked();

    drop_image_on_row(&mut cx, 2, 0);

    pretty_assertions::assert_eq!(
        cx.buffer_text(),
        indoc::indoc! {"
            ![shot](a.png)

            first

            last
        "},
        "the image should have moved above the first line, taking one of the \
         blank lines that surrounded it rather than leaving both behind"
    );
}

#[gpui::test]
async fn test_dragging_an_image_down_lands_above_the_target_line(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇ![shot](a.png)
        one
        two
        three
    "});
    cx.executor().run_until_parked();

    // Rows shift up by one once the image line is cut, which the move has to
    // account for or the image lands a line short.
    drop_image_on_row(&mut cx, 0, 3);

    pretty_assertions::assert_eq!(
        cx.buffer_text(),
        indoc::indoc! {"
            one
            two

            ![shot](a.png)

            three
        "},
        "the image landed against its neighbours, which in markdown makes it \
         part of their paragraph instead of a block of its own"
    );
}

#[gpui::test]
async fn test_dragging_an_image_past_the_last_line_appends_it(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state("ˇ![shot](a.png)\none\ntwo");
    cx.executor().run_until_parked();

    // One past the last row is what `buffer_row_at_position` reports when the
    // pointer is below every line.
    drop_image_on_row(&mut cx, 0, 3);

    pretty_assertions::assert_eq!(cx.buffer_text(), "one\ntwo\n\n![shot](a.png)");
}

#[gpui::test]
async fn test_dragging_the_last_line_image_up_leaves_no_blank_line(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    // No trailing newline, so the image's line has no newline of its own to
    // travel with; the one in front of it has to move instead.
    cx.set_state("ˇone\ntwo\n![shot](a.png)");
    cx.executor().run_until_parked();

    drop_image_on_row(&mut cx, 2, 0);

    pretty_assertions::assert_eq!(cx.buffer_text(), "![shot](a.png)\n\none\ntwo");
}

#[gpui::test]
async fn test_dropping_an_image_where_it_already_is_changes_nothing(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    let original = indoc::indoc! {"
        one
        ![shot](a.png)
        two
    "};
    cx.set_state(&format!("ˇ{original}"));
    cx.executor().run_until_parked();

    // Landing above its own line, and above the line under it, are both where
    // the image already sits.
    drop_image_on_row(&mut cx, 1, 1);
    pretty_assertions::assert_eq!(cx.buffer_text(), original);
    drop_image_on_row(&mut cx, 1, 2);
    pretty_assertions::assert_eq!(cx.buffer_text(), original);
}

#[gpui::test]
async fn test_a_moved_image_stays_selected(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇone
        two
        ![shot](a.png)
    "});
    cx.executor().run_until_parked();

    drop_image_on_row(&mut cx, 2, 0);

    let selected_row = cx.update_editor(|editor, _, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        editor
            .addon::<LivePreviewAddon>()
            .and_then(|addon| addon.selected_image.clone())
            .map(|range| range.start.to_point(&snapshot).row)
    });
    assert_eq!(
        selected_row,
        Some(0),
        "the widget lost its selection border on drop, so its handles vanish \
         the moment it lands"
    );
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

    // The two standalone image embeds render as image blocks, and the note
    // embed as a transclusion card; only the inline embed stays raw.
    assert_eq!(applied_block_count(&mut cx), 3);
    let display = cx.display_text();
    assert!(
        display.contains("inline stays raw: ![[photo.png]]"),
        "{display}"
    );

    let image_blocks = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .expect("markdown buffer should produce markers")
            .blocks
            .iter()
            .filter(|block| matches!(block.kind, BlockRenderKind::Image { .. }))
            .count()
    });
    assert_eq!(
        image_blocks, 2,
        "an image target must not become a note embed"
    );
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
    assert!(
        display.contains("loose delimiters: a == b == c"),
        "{display}"
    );
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

#[gpui::test]
async fn test_callouts_render_as_callout_blocks(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        > [!warning] Careful
        > This is the body.

        > an ordinary quote
        > stays an ordinary quote
    "});
    cx.executor().run_until_parked();

    let kinds = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .expect("markdown buffer should produce markers")
            .blocks
            .iter()
            .map(|block| block.kind.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds.len(), 2, "one callout and one plain quote");
    assert!(
        matches!(
            &kinds[0],
            BlockRenderKind::Callout {
                kind: CalloutKind::Warning,
                title,
                collapse: None,
            } if title == "Careful"
        ),
        "expected a warning callout"
    );
    assert!(
        matches!(kinds[1], BlockRenderKind::Markdown),
        "a plain block quote must keep rendering as one"
    );
}

#[gpui::test]
async fn test_collapsed_callout_hides_its_body(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        > [!note]- Folded
        > Hidden body.

        > [!note]+ Open
        > Shown body.
    "});
    cx.executor().run_until_parked();

    let collapsed = cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .unwrap()
            .applied_blocks
            .iter()
            .map(|block| block.collapsed)
            .collect::<Vec<_>>()
    });
    assert_eq!(
        collapsed,
        vec![true, false],
        "`-` starts collapsed, `+` starts expanded"
    );
}

#[test]
fn test_parse_callout_header() {
    let parse = parse_callout_header;

    // An untitled callout takes its type's name as the title, like Obsidian.
    assert!(matches!(
        parse("> [!note]"),
        Some((CalloutKind::Note, ref title, None)) if title == "Note"
    ));
    assert!(matches!(
        parse("> [!TIP] Try this"),
        Some((CalloutKind::Tip, ref title, None)) if title == "Try this"
    ));

    // Aliases collapse onto one look.
    assert!(matches!(
        parse("> [!tldr]"),
        Some((CalloutKind::Abstract, ..))
    ));
    assert!(matches!(
        parse("> [!faq]"),
        Some((CalloutKind::Question, ..))
    ));
    assert!(matches!(
        parse("> [!caution]"),
        Some((CalloutKind::Warning, ..))
    ));

    // Collapse suffixes.
    assert!(matches!(parse("> [!note]- x"), Some((_, _, Some(true)))));
    assert!(matches!(parse("> [!note]+ x"), Some((_, _, Some(false)))));
    // The fold character binds to the `]`; a title may still open with a dash.
    assert!(matches!(
        parse("> [!note] - a dash"),
        Some((_, ref title, None)) if title == "- a dash"
    ));

    // An unknown type is still a callout, styled as a note.
    assert!(matches!(
        parse("> [!recipe] Pasta"),
        Some((CalloutKind::Note, ..))
    ));

    // Not callouts.
    assert!(parse("> just a quote").is_none());
    assert!(parse("> [!two words] x").is_none());
    assert!(parse("> [!unclosed").is_none());
    assert!(parse("[!note] no quote marker").is_none());
}

#[test]
fn test_callout_body_strips_quote_prefixes() {
    assert_eq!(
        callout_body("> [!note] Title\n> first\n> - item\n>\n> last"),
        "first\n- item\n\nlast"
    );
    // Reference definitions are appended past the quote's own text and carry
    // no `>`; they must survive the strip.
    assert_eq!(
        callout_body("> [!note]\n> see [ref]\n\n[ref]: https://example.com"),
        "see [ref]\n\n[ref]: https://example.com"
    );
    assert_eq!(callout_body("> [!note] Title"), "");
}

#[gpui::test]
async fn test_note_embeds_extract_as_embed_blocks(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        ![[Some Note]]

        ![[Some Note#Method]]

        ![[photo.png]]

        inline ![[Some Note]] stays raw
    "});
    cx.executor().run_until_parked();

    let kinds = cx.update_editor(|editor, _, cx| {
        extract_markers(editor, cx)
            .expect("markdown buffer should produce markers")
            .blocks
            .iter()
            .map(|block| block.kind.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds.len(), 3, "an inline embed must not become a block");
    assert!(
        matches!(&kinds[0], BlockRenderKind::Embed { target, section: None } if target == "Some Note"),
        "a bare note embed"
    );
    assert!(
        matches!(
            &kinds[1],
            BlockRenderKind::Embed { target, section: Some(section) }
                if target == "Some Note" && section == "Method"
        ),
        "a section embed"
    );
    assert!(
        matches!(kinds[2], BlockRenderKind::Image { .. }),
        "an image target must still render as an image"
    );

    // The inline embed keeps its source on screen rather than disappearing.
    assert!(
        cx.display_text()
            .contains("inline ![[Some Note]] stays raw"),
        "{}",
        cx.display_text()
    );
}

#[test]
fn test_embed_section_extraction() {
    let note = indoc::indoc! {"
        # Title

        Intro text.

        ## Method

        The method body.

        ### Detail

        Nested detail.

        ## Results

        Results body.
    "};

    let method = embed_section(note, "Method").expect("the heading exists");
    assert!(method.contains("The method body."), "{method}");
    // A deeper heading belongs to the section; a sibling ends it.
    assert!(method.contains("Nested detail."), "{method}");
    assert!(!method.contains("Results body."), "{method}");

    // The last section runs to the end of the note.
    let results = embed_section(note, "Results").expect("the heading exists");
    assert!(results.contains("Results body."), "{results}");

    // Matching ignores case, and a missing heading is reported rather than
    // silently embedding the whole note.
    assert!(embed_section(note, "method").is_some());
    assert!(embed_section(note, "Nowhere").is_none());
}

#[gpui::test]
async fn test_resolve_embed_target(cx: &mut TestAppContext) {
    init_test(cx);
    use project::Fs as _;

    let fs = project::FakeFs::new(cx.executor());
    for directory in [
        "/vault",
        "/vault/notes",
        "/vault/notes/deep",
        "/vault/archive",
    ] {
        fs.create_dir(directory.as_ref())
            .await
            .expect("failed to create the test directory");
    }
    for path in [
        "/vault/Index.md",
        "/vault/notes/Method.md",
        "/vault/notes/deep/Method.md",
        "/vault/archive/Index.md",
    ] {
        fs.insert_file(path, Vec::new()).await;
    }
    let project = project::Project::test(fs, ["/vault".as_ref()], cx).await;

    let resolve =
        |directory: Option<&'static str>, target: &'static str, cx: &mut TestAppContext| {
            let project = project.clone();
            cx.update(|cx| {
                resolve_embed_target(&project, directory.map(Path::new), target, cx)
                    .map(|(_, absolute)| absolute)
            })
        };

    // A bare name prefers the note beside the embedding one over a deeper match.
    assert_eq!(
        resolve(Some("/vault/notes"), "Method", cx),
        Some(PathBuf::from("/vault/notes/Method.md"))
    );
    // With no directory to prefer, the shallowest match wins.
    assert_eq!(
        resolve(None, "Index", cx),
        Some(PathBuf::from("/vault/Index.md"))
    );
    // A target that spells out a path matches that path exactly, beating the
    // shallower note of the same name.
    assert_eq!(
        resolve(None, "archive/Index", cx),
        Some(PathBuf::from("/vault/archive/Index.md"))
    );
    // The `.md` extension is implied but may be written out.
    assert_eq!(
        resolve(None, "notes/Method.md", cx),
        Some(PathBuf::from("/vault/notes/Method.md"))
    );
    assert_eq!(resolve(None, "Nowhere", cx), None);
}

/// Clicks the center of a rendered element located by its debug selector.
fn click_debug_element(cx: &mut EditorTestContext, selector: &'static str) {
    let bounds = cx
        .cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"));
    cx.cx.simulate_mouse_down(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.cx.simulate_mouse_up(
        bounds.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();
}

fn callout_collapse_states(cx: &mut EditorTestContext) -> Vec<bool> {
    cx.update_editor(|editor, _, _| {
        editor
            .addon::<LivePreviewAddon>()
            .unwrap()
            .applied_blocks
            .iter()
            .map(|block| block.collapsed)
            .collect()
    })
}

#[gpui::test]
async fn test_clicking_a_callout_title_toggles_it(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        > [!danger]- Folded
        > Hidden body.
    "});
    cx.executor().run_until_parked();
    assert_eq!(callout_collapse_states(&mut cx), vec![true]);

    // Expanding must not also reveal the source: the title claims the click,
    // so the widget stays on screen instead of dissolving into markdown.
    click_debug_element(&mut cx, "mdlp-callout-title-Folded");
    assert_eq!(callout_collapse_states(&mut cx), vec![false]);
    assert!(
        !cx.display_text().contains("[!danger]"),
        "{}",
        cx.display_text()
    );

    click_debug_element(&mut cx, "mdlp-callout-title-Folded");
    assert_eq!(callout_collapse_states(&mut cx), vec![true]);
}

#[gpui::test]
async fn test_clicking_a_plain_callout_title_reveals_its_source(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        > [!note] Plain
        > Body text.
    "});
    cx.executor().run_until_parked();
    assert!(!cx.display_text().contains("[!note]"), "starts rendered");

    // A callout with no `+`/`-` is not collapsible, so its title falls
    // through to the wrapper and reveals the markdown for editing.
    click_debug_element(&mut cx, "mdlp-callout-title-Plain");
    assert!(
        cx.display_text().contains("> [!note] Plain"),
        "{}",
        cx.display_text()
    );
}

/// An editor over a real project, which transclusion needs: a target is a
/// note name resolved against the project's worktrees, not a path resolved
/// against the buffer's directory, so an editor with no project can only ever
/// report the target missing.
async fn markdown_vault_test_context<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&'static str, &'static str)],
    open: &str,
) -> (
    Entity<Editor>,
    Arc<project::FakeFs>,
    &'a mut gpui::VisualTestContext,
) {
    use project::Fs as _;

    init_test(cx);
    let fs = project::FakeFs::new(cx.executor());
    fs.create_dir("/vault".as_ref())
        .await
        .expect("failed to create the vault");
    for (path, content) in files {
        fs.insert_file(format!("/vault/{path}"), content.as_bytes().to_vec())
            .await;
    }
    let project = project::Project::test(fs.clone(), ["/vault".as_ref()], cx).await;

    let registry = project.read_with(cx, |project, _| project.languages().clone());
    registry.add(language::markdown_lang());
    registry.add(markdown_inline_lang());

    let buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer(format!("/vault/{open}"), cx)
        })
        .await
        .expect("failed to open the note");
    cx.executor().run_until_parked();

    let (editor, cx) = cx.add_window_view({
        let project = project.clone();
        |window, cx| Editor::for_buffer(buffer, Some(project), window, cx)
    });
    cx.run_until_parked();
    (editor, fs, cx)
}

/// The markdown a transclusion widget is currently drawing, or the state it is
/// reporting instead. Read from the applied block's source, which is where
/// `EmbedState::reuse_key` records what reached the screen.
fn embed_block_sources(editor: &Entity<Editor>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
    editor.update(cx, |editor, _| {
        editor
            .addon::<LivePreviewAddon>()
            .unwrap()
            .applied_blocks
            .iter()
            .filter(|block| block.source.starts_with('\u{0}'))
            .map(|block| block.source.clone())
            .collect()
    })
}

#[gpui::test]
async fn test_an_embed_loads_its_target_and_redraws(cx: &mut TestAppContext) {
    use project::Fs as _;

    let (editor, fs, cx) = markdown_vault_test_context(
        cx,
        &[
            ("Note.md", "Opening line.\n\n![[Method]]\n"),
            ("Method.md", "# Method\n\nWe sampled 40 participants.\n"),
        ],
        "Note.md",
    )
    .await;

    let sources = embed_block_sources(&editor, cx);
    assert_eq!(sources.len(), 1, "one transclusion block");
    assert!(
        sources[0].contains("We sampled 40 participants."),
        "the load never reached the screen: {:?}",
        sources[0]
    );

    // Changing the target on disk evicts its cache entry and redraws, so an
    // embed does not keep showing a note that has since been edited.
    fs.save(
        "/vault/Method.md".as_ref(),
        &"# Method\n\nWe sampled 80 participants.\n".into(),
        Default::default(),
    )
    .await
    .expect("failed to rewrite the target");
    cx.run_until_parked();

    let sources = embed_block_sources(&editor, cx);
    assert!(
        sources[0].contains("We sampled 80 participants."),
        "a target edited on disk kept showing its old text: {:?}",
        sources[0]
    );
}

#[gpui::test]
async fn test_an_unresolved_embed_reports_itself(cx: &mut TestAppContext) {
    let (editor, _fs, cx) = markdown_vault_test_context(
        cx,
        &[("Note.md", "Opening line.\n\n![[Nowhere]]\n")],
        "Note.md",
    )
    .await;

    let sources = embed_block_sources(&editor, cx);
    assert_eq!(sources.len(), 1, "a missing target still draws a block");
    assert!(
        sources[0].contains("missing"),
        "expected the missing state, got {:?}",
        sources[0]
    );
}

/// The keys carried by a highlight namespace, as buffer text, so citation
/// tests can assert which keys resolved rather than counting ranges.
fn highlighted_texts(
    editor: &Entity<Editor>,
    key: usize,
    cx: &mut gpui::VisualTestContext,
) -> Vec<String> {
    editor.update(cx, |editor, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        editor
            .text_highlights(HighlightKey::MarkdownLivePreview(key), cx)
            .map_or(Vec::new(), |(_, ranges)| {
                ranges
                    .iter()
                    .map(|range| snapshot.text_for_range(range.clone()).collect())
                    .collect()
            })
    })
}

#[gpui::test]
async fn test_cite_keys_resolve_against_the_vault_bibliography(cx: &mut TestAppContext) {
    use project::Fs as _;

    let (editor, fs, cx) = markdown_vault_test_context(
        cx,
        &[
            (
                "Note.md",
                "As shown in [@smith2020] and [@missing2024].\n",
            ),
            (
                "refs.bib",
                "@article{smith2020,\n  title = {A Study},\n  author = {Smith, Jane},\n  year = {2020},\n}\n",
            ),
        ],
        "Note.md",
    )
    .await;
    cx.run_until_parked();

    assert_eq!(
        highlighted_texts(&editor, CITATION, cx),
        vec!["@smith2020"],
        "the key present in refs.bib keeps the citation chip"
    );
    assert_eq!(
        highlighted_texts(&editor, CITATION_UNKNOWN, cx),
        vec!["@missing2024"],
        "the key absent from refs.bib is flagged unresolved"
    );

    // Adding the missing entry to the `.bib` clears the flag without
    // touching the note: the worktree change reloads the index and the
    // bibliography observer restyles.
    fs.save(
        "/vault/refs.bib".as_ref(),
        &concat!(
            "@article{smith2020,\n  title = {A Study},\n  author = {Smith, Jane},\n  year = {2020},\n}\n",
            "@article{missing2024,\n  title = {Found},\n  year = {2024},\n}\n"
        )
        .into(),
        Default::default(),
    )
    .await
    .expect("failed to update refs.bib");
    cx.run_until_parked();

    let mut resolved = highlighted_texts(&editor, CITATION, cx);
    resolved.sort();
    assert_eq!(
        resolved,
        vec!["@missing2024", "@smith2020"],
        "both keys resolve after the bib gains the entry"
    );
    assert_eq!(
        highlighted_texts(&editor, CITATION_UNKNOWN, cx),
        Vec::<String>::new()
    );
}

#[gpui::test]
async fn test_citations_stay_plain_without_a_bibliography(cx: &mut TestAppContext) {
    let (editor, _fs, cx) = markdown_vault_test_context(
        cx,
        &[("Note.md", "A hunch [@unverified] in a vault with no bib.\n")],
        "Note.md",
    )
    .await;
    cx.run_until_parked();

    assert_eq!(
        highlighted_texts(&editor, CITATION, cx),
        vec!["@unverified"],
        "citations keep the ordinary chip"
    );
    assert_eq!(
        highlighted_texts(&editor, CITATION_UNKNOWN, cx),
        Vec::<String>::new(),
        "no bibliography means no unresolved flags"
    );
}

#[gpui::test]
async fn test_citation_completion_offers_bib_keys(cx: &mut TestAppContext) {
    use editor::CompletionProvider as _;

    let (editor, _fs, cx) = markdown_vault_test_context(
        cx,
        &[
            ("Note.md", "cite [@smi"),
            (
                "refs.bib",
                concat!(
                    "@article{smith2020,\n  title = {A Study of Things},\n  author = {Smith, Jane},\n  year = {2020},\n}\n",
                    "@book{knuth1984,\n  title = {The Book},\n  year = {1984},\n}\n"
                ),
            ),
        ],
        "Note.md",
    )
    .await;
    cx.run_until_parked();

    let task = editor.update_in(cx, |editor, window, cx| {
        let project = editor
            .project()
            .expect("vault editor has a project")
            .clone();
        let provider = CitationCompletionProvider::new(project, cx);
        let buffer = editor
            .buffer()
            .read(cx)
            .as_singleton()
            .expect("single buffer");
        let position = buffer.read(cx).anchor_before("cite [@smi".len());
        provider.completions(
            &buffer,
            position,
            editor::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
            window,
            cx,
        )
    });
    let responses = task.await.expect("completions succeed");

    let completions: Vec<_> = responses
        .into_iter()
        .flat_map(|response| response.completions)
        .collect();
    let mut keys: Vec<_> = completions
        .iter()
        .map(|completion| completion.new_text.clone())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["knuth1984", "smith2020"],
        "every bib entry is offered; the menu filters as the user types"
    );

    // The replacement covers only the typed key fragment, not the `@`, so
    // accepting a completion yields `[@smith2020` rather than doubling up.
    let smith = completions
        .iter()
        .find(|completion| completion.new_text == "smith2020")
        .expect("smith2020 offered");
    editor.update(cx, |editor, cx| {
        let buffer = editor
            .buffer()
            .read(cx)
            .as_singleton()
            .expect("single buffer");
        let buffer = buffer.read(cx);
        let replaced = {
            use text::ToOffset as _;
            smith.replace_range.start.to_offset(buffer)..smith.replace_range.end.to_offset(buffer)
        };
        assert_eq!(&buffer.text()[replaced], "smi");
    });
}

/// A vault often holds several copies of one master bibliography (a
/// `references.bib` per paper); a key defined in many files is still one
/// citation and must appear once in the menu.
#[gpui::test]
async fn test_duplicate_bib_keys_complete_once(cx: &mut TestAppContext) {
    use editor::CompletionProvider as _;

    let shared = "@article{smith2020,\n  title = {A Study},\n  year = {2020},\n}\n";
    let (editor, _fs, cx) = markdown_vault_test_context(
        cx,
        &[
            ("Note.md", "cite [@smi"),
            ("refs.bib", shared),
            ("paper-copy.bib", shared),
            ("another-copy.bib", shared),
        ],
        "Note.md",
    )
    .await;
    cx.run_until_parked();

    let task = editor.update_in(cx, |editor, window, cx| {
        let project = editor.project().expect("vault editor has a project").clone();
        let provider = CitationCompletionProvider::new(project, cx);
        let buffer = editor
            .buffer()
            .read(cx)
            .as_singleton()
            .expect("single buffer");
        let position = buffer.read(cx).anchor_before("cite [@smi".len());
        provider.completions(
            &buffer,
            position,
            editor::CompletionContext {
                trigger_kind: lsp::CompletionTriggerKind::INVOKED,
                trigger_character: None,
            },
            window,
            cx,
        )
    });
    let responses = task.await.expect("completions succeed");

    let keys: Vec<_> = responses
        .into_iter()
        .flat_map(|response| response.completions)
        .map(|completion| completion.new_text)
        .collect();
    assert_eq!(
        keys,
        vec!["smith2020"],
        "a key defined in three .bib files is offered exactly once"
    );
}

/// Pandoc also allows in-text citations with no brackets (`@key argues`),
/// which is what accepting a bare `@` completion produces. They verify
/// exactly like bracketed ones — chip when resolved, red flag when not —
/// while emails, infix `@`, and code spans never register at all.
#[gpui::test]
async fn test_bare_in_text_citations_chip_and_flag_like_bracketed(cx: &mut TestAppContext) {
    let (editor, _fs, cx) = markdown_vault_test_context(
        cx,
        &[
            (
                "Note.md",
                "Bare: @smith2020 argues this, though @somehandle disagrees, \
                 and me@example.com or `@smith2020` never chip.\n",
            ),
            (
                "refs.bib",
                "@article{smith2020,\n  title = {A Study},\n  year = {2020},\n}\n",
            ),
        ],
        "Note.md",
    )
    .await;
    cx.run_until_parked();

    assert_eq!(
        highlighted_texts(&editor, CITATION, cx),
        vec!["@smith2020"],
        "the bare key that resolves chips"
    );
    assert_eq!(
        highlighted_texts(&editor, CITATION_UNKNOWN, cx),
        vec!["@somehandle"],
        "the bare key that resolves to nothing flags, like a bracketed one"
    );
}

/// A citation arrives as one token through completion, so backspacing right
/// after `]` (or forward-deleting right before `[`) removes the whole group,
/// like the block widgets do. With the cursor inside the group, deletion
/// stays per-character so a key can still be edited.
#[gpui::test]
async fn test_backspace_after_a_citation_deletes_the_whole_group(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state("A claim [@doe2020]ˇ stands\n");
    cx.executor().run_until_parked();
    cx.dispatch_action(editor::actions::Backspace);
    cx.assert_editor_state("A claim ˇ stands\n");

    cx.set_state("A claim ˇ[@doe2020] stands\n");
    cx.executor().run_until_parked();
    cx.dispatch_action(editor::actions::Delete);
    cx.assert_editor_state("A claim ˇ stands\n");

    // Inside the group the ordinary one-character deletion still applies.
    cx.set_state("A claim [@doe2020ˇ] stands\n");
    cx.executor().run_until_parked();
    cx.dispatch_action(editor::actions::Backspace);
    cx.assert_editor_state("A claim [@doe202ˇ] stands\n");
}

#[gpui::test]
async fn test_citation_key_start_finds_pandoc_contexts(cx: &mut TestAppContext) {
    let cases: &[(&str, Option<usize>)] = &[
        // Bracketed and bare citations complete after their `@`.
        ("see [@smi", Some("see [@".len())),
        ("see @smi", Some("see @".len())),
        ("@smi", Some(1)),
        ("[@a; @b", Some("[@a; @".len())),
        // An email address or infix `@` is not a citation.
        ("write me@exa", None),
        // No `@` at all.
        ("see [smi", None),
    ];
    for (text, expected) in cases {
        let buffer = cx.new(|cx| language::Buffer::local(*text, cx));
        let start =
            cx.update(|cx| crate::bibliography::citation_key_start(buffer.read(cx), text.len()));
        assert_eq!(start, *expected, "context detection for {text:?}");
    }
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

/// `register_editor` attaches to every full editor, and the git panel and
/// project diff open non-singleton multibuffers — the shape built here. There
/// the addon must be completely inert (no concealments, blocks, or highlights)
/// so a diff of markdown files reads as plain source, and its editing actions
/// must pass through to the editor's own untouched.
///
/// `extract_markers` bails at `as_singleton()`, which is what makes this hold;
/// this pins that, since a future change reaching for the first excerpt's
/// buffer instead would decorate multibuffer rows using excerpt-relative
/// offsets.
#[gpui::test]
async fn test_multibuffer_excerpts_stay_plain_source(cx: &mut TestAppContext) {
    init_test(cx);
    let mut cx = EditorTestContext::new_multibuffer(
        cx,
        [
            "«# One\n\nsome **bold** and *italic* text\n»",
            "«# Two\n\n- [ ] task item\n»",
        ],
    );
    let markdown = language::markdown_lang();
    cx.update_multibuffer(|multibuffer, cx| {
        for buffer in multibuffer.all_buffers() {
            buffer.update(cx, |buffer, cx| {
                buffer.set_language(Some(markdown.clone()), cx)
            });
        }
    });
    cx.executor().run_until_parked();

    // The addon is attached (`applied_block_count` would panic otherwise),
    // but with markdown excerpts on both sides it decorates nothing.
    assert_eq!(
        applied_block_count(&mut cx),
        0,
        "a diff multibuffer must not render preview blocks"
    );
    let display = cx.display_text();
    assert!(
        display.contains("# One") && display.contains("**bold**"),
        "syntax markers must stay visible in a diff multibuffer: {display:?}"
    );
    for key in [BOLD, ITALIC] {
        let highlights = cx.update_editor(|editor, _, cx| {
            editor
                .text_highlights(HighlightKey::MarkdownLivePreview(key), cx)
                .map_or(0, |(_, ranges)| ranges.len())
        });
        assert_eq!(highlights, 0, "no emphasis styling in a diff multibuffer");
    }

    // The addon's Backspace handler must propagate to the editor's own.
    let before = cx.buffer_text();
    cx.update_editor(|editor, window, cx| {
        editor.move_to_end(&Default::default(), window, cx);
    });
    cx.dispatch_action(editor::actions::Backspace);
    cx.executor().run_until_parked();
    assert_eq!(
        cx.buffer_text().len(),
        before.len() - 1,
        "backspace must edit normally in a diff multibuffer"
    );
}

/// An expanded diff hunk splices the deleted rows into the multibuffer's
/// coordinate space, so the buffer byte offsets extraction anchors with stop
/// addressing multibuffer positions: every decoration below the hunk lands
/// shifted by the deleted text's length. The damage is not cosmetic — without
/// the guard this fixture rendered as
///
/// ```text
/// # tle
/// ome **bold** text
/// ```
///
/// with both title rows gone and a character eaten off the body, because the
/// heading block replaced the wrong rows and concealments hid the wrong spans.
/// The preview therefore retreats to plain source while any hunk is expanded,
/// and returns when they collapse.
#[gpui::test]
async fn test_expanded_diff_hunks_reveal_plain_source(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;

    cx.set_state(indoc::indoc! {"
        # Title

        some **bold** textˇ
    "});
    cx.executor().run_until_parked();
    assert_eq!(
        applied_block_count(&mut cx),
        1,
        "heading renders as a block"
    );
    assert!(cx.display_text().contains("some bold text"));

    cx.set_head_text(indoc::indoc! {"
        # Old Title

        some **bold** text
    "});
    cx.executor().run_until_parked();
    cx.dispatch_action(editor::actions::ExpandAllDiffHunks);
    cx.executor().run_until_parked();

    assert_eq!(
        applied_block_count(&mut cx),
        0,
        "an expanded hunk must drop the preview to plain source"
    );
    let display = cx.display_text();
    assert!(
        display.contains("# Old Title"),
        "the deleted rows show: {display:?}"
    );
    assert!(
        display.contains("# Title") && display.contains("**bold**"),
        "current source shows unconcealed: {display:?}"
    );

    cx.dispatch_action(editor::actions::CollapseAllDiffHunks);
    cx.executor().run_until_parked();
    assert_eq!(
        applied_block_count(&mut cx),
        1,
        "collapsing the hunks brings the preview back"
    );
    assert!(cx.display_text().contains("some bold text"));
}

/// The image drag follows the pointer across the whole document, which only
/// works because gpui delivers `on_drag_move` to every painted listener of the
/// dragged type, not just those under the pointer. If upstream started gating
/// it on the element's bounds, dragging an image would stop tracking the moment
/// the pointer left the image itself.
#[gpui::test]
fn test_drag_move_fires_outside_the_element(cx: &mut TestAppContext) {
    init_test(cx);

    struct Dragged;

    struct DragSource(Rc<Cell<usize>>);

    impl Render for DragSource {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let moves = self.0.clone();
            div().size_full().child(
                div()
                    .id("source")
                    .debug_selector(|| "SOURCE".into())
                    .w(gpui::px(20.))
                    .h(gpui::px(20.))
                    .on_drag(Dragged, |_, _, _, cx| cx.new(|_| EmptyDragPreview))
                    .on_drag_move::<Dragged>(move |_, _, _| {
                        moves.set(moves.get() + 1);
                    }),
            )
        }
    }

    let moves = Rc::new(Cell::new(0));
    let (_view, cx) = cx.add_window_view({
        let moves = moves.clone();
        move |_window, _cx| DragSource(moves)
    });
    cx.run_until_parked();

    let bounds = cx
        .debug_bounds("SOURCE")
        .expect("the drag source should have been laid out");
    cx.simulate_event(MouseDownEvent {
        position: bounds.center(),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    // Past the drag threshold, but still over the source, so the drag starts.
    cx.simulate_event(gpui::MouseMoveEvent {
        position: bounds.center() + gpui::point(gpui::px(8.), gpui::px(0.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    cx.run_until_parked();

    let moves_before = moves.get();
    cx.simulate_event(gpui::MouseMoveEvent {
        position: bounds.center() + gpui::point(gpui::px(300.), gpui::px(300.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });

    assert!(
        moves.get() > moves_before,
        "gpui stopped delivering `on_drag_move` for pointer moves outside the \
         dragged element, so an image drag can no longer track the pointer"
    );
}

#[gpui::test]
async fn test_narrow_table_columns_take_content_width(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    cx.set_state(indoc::indoc! {"
        ˇplain line

        | R | School | E |
        | --- | --- | --- |
        | 1 | Duke | 4.5 |
    "});
    cx.executor().run_until_parked();

    let window_width = cx.update_editor(|_, window, _| window.bounds().size.width);
    let mut total = gpui::px(0.);
    for column in 0..3 {
        let bounds = cx
            .cx
            .debug_bounds(format!("mdlp-cell-h-{column}").leak())
            .expect("header cell rendered");
        assert!(
            bounds.size.width < gpui::px(120.),
            "short column {column} should hug its content, got {:?}",
            bounds.size.width
        );
        total += bounds.size.width;
    }
    assert!(
        total < window_width / 2.,
        "a small table should not stretch across the editor: {total:?} of {window_width:?}"
    );
}

#[gpui::test]
async fn test_wide_table_scrolls_horizontally_in_place(cx: &mut TestAppContext) {
    let mut cx = markdown_test_context(cx).await;
    let long = "a-fairly-long-piece-of-cell-content-that-goes-on-and-on-for-a-while";
    let header: String = (0..7).map(|i| format!("| Column {i} ")).collect::<String>() + "|";
    let separator = "| --- ".repeat(7) + "|";
    let row: String = (0..7).map(|_| format!("| {long} ")).collect::<String>() + "|";
    cx.set_state(&format!("ˇplain line\n\n{header}\n{separator}\n{row}\n"));
    cx.executor().run_until_parked();

    let window_width = cx.update_editor(|_, window, _| window.bounds().size.width);
    let container = cx
        .cx
        .debug_bounds("mdlp-table-scroll")
        .expect("scroll container rendered");
    assert!(
        container.right() <= window_width,
        "the scroll container must stay within the window: {container:?}"
    );
    let last_cell = cx
        .cx
        .debug_bounds("mdlp-cell-h-6")
        .expect("last header cell rendered");
    assert!(
        last_cell.right() > window_width,
        "the grid content should overflow the window before scrolling: {last_cell:?}"
    );

    // Wheel over the table scrolls the grid in place instead of the editor.
    cx.cx.simulate_mouse_move(
        container.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.executor().run_until_parked();
    cx.cx.simulate_event(gpui::ScrollWheelEvent {
        position: container.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(-600.), gpui::px(0.))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.executor().run_until_parked();
    cx.update_editor(|_, _, cx| cx.notify());
    cx.executor().run_until_parked();

    let editor_scroll = cx.update_editor(|editor, _, cx| editor.scroll_position(cx));
    assert_eq!(
        editor_scroll.x, 0.0,
        "the table consumes the horizontal wheel; the editor must not scroll"
    );
    let first_cell = cx
        .cx
        .debug_bounds("mdlp-cell-h-0")
        .expect("first header cell rendered");
    assert!(
        first_cell.origin.x < container.origin.x,
        "the grid should have scrolled left: {first_cell:?} vs {container:?}"
    );
}
