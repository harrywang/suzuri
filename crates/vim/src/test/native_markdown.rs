// SUZURI: Exercise concealed heading source through real Vim input and the live-preview addon.
use super::VimTestContext;
use crate::state::Mode;

async fn markdown_context(cx: &mut gpui::TestAppContext, text: &str) -> VimTestContext {
    VimTestContext::init(cx);
    cx.update(markdown_live_preview::init);
    let mut context = VimTestContext::new_markdown_with_rust(cx).await;
    context.update_workspace(|workspace, _, cx| {
        workspace
            .project()
            .read(cx)
            .languages()
            .add(std::sync::Arc::new(language::Language::new(
                language::LanguageConfig {
                    name: "Markdown-Inline".into(),
                    hidden: true,
                    ..Default::default()
                },
                Some(tree_sitter_md::INLINE_LANGUAGE.into()),
            )));
    });
    context.set_state(text, Mode::Normal);
    context.executor().run_until_parked();
    context
}

#[gpui::test]
async fn test_native_heading_delete_previous_line(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    assert_eq!(cx.display_text(), "above\nHeading\nbody");
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_blank_previous_line(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇ\n## Heading\nbody").await;
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
    cx.simulate_keystrokes("u");
    assert_eq!(cx.buffer_text(), "\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_counted_delete(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇone\ntwo\n## Heading\nbody").await;
    cx.simulate_keystrokes("2 d d");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_visual_line_delete(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("shift-v d");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_yank_and_paste(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("j y y p");
    assert_eq!(cx.buffer_text(), "above\n## Heading\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_change_previous_line(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("c c r e p l a c e escape");
    assert_eq!(cx.buffer_text(), "replace\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_and_paste(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("j d d shift-p");
    assert_eq!(cx.buffer_text(), "above\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_join_and_undo(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("shift-j");
    assert_eq!(cx.buffer_text(), "above ## Heading\nbody");
    cx.simulate_keystrokes("u");
    assert_eq!(cx.buffer_text(), "above\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_insert_line_above(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("j shift-o n e w escape");
    assert_eq!(cx.buffer_text(), "above\nnew\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_all_levels(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇ").await;
    for level in 1..=6 {
        let prefix = "#".repeat(level);
        cx.set_state(&format!("ˇ\n{prefix} Heading\nbody"), Mode::Normal);
        cx.executor().run_until_parked();
        cx.simulate_keystrokes("d d");
        assert_eq!(cx.buffer_text(), format!("{prefix} Heading\nbody"));
        cx.executor().run_until_parked();
        cx.update_editor(|editor, _, cx| {
            assert!(
                editor
                    .display_snapshot(cx)
                    .line_style_for_row(editor::display_map::DisplayRow(0))
                    .is_some(),
                "heading lost typography after dd"
            );
        });
    }
}

#[gpui::test]
async fn test_native_heading_delete_through_heading(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Remove\n## Keep\nbody").await;
    cx.simulate_keystrokes("2 d d");
    assert_eq!(cx.buffer_text(), "## Keep\nbody");
    cx.simulate_keystrokes("u");
    assert_eq!(cx.buffer_text(), "above\n## Remove\n## Keep\nbody");
}

#[gpui::test]
async fn test_native_heading_visual_delete_through_heading(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Remove\n## Keep\nbody").await;
    cx.simulate_keystrokes("shift-v j d");
    assert_eq!(cx.buffer_text(), "## Keep\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_before_eof_heading(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading").await;
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), "## Heading");
}

#[gpui::test]
async fn test_native_heading_forward_delete_newline(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇ\n## Heading\nbody").await;
    cx.simulate_keystrokes("i delete escape");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_and_redo(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("d d u ctrl-r");
    assert_eq!(cx.buffer_text(), "## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_delete_with_other_fold(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nfold start\nfold end\nafter").await;
    cx.update_editor(|editor, window, cx| {
        editor.fold_ranges(
            vec![language::Point::new(2, 0)..language::Point::new(3, 8)],
            false,
            window,
            cx,
        );
    });
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), "## Heading\nfold start\nfold end\nafter");
}

#[gpui::test]
async fn test_native_heading_delete_before_wrapped_heading(cx: &mut gpui::TestAppContext) {
    let heading = "heading words ".repeat(30);
    let mut cx = markdown_context(cx, &format!("ˇabove\n## {heading}\nbody")).await;
    cx.update_editor(|editor, _, cx| {
        editor.set_soft_wrap();
        cx.notify();
    });
    cx.executor().run_until_parked();
    cx.update_editor(|editor, _, cx| {
        let snapshot = editor.display_snapshot(cx);
        assert!(
            snapshot
                .point_to_display_point(language::Point::new(2, 0), editor::Bias::Left)
                .row()
                .0
                > 2,
            "the heading must actually soft-wrap"
        );
    });
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), format!("## {heading}\nbody"));
}

#[gpui::test]
async fn test_native_heading_change_through_heading(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Remove\n## Keep\nbody").await;
    cx.simulate_keystrokes("2 c c n e w escape");
    assert_eq!(cx.buffer_text(), "new\n## Keep\nbody");
}

#[gpui::test]
async fn test_native_heading_yank_through_heading(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("2 y y shift-p");
    assert_eq!(
        cx.buffer_text(),
        "above\n## Heading\nabove\n## Heading\nbody"
    );
}

#[gpui::test]
async fn test_native_heading_search_yank_and_paste(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    assert_eq!(cx.display_text(), "above\nHeading\nbody");
    cx.simulate_keystrokes("y y p");
    assert_eq!(cx.buffer_text(), "above\n## Heading\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_search_delete_and_paste(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    cx.simulate_keystrokes("d d shift-p");
    assert_eq!(cx.buffer_text(), "above\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_search_change_line(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    cx.simulate_keystrokes("c c n e w escape");
    assert_eq!(cx.buffer_text(), "above\nnew\nbody");
}

#[gpui::test]
async fn test_native_heading_search_change_with_other_fold(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nfold start\nfold end\nafter").await;
    cx.update_editor(|editor, window, cx| {
        editor.fold_ranges(
            vec![language::Point::new(2, 0)..language::Point::new(3, 8)],
            false,
            window,
            cx,
        );
    });
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    cx.simulate_keystrokes("c c n e w escape");
    assert_eq!(cx.buffer_text(), "above\nnew\nfold start\nfold end\nafter");
}

#[gpui::test]
async fn test_native_heading_unicode_and_inline_markup(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## **Κύριε** мир 🌿\nbody").await;
    assert_eq!(cx.display_text(), "above\nΚύριε мир 🌿\nbody");
    cx.simulate_keystrokes("d d");
    assert_eq!(cx.buffer_text(), "## **Κύριε** мир 🌿\nbody");
    cx.simulate_keystrokes("u");
    assert_eq!(cx.buffer_text(), "above\n## **Κύριε** мир 🌿\nbody");
}

#[gpui::test]
async fn test_native_heading_search_visual_yank(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    cx.simulate_keystrokes("shift-v y p");
    assert_eq!(cx.buffer_text(), "above\n## Heading\n## Heading\nbody");
}

#[gpui::test]
async fn test_native_heading_search_visual_change(cx: &mut gpui::TestAppContext) {
    let mut cx = markdown_context(cx, "ˇabove\n## Heading\nbody").await;
    cx.simulate_keystrokes("/ H e a d i n g enter");
    cx.executor().run_until_parked();
    cx.simulate_keystrokes("shift-v c n e w escape");
    assert_eq!(cx.buffer_text(), "above\nnew\nbody");
}
