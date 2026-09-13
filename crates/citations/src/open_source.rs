//! Following a citation to its source: with the cursor on a cite key, open
//! `refs/<key>.pdf` in a split beside the note. The PDF viewer is a
//! registered project item, so a plain open reaches it; nothing leaves the
//! editor.

use std::path::{Path, PathBuf};

use editor::Editor;
use editor::ToOffset as _;
use gpui::{App, Context, TaskExt as _, Window, actions};
use project::Project;
use settings::Settings as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

use crate::{CitationsSettings, citation_key_at};

actions!(
    citations,
    [
        /// Opens the PDF behind the citation key under the cursor in a split.
        OpenSource
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &OpenSource, window, cx| {
            open_source(workspace, window, cx);
        });
    })
    .detach();
}

struct OpenSourceToast;

/// The cite key under the cursor and the PDF the vault would hold for it,
/// whether or not that file exists yet.
pub fn source_for_cursor(
    editor: &Editor,
    project: &Project,
    cx: &App,
) -> Option<(String, PathBuf)> {
    let multibuffer = editor.buffer().read(cx);
    let buffer = multibuffer.as_singleton()?;
    let buffer = buffer.read(cx);
    // A singleton multibuffer's offsets are the buffer's own.
    let offset = editor
        .selections
        .newest_anchor()
        .head()
        .to_offset(&multibuffer.snapshot(cx))
        .0;
    let (_, key) = citation_key_at(buffer, offset)?;
    let file = buffer.file()?;
    let worktree = project.worktree_for_id(file.worktree_id(cx), cx)?;
    let library = worktree
        .read(cx)
        .abs_path()
        .join(&CitationsSettings::get_global(cx).library);
    let directory = library.parent().unwrap_or(Path::new(""));
    let pdf = directory.join(format!("{key}.pdf"));
    Some((key.to_string(), pdf))
}

fn open_source(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace.active_item_as::<Editor>(cx) else {
        return;
    };
    let project = workspace.project().clone();
    let Some((key, pdf)) = source_for_cursor(editor.read(cx), project.read(cx), cx) else {
        toast(
            workspace,
            "Put the cursor on a citation key to open its source.",
            cx,
        );
        return;
    };
    let fs = project.read(cx).fs().clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let exists = fs.is_file(&pdf).await;
        workspace.update_in(cx, |workspace, window, cx| {
            if exists {
                open_in_split(workspace, &pdf, window, cx);
            } else {
                let where_ = pdf
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                toast(workspace, &format!("No PDF for @{key} in {where_}/."), cx);
            }
        })
    })
    .detach_and_log_err(cx);
}

/// Reveals the file if a pane already shows it; otherwise splits the active
/// pane right and opens it there. Same shape as the typeset preview's
/// split, so the two features feel like one.
fn open_in_split(
    workspace: &mut Workspace,
    path: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().read(cx);
    let already_open = workspace.panes().iter().any(|pane| {
        pane.read(cx).items().any(|item| {
            item.project_path(cx)
                .and_then(|project_path| project.absolute_path(&project_path, cx))
                .is_some_and(|absolute| absolute == path)
        })
    });
    if already_open {
        return;
    }
    let Some(project_path) = project.find_project_path(path, cx) else {
        toast(workspace, "The source PDF is outside the project.", cx);
        return;
    };
    let pane = workspace.split_pane(
        workspace.active_pane().clone(),
        workspace::SplitDirection::Right,
        window,
        cx,
    );
    workspace
        .open_path(project_path, Some(pane.downgrade()), false, window, cx)
        .detach_and_log_err(cx);
}

fn toast(workspace: &mut Workspace, message: &str, cx: &mut Context<Workspace>) {
    workspace.show_toast(
        Toast::new(
            NotificationId::unique::<OpenSourceToast>(),
            message.to_string(),
        ),
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::FakeFs;
    use serde_json::json;
    use settings::SettingsStore;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    #[gpui::test]
    async fn opens_the_cited_pdf_in_a_split(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                "Note.md": "See [@smith2020] and [@nopdf2021].",
                "refs": {
                    "refs.bib": "@article{smith2020,\n  title = {A Study},\n}\n",
                    "smith2020.pdf": "%PDF-1.4 fake"
                }
            }),
        )
        .await;
        let project = Project::test(fs.clone(), ["/vault".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let note_path = project
            .read_with(cx, |project, cx| {
                project.find_project_path("/vault/Note.md", cx)
            })
            .expect("the note is in the vault");
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(note_path, None, true, window, cx)
            })
            .await
            .expect("failed to open the note")
            .downcast::<Editor>()
            .expect("a markdown file opens in an editor");

        let place_cursor = |offset: usize, cx: &mut gpui::VisualTestContext| {
            editor.update_in(cx, |editor, window, cx| {
                let point = text::Point::new(0, offset as u32);
                editor.change_selections(Default::default(), window, cx, |selections| {
                    selections.select_ranges([point..point]);
                });
            });
        };

        // Off any key: nothing to open.
        place_cursor(0, cx);
        let none = workspace.read_with(cx, |workspace, cx| {
            let editor = workspace.active_item_as::<Editor>(cx).unwrap();
            source_for_cursor(editor.read(cx), workspace.project().read(cx), cx)
        });
        assert_eq!(none, None);

        // On the key: the vault's PDF for it, whether or not it exists.
        place_cursor("See [@smi".len(), cx);
        let found = workspace.read_with(cx, |workspace, cx| {
            let editor = workspace.active_item_as::<Editor>(cx).unwrap();
            source_for_cursor(editor.read(cx), workspace.project().read(cx), cx)
        });
        assert_eq!(
            found,
            Some((
                "smith2020".to_string(),
                PathBuf::from("/vault/refs/smith2020.pdf")
            ))
        );

        // The PDF opens in a new split. (Without the PDF viewer registered
        // in this test, the item is a plain one with no project path, so
        // the split's item count is what can be asserted.)
        workspace.update_in(cx, |workspace, window, cx| {
            open_source(workspace, window, cx)
        });
        cx.run_until_parked();
        let panes = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .panes()
                    .iter()
                    .map(|pane| pane.read(cx).items_len())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            panes(cx),
            [1, 1],
            "the note's pane and a split holding the PDF"
        );

        // A key whose PDF is missing opens nothing.
        place_cursor("See [@smith2020] and [@nop".len(), cx);
        workspace.update_in(cx, |workspace, window, cx| {
            open_source(workspace, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(panes(cx), [1, 1]);
    }
}
