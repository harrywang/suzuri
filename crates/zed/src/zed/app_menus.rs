use collab_ui::collab_panel;
use gpui::{App, Menu, MenuItem, OsAction};
use project::DisableAiSettings;
use release_channel::ReleaseChannel;
use settings::{Settings, localization::text};
use terminal_view::terminal_panel;
use zed_actions::{Quit, assistant, debug_panel, dev, git_panel, project_panel};

pub fn app_menus(cx: &mut App) -> Vec<Menu> {
    let mut view_items = vec![
        MenuItem::action(
            text("menu.zoom_in", cx),
            zed_actions::IncreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            text("menu.zoom_out", cx),
            zed_actions::DecreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            text("menu.reset_zoom", cx),
            zed_actions::ResetBufferFontSize { persist: false },
        ),
        MenuItem::action(
            text("menu.reset_all_zoom", cx),
            zed_actions::ResetAllZoom { persist: false },
        ),
        MenuItem::separator(),
        MenuItem::action(text("menu.toggle_left_dock", cx), workspace::ToggleLeftDock),
        MenuItem::action(
            text("menu.toggle_right_dock", cx),
            workspace::ToggleRightDock,
        ),
        MenuItem::action(
            text("menu.toggle_bottom_dock", cx),
            workspace::ToggleBottomDock,
        ),
        MenuItem::action(text("menu.toggle_all_docks", cx), workspace::ToggleAllDocks),
        MenuItem::submenu(Menu {
            name: text("menu.editor_layout", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::action(text("menu.split_up", cx), workspace::SplitUp::default()),
                MenuItem::action(text("menu.split_down", cx), workspace::SplitDown::default()),
                MenuItem::action(text("menu.split_left", cx), workspace::SplitLeft::default()),
                MenuItem::action(
                    text("menu.split_right", cx),
                    workspace::SplitRight::default(),
                ),
            ],
        }),
        MenuItem::separator(),
        MenuItem::action(text("menu.project_panel", cx), project_panel::ToggleFocus),
        MenuItem::action(text("menu.outline_panel", cx), outline_panel::ToggleFocus),
        MenuItem::action(text("menu.collab_panel", cx), collab_panel::ToggleFocus),
        MenuItem::action(text("menu.terminal_panel", cx), terminal_panel::Toggle),
        MenuItem::action(text("menu.debugger_panel", cx), debug_panel::ToggleFocus),
    ];

    if !DisableAiSettings::get_global(cx).disable_ai {
        view_items.push(MenuItem::action(
            text("menu.agent_panel", cx),
            assistant::ToggleFocus,
        ));
    }

    view_items.extend([
        MenuItem::action(text("menu.git_panel", cx), git_panel::ToggleFocus),
        MenuItem::separator(),
        MenuItem::action(text("menu.diagnostics", cx), diagnostics::Deploy),
        MenuItem::separator(),
    ]);

    if ReleaseChannel::try_global(cx) == Some(ReleaseChannel::Dev) {
        view_items.push(MenuItem::action(
            text("menu.gpui_inspector", cx),
            dev::ToggleInspector,
        ));
        view_items.push(MenuItem::separator());
    }

    vec![
        Menu {
            // SUZURI: shown in the title-bar menu on Windows/Linux; macOS uses the bundle name.
            name: "Suzuri".into(),
            disabled: false,
            items: vec![
                MenuItem::action(text("menu.about", cx), zed_actions::About),
                MenuItem::action(text("menu.check_updates", cx), auto_update::Check),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new(text("menu.settings", cx)).items([
                    MenuItem::action(text("menu.open_settings", cx), zed_actions::OpenSettings),
                    MenuItem::action(text("menu.open_settings_file", cx), super::OpenSettingsFile),
                    MenuItem::action(
                        text("menu.open_project_settings", cx),
                        zed_actions::OpenProjectSettings,
                    ),
                    MenuItem::action(
                        text("menu.open_project_settings_file", cx),
                        super::OpenProjectSettingsFile,
                    ),
                    MenuItem::action(
                        text("menu.open_default_settings", cx),
                        super::OpenDefaultSettings,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(text("menu.open_keymap", cx), zed_actions::OpenKeymap),
                    MenuItem::action(
                        text("menu.open_keymap_file", cx),
                        zed_actions::OpenKeymapFile,
                    ),
                    MenuItem::action(
                        text("menu.open_default_key_bindings", cx),
                        zed_actions::OpenDefaultKeymap,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(
                        text("menu.select_theme", cx),
                        zed_actions::theme_selector::Toggle::default(),
                    ),
                    MenuItem::action(
                        text("menu.select_icon_theme", cx),
                        zed_actions::icon_theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu(text("menu.services", cx), gpui::SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.extensions", cx),
                    zed_actions::Extensions::default(),
                ),
                #[cfg(not(target_os = "windows"))]
                MenuItem::action(text("menu.install_cli", cx), install_cli::InstallCliBinary),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action(text("menu.hide", cx), super::Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action(text("menu.hide_others", cx), super::HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action(text("menu.show_all", cx), super::ShowAll),
                MenuItem::separator(),
                MenuItem::action(text("menu.quit", cx), Quit),
            ],
        },
        Menu {
            name: text("menu.file", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::action(text("menu.new", cx), workspace::NewFile),
                MenuItem::action(text("menu.new_window", cx), workspace::NewWindow),
                MenuItem::separator(),
                #[cfg(not(target_os = "macos"))]
                MenuItem::action(text("menu.open_file", cx), workspace::OpenFiles),
                MenuItem::action(
                    if cfg!(not(target_os = "macos")) {
                        text("menu.open_folder", cx)
                    } else {
                        text("menu.open", cx)
                    },
                    workspace::Open::default(),
                ),
                MenuItem::action(
                    text("menu.open_recent", cx),
                    zed_actions::OpenRecent::default(),
                ),
                MenuItem::action(
                    text("menu.open_remote", cx),
                    zed_actions::OpenRemote::default(),
                ),
                MenuItem::separator(),
                MenuItem::action(text("menu.add_folder", cx), workspace::AddFolderToProject),
                MenuItem::separator(),
                MenuItem::action(text("menu.save", cx), workspace::Save { save_intent: None }),
                MenuItem::action(text("menu.save_as", cx), workspace::SaveAs),
                MenuItem::action(
                    text("menu.save_all", cx),
                    workspace::SaveAll { save_intent: None },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.close_editor", cx),
                    workspace::CloseActiveItem {
                        save_intent: None,
                        close_pinned: true,
                    },
                ),
                MenuItem::action(text("menu.close_project", cx), workspace::CloseProject),
                MenuItem::action(text("menu.close_window", cx), workspace::CloseWindow),
            ],
        },
        Menu {
            name: text("menu.edit", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::os_action(text("menu.undo", cx), editor::actions::Undo, OsAction::Undo),
                MenuItem::os_action(text("menu.redo", cx), editor::actions::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action(text("menu.cut", cx), editor::actions::Cut, OsAction::Cut),
                MenuItem::os_action(text("menu.copy", cx), editor::actions::Copy, OsAction::Copy),
                MenuItem::action(text("menu.copy_trim", cx), editor::actions::CopyAndTrim),
                MenuItem::os_action(
                    text("menu.paste", cx),
                    editor::actions::Paste,
                    OsAction::Paste,
                ),
                MenuItem::separator(),
                MenuItem::action(text("menu.find", cx), search::buffer_search::Deploy::find()),
                MenuItem::action(
                    text("menu.find_project", cx),
                    workspace::DeploySearch::default(),
                ),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.toggle_comment", cx),
                    editor::actions::ToggleComments::default(),
                ),
            ],
        },
        Menu {
            name: text("menu.selection", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::os_action(
                    text("menu.select_all", cx),
                    editor::actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::action(
                    text("menu.expand_selection", cx),
                    editor::actions::SelectLargerSyntaxNode,
                ),
                MenuItem::action(
                    text("menu.shrink_selection", cx),
                    editor::actions::SelectSmallerSyntaxNode,
                ),
                MenuItem::action(
                    text("menu.next_sibling", cx),
                    editor::actions::SelectNextSyntaxNode,
                ),
                MenuItem::action(
                    text("menu.previous_sibling", cx),
                    editor::actions::SelectPreviousSyntaxNode,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.cursor_above", cx),
                    editor::actions::AddSelectionAbove {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    text("menu.cursor_below", cx),
                    editor::actions::AddSelectionBelow {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    text("menu.next_occurrence", cx),
                    editor::actions::SelectNext {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    text("menu.previous_occurrence", cx),
                    editor::actions::SelectPrevious {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    text("menu.all_occurrences", cx),
                    editor::actions::SelectAllMatches,
                ),
                MenuItem::separator(),
                MenuItem::action(text("menu.move_up", cx), editor::actions::MoveLineUp),
                MenuItem::action(text("menu.move_down", cx), editor::actions::MoveLineDown),
                MenuItem::action(
                    text("menu.duplicate", cx),
                    editor::actions::DuplicateLineDown,
                ),
            ],
        },
        Menu {
            name: text("menu.view", cx).into(),
            disabled: false,
            items: view_items,
        },
        Menu {
            name: text("menu.go", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::action(text("menu.back", cx), workspace::GoBack),
                MenuItem::action(text("menu.forward", cx), workspace::GoForward),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.command_palette", cx),
                    zed_actions::command_palette::Toggle,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.go_file", cx),
                    workspace::ToggleFileFinder::default(),
                ),
                // MenuItem::action(text("menu.go_symbol_project", cx), project_symbols::Toggle),
                MenuItem::action(
                    text("menu.go_symbol", cx),
                    zed_actions::outline::ToggleOutline,
                ),
                MenuItem::action(text("menu.go_line", cx), editor::actions::ToggleGoToLine),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.go_definition", cx),
                    editor::actions::GoToDefinition::default(),
                ),
                MenuItem::action(
                    text("menu.go_declaration", cx),
                    editor::actions::GoToDeclaration::default(),
                ),
                MenuItem::action(
                    text("menu.go_type", cx),
                    editor::actions::GoToTypeDefinition::default(),
                ),
                MenuItem::action(
                    text("menu.references", cx),
                    editor::actions::FindAllReferences::default(),
                ),
                MenuItem::action(
                    text("menu.incoming_calls", cx),
                    call_hierarchy::ShowIncomingCalls,
                ),
                MenuItem::action(
                    text("menu.outgoing_calls", cx),
                    call_hierarchy::ShowOutgoingCalls,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.next_problem", cx),
                    editor::actions::GoToDiagnostic::default(),
                ),
                MenuItem::action(
                    text("menu.previous_problem", cx),
                    editor::actions::GoToPreviousDiagnostic::default(),
                ),
            ],
        },
        Menu {
            name: text("menu.run", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    text("menu.spawn_task", cx),
                    zed_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::action(text("menu.start_debugger", cx), debugger_ui::Start),
                MenuItem::separator(),
                MenuItem::action(text("menu.edit_tasks", cx), zed_actions::OpenProjectTasks),
                MenuItem::action(
                    text("menu.edit_debug", cx),
                    zed_actions::OpenProjectDebugTasks,
                ),
                MenuItem::separator(),
                MenuItem::action(text("menu.continue", cx), debugger_ui::Continue),
                MenuItem::action(text("menu.step_over", cx), debugger_ui::StepOver),
                MenuItem::action(text("menu.step_into", cx), debugger_ui::StepInto),
                MenuItem::action(text("menu.step_out", cx), debugger_ui::StepOut),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.toggle_breakpoint", cx),
                    editor::actions::ToggleBreakpoint,
                ),
                MenuItem::action(
                    text("menu.edit_breakpoint", cx),
                    editor::actions::EditLogBreakpoint,
                ),
                MenuItem::action(
                    text("menu.clear_breakpoints", cx),
                    debugger_ui::ClearAllBreakpoints,
                ),
            ],
        },
        Menu {
            // AppKit identifies the native Window menu by this name.
            name: if cfg!(target_os = "macos") {
                "Window"
            } else {
                text("menu.window", cx)
            }
            .into(),
            disabled: false,
            items: vec![
                MenuItem::action(text("menu.minimize", cx), super::Minimize),
                MenuItem::action(text("menu.zoom", cx), super::Zoom),
                MenuItem::separator(),
            ],
        },
        Menu {
            name: text("menu.help", cx).into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    text("menu.release_notes", cx),
                    auto_update_ui::ViewReleaseNotesLocally,
                ),
                MenuItem::action(text("menu.telemetry", cx), zed_actions::OpenTelemetryLog),
                MenuItem::action(text("menu.licenses", cx), zed_actions::OpenLicenses),
                MenuItem::action(text("menu.welcome", cx), onboarding::ShowWelcome),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.report_bug", cx),
                    zed_actions::feedback::FileBugReport,
                ),
                MenuItem::action(
                    text("menu.request_feature", cx),
                    zed_actions::feedback::RequestFeature,
                ),
                MenuItem::action(text("menu.email", cx), zed_actions::feedback::EmailZed),
                MenuItem::separator(),
                MenuItem::action(
                    text("menu.documentation", cx),
                    super::OpenBrowser {
                        url: "https://zed.dev/docs".into(),
                    },
                ),
                MenuItem::action(text("menu.repository", cx), feedback::OpenZedRepo),
                MenuItem::action(
                    text("menu.twitter", cx),
                    super::OpenBrowser {
                        url: "https://twitter.com/zeddotdev".into(),
                    },
                ),
                MenuItem::action(
                    text("menu.join_team", cx),
                    super::OpenBrowser {
                        url: "https://zed.dev/jobs".into(),
                    },
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use gpui::UpdateGlobal;

    use super::*;

    fn action_names(menus: &[Menu]) -> Vec<String> {
        fn collect(items: &[MenuItem], names: &mut Vec<String>) {
            for item in items {
                match item {
                    MenuItem::Action { action, .. } => names.push(action.name().to_string()),
                    MenuItem::Submenu(menu) => collect(&menu.items, names),
                    MenuItem::Separator | MenuItem::SystemMenu(_) => {}
                }
            }
        }
        let mut names = Vec::new();
        for menu in menus {
            collect(&menu.items, &mut names);
        }
        names
    }

    #[gpui::test]
    fn translated_menus_preserve_actions(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            settings::localization::init(cx);
            let english = app_menus(cx);
            assert!(english.iter().any(|menu| menu.name == "File"));
            settings::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"ui_language":"zh-CN"}"#, cx)
                    .expect("valid language setting");
            });
            settings::localization::init(cx);
            let chinese = app_menus(cx);
            assert_eq!(english.len(), chinese.len());
            assert!(chinese.iter().any(|menu| menu.name == "文件"));
            assert_eq!(action_names(&english), action_names(&chinese));
        });
    }
}
