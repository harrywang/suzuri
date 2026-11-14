use super::*;
use fs::{FakeFs, Fs as _};
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use serde_json::json;
use std::path::Path;
use util::{path, rel_path::rel_path};
use workspace::{AppState, MultiWorkspace};

struct Fixture<'a> {
    fs: Arc<FakeFs>,
    workspace: Entity<Workspace>,
    cx: &'a mut VisualTestContext,
}

/// A vault where Hub links to Alpha and Beta, Backlinker links to Hub, and
/// Alpha links on to Gamma.
async fn open_vault(cx: &mut TestAppContext) -> Fixture<'_> {
    cx.update(|cx| {
        let _state = AppState::test(cx);
        editor::init(cx);
        init(cx);
    });
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/vault"),
        json!({
            "Hub.md": "Builds on [[Alpha]] and [the second](notes/Beta.md).\n",
            "Alpha.md": "Leads to [[Gamma|a third]].\n",
            "Gamma.md": "The end of the chain.\n",
            "Backlinker.md": "As [[Hub#Intro]] says.\n",
            "Alone.md": "Nothing links here, and `[[Hub]]` in code is not a link.\n",
            "notes": { "Beta.md": "A note in a folder.\n" },
            "readme.txt": "Not a note: [[Hub]]\n",
        }),
    )
    .await;
    let project = Project::test(fs.clone(), [path!("/vault").as_ref()], cx).await;
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
    let workspace =
        multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
    Fixture { fs, workspace, cx }
}

async fn open_file(path: &str, workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
    let worktree_id = workspace.read_with(cx, |workspace, cx| {
        workspace
            .project()
            .read(cx)
            .worktrees(cx)
            .next()
            .expect("a worktree")
            .read(cx)
            .id()
    });
    let pane = workspace.read_with(cx, |workspace, cx| {
        // Notes open where notes already are, never in the graph's pane.
        workspace
            .panes()
            .iter()
            .find(|pane| {
                pane.read(cx)
                    .items()
                    .all(|item| item.downcast::<GraphView>().is_none())
            })
            .map(|pane| pane.downgrade())
    });
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path((worktree_id, rel_path(path)), pane, true, window, cx)
        })
        .await
        .expect("the file opens");
    redraw(cx);
}

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

async fn open_graph(
    note: &str,
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Entity<GraphView> {
    open_file(note, workspace, cx).await;
    cx.dispatch_action(OpenLocalGraph);
    redraw(cx);
    graph_view(workspace, cx).expect("the graph opens")
}

fn graph_view(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Option<Entity<GraphView>> {
    workspace.read_with(cx, |workspace, cx| {
        workspace.items_of_type::<GraphView>(cx).next()
    })
}

/// The shown notes by name with their distance from the center, sorted.
fn shown(view: &Entity<GraphView>, cx: &mut VisualTestContext) -> Vec<(String, usize)> {
    view.read_with(cx, |view, _| {
        let mut shown: Vec<(String, usize)> = view
            .shown
            .notes
            .iter()
            .map(|placed| (view.note_name(placed.note).to_string(), placed.hops))
            .collect();
        shown.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        shown
    })
}

fn names(shown: &[(&str, usize)]) -> Vec<(String, usize)> {
    shown
        .iter()
        .map(|(name, hops)| (name.to_string(), *hops))
        .collect()
}

fn active_path(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<String> {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_item(cx)?
            .project_path(cx)
            .map(|path| path.path.as_unix_str().to_string())
    })
}

fn node_center(name: &str, cx: &mut VisualTestContext) -> Point<Pixels> {
    // `debug_bounds` takes a `'static` selector.
    let selector: &'static str = format!("note-graph-{name}").leak();
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("note {name} is drawn"));
    point(bounds.center().x, bounds.origin.y + px(4.0))
}

fn click(position: Point<Pixels>, cx: &mut VisualTestContext) {
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());
    redraw(cx);
}

#[gpui::test]
async fn the_graph_shows_links_in_both_directions(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Hub.md", &workspace, cx).await;

    assert_eq!(
        shown(&view, cx),
        names(&[("Hub", 0), ("Alpha", 1), ("Backlinker", 1), ("Beta", 1)]),
        "a wikilink, a markdown link, and a link from another note; not the \
         note two links away, the unlinked note, or the text file"
    );
    view.read_with(cx, |view, _| {
        assert!(view.fitted);
        assert_eq!(view.shown.links.len(), 3);
    });
    for name in ["Hub", "Alpha", "Backlinker", "Beta"] {
        node_center(name, cx);
    }
}

#[gpui::test]
async fn the_graph_opens_beside_the_note_and_leaves_the_cursor_in_it(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Hub.md", &workspace, cx).await;

    assert_eq!(active_path(&workspace, cx).as_deref(), Some("Hub.md"));
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.panes().len(), 2);
        let graph_pane = workspace.pane_for(&view).expect("the graph is in a pane");
        assert_ne!(&graph_pane, workspace.active_pane());
        assert_eq!(graph_pane.read(cx).items_len(), 1);
    });

    // Asking again while one is open does not open a second.
    cx.dispatch_action(OpenLocalGraph);
    redraw(cx);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.items_of_type::<GraphView>(cx).count(), 1);
        assert_eq!(workspace.panes().len(), 2);
    });
}

#[gpui::test]
async fn the_graph_follows_the_active_note(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Hub.md", &workspace, cx).await;

    open_file("Alpha.md", &workspace, cx).await;
    assert_eq!(
        shown(&view, cx),
        names(&[("Alpha", 0), ("Gamma", 1), ("Hub", 1)])
    );

    // A file that is not a note, and the graph itself becoming active,
    // leave the graph where it was instead of emptying it.
    open_file("readme.txt", &workspace, cx).await;
    assert_eq!(shown(&view, cx).len(), 3);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&view, true, true, window, cx);
    });
    redraw(cx);
    assert_eq!(
        shown(&view, cx),
        names(&[("Alpha", 0), ("Gamma", 1), ("Hub", 1)])
    );

    open_file("Alone.md", &workspace, cx).await;
    assert_eq!(shown(&view, cx), names(&[("Alone", 0)]));
}

#[gpui::test]
async fn clicking_a_note_opens_it_where_the_notes_are(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Hub.md", &workspace, cx).await;
    let note_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());

    click(node_center("Beta", cx), cx);
    assert_eq!(
        active_path(&workspace, cx).as_deref(),
        Some("notes/Beta.md")
    );
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active_pane(), &note_pane);
        let graph_pane = workspace.pane_for(&view).expect("the graph is in a pane");
        assert_eq!(
            graph_pane.read(cx).items_len(),
            1,
            "the note did not open over the graph"
        );
    });
    assert_eq!(shown(&view, cx), names(&[("Beta", 0), ("Hub", 1)]));

    // With the notes' pane closed, a click still does not cover the graph.
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&view, true, true, window, cx);
        note_pane.update(cx, |pane, cx| {
            pane.close_all_items(&Default::default(), window, cx)
                .detach_and_log_err(cx);
        });
    });
    redraw(cx);
    click(node_center("Hub", cx), cx);
    assert_eq!(active_path(&workspace, cx).as_deref(), Some("Hub.md"));
    assert!(graph_view(&workspace, cx).is_some());
}

#[gpui::test]
async fn dragging_pans_instead_of_opening(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Hub.md", &workspace, cx).await;
    let offset_before = view.read_with(cx, |view, _| view.offset);

    let start = node_center("Alpha", cx);
    let end = point(start.x + px(60.0), start.y + px(25.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);

    assert_eq!(
        shown(&view, cx)[0],
        ("Hub".to_string(), 0),
        "the note under the press did not open"
    );
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.offset,
            point(offset_before.x + 60.0, offset_before.y + 25.0)
        );
    });
}

#[gpui::test]
async fn depth_reaches_further_links(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let view = open_graph("Backlinker.md", &workspace, cx).await;
    assert_eq!(shown(&view, cx), names(&[("Backlinker", 0), ("Hub", 1)]));

    view.update(cx, |view, cx| view.set_hops(2, cx));
    redraw(cx);
    assert_eq!(
        shown(&view, cx),
        names(&[("Backlinker", 0), ("Hub", 1), ("Alpha", 2), ("Beta", 2)])
    );

    view.update(cx, |view, cx| view.set_hops(3, cx));
    redraw(cx);
    assert_eq!(shown(&view, cx).len(), 5);
    assert!(shown(&view, cx).contains(&("Gamma".to_string(), 3)));
}

#[gpui::test]
async fn a_link_shows_up_as_it_is_typed_and_when_a_file_changes(cx: &mut TestAppContext) {
    let Fixture { fs, workspace, cx } = open_vault(cx).await;
    let view = open_graph("Alone.md", &workspace, cx).await;
    assert_eq!(shown(&view, cx), names(&[("Alone", 0)]));

    let editor = workspace
        .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
        .expect("the note is in an editor");
    editor.update_in(cx, |editor, window, cx| {
        editor.move_to_end(&Default::default(), window, cx);
        editor.insert("See [[Gamma]].", window, cx);
    });
    cx.executor().advance_clock(TYPING_DEBOUNCE * 2);
    redraw(cx);
    assert_eq!(
        shown(&view, cx),
        names(&[("Alone", 0), ("Gamma", 1)]),
        "the unsaved link is read from the buffer"
    );

    // Another program, or an agent, writes a note that links here.
    fs.insert_file(
        Path::new(path!("/vault/New.md")),
        b"About [[Alone]].\n".to_vec(),
    )
    .await;
    cx.run_until_parked();
    cx.executor().advance_clock(INDEX_DEBOUNCE * 2);
    redraw(cx);
    assert_eq!(
        shown(&view, cx),
        names(&[("Alone", 0), ("Gamma", 1), ("New", 1)])
    );

    fs.remove_file(Path::new(path!("/vault/New.md")), Default::default())
        .await
        .expect("the note is deleted");
    cx.run_until_parked();
    cx.executor().advance_clock(INDEX_DEBOUNCE * 2);
    redraw(cx);
    assert_eq!(shown(&view, cx), names(&[("Alone", 0), ("Gamma", 1)]));
}

#[gpui::test]
async fn the_command_is_available_only_on_a_note(cx: &mut TestAppContext) {
    let Fixture { workspace, cx, .. } = open_vault(cx).await;
    let available = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.is_action_available(&OpenLocalGraph, cx)
        })
    };
    assert!(!available(cx), "nothing is open");

    open_file("readme.txt", &workspace, cx).await;
    assert!(!available(cx), "a text file has no note links");

    open_file("Hub.md", &workspace, cx).await;
    assert!(available(cx));
}
