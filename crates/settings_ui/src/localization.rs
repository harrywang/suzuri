use gpui::{App, SharedString};
use settings::localization::text;

// Keep source labels as stable navigation/search identifiers; translate only at display boundaries.
pub(crate) fn localize(source: &str, cx: &App) -> SharedString {
    let key = match source {
        "General" => "settings.label.general",
        "Appearance" => "settings.label.appearance",
        "Keymap" => "settings.label.keymap",
        "Editor" => "settings.label.editor",
        "Languages & Tools" => "settings.label.languages_tools",
        "Search & Files" => "settings.label.search_files",
        "Window & Layout" => "settings.label.window_layout",
        "Panels" => "settings.label.panels",
        "Debugger" => "settings.label.debugger",
        "Terminal" => "settings.label.terminal",
        "Version Control" => "settings.label.version_control",
        "Collaboration" => "settings.label.collaboration",
        "AI" => "settings.label.ai",
        "Network" => "settings.label.network",
        "Developer" => "settings.label.developer",
        "Feature Flags" => "settings.label.feature_flags",
        "Instrumentation" => "settings.label.instrumentation",
        "General Settings" => "settings.label.general_settings",
        "Security" => "settings.label.security",
        "Workspace Restoration" => "settings.label.workspace_restoration",
        "Scoped Settings" => "settings.label.scoped_settings",
        "Privacy" => "settings.label.privacy",
        "Auto Update" => "settings.label.auto_update",
        "Theme" => "settings.label.theme",
        "Buffer Font" => "settings.label.buffer_font",
        "UI Font" => "settings.label.ui_font",
        "Agent Panel Font" => "settings.label.agent_panel_font",
        "Markdown Preview Font" => "settings.label.markdown_preview_font",
        "Text Rendering" => "settings.label.text_rendering",
        "Cursor" => "settings.label.cursor",
        "Highlighting" => "settings.label.highlighting",
        "Guides" => "settings.label.guides",
        "Keybindings" => "settings.label.keybindings",
        "Base Keymap" => "settings.label.base_keymap",
        "Modal Editing" => "settings.label.modal_editing",
        "Auto Save" => "settings.label.auto_save",
        "Which-key Menu" => "settings.label.which_key_menu",
        "Multibuffer" => "settings.label.multibuffer",
        "Scrolling" => "settings.label.scrolling",
        "Signature Help" => "settings.label.signature_help",
        "Hover Popover" => "settings.label.hover_popover",
        "Drag And Drop Selection" => "settings.label.drag_and_drop_selection",
        "Gutter" => "settings.label.gutter",
        "Scrollbar" => "settings.label.scrollbar",
        "Minimap" => "settings.label.minimap",
        "Toolbar" => "settings.label.toolbar",
        "Vim" => "settings.label.vim",
        "File Types" => "settings.label.file_types",
        "Diagnostics" => "settings.label.diagnostics",
        "Inline Diagnostics" => "settings.label.inline_diagnostics",
        "LSP Pull Diagnostics" => "settings.label.lsp_pull_diagnostics",
        "LSP Highlights" => "settings.label.lsp_highlights",
        "Languages" => "settings.label.languages",
        "Search" => "settings.label.search",
        "File Finder" => "settings.label.file_finder",
        "File Scan" => "settings.label.file_scan",
        "Status Bar" => "settings.label.status_bar",
        "Title Bar" => "settings.label.title_bar",
        "Tab Bar" => "settings.label.tab_bar",
        "Tab Settings" => "settings.label.tab_settings",
        "Preview Tabs" => "settings.label.preview_tabs",
        "Layout" => "settings.label.layout",
        "Window" => "settings.label.window",
        "Pane Modifiers" => "settings.label.pane_modifiers",
        "Pane Split Direction" => "settings.label.pane_split_direction",
        "Project Panel" => "settings.label.project_panel",
        "Terminal Panel" => "settings.label.terminal_panel",
        "Outline Panel" => "settings.label.outline_panel",
        "Git Panel" => "settings.label.git_panel",
        "Debugger Panel" => "settings.label.debugger_panel",
        "Collaboration Panel" => "settings.label.collaboration_panel",
        "Agent Panel" => "settings.label.agent_panel",
        "Environment" => "settings.label.environment",
        "Font" => "settings.label.font",
        "Display Settings" => "settings.label.display_settings",
        "Behavior Settings" => "settings.label.behavior_settings",
        "Layout Settings" => "settings.label.layout_settings",
        "Advanced Settings" => "settings.label.advanced_settings",
        "Git Integration" => "settings.label.git_integration",
        "Git Gutter" => "settings.label.git_gutter",
        "Inline Git Blame" => "settings.label.inline_git_blame",
        "Git Blame View" => "settings.label.git_blame_view",
        "Branch Picker" => "settings.label.branch_picker",
        "Git Hunks" => "settings.label.git_hunks",
        "File Diff" => "settings.label.file_diff",
        "Calls" => "settings.label.calls",
        "Agent Configuration" => "settings.label.agent_configuration",
        "Indentation" => "settings.label.indentation",
        "Wrapping" => "settings.label.wrapping",
        "Indent Guides" => "settings.label.indent_guides",
        "Formatting" => "settings.label.formatting",
        "Autoclose" => "settings.label.autoclose",
        "Whitespace" => "settings.label.whitespace",
        "Completions" => "settings.label.completions",
        "Inlay Hints" => "settings.label.inlay_hints",
        "Tasks" => "settings.label.tasks",
        "Miscellaneous" => "settings.label.miscellaneous",
        "LSP" => "settings.label.lsp",
        "LSP Completions" => "settings.label.lsp_completions",
        "Debuggers" => "settings.label.debuggers",
        "Prettier" => "settings.label.prettier",
        "Edit Predictions" => "settings.label.edit_predictions",
        "Performance Profiler" => "settings.label.performance_profiler",
        "Language / 界面语言" => "settings.label.language",
        "Accessible Mode" => "settings.label.accessible_mode",
        "When Closing With No Tabs" => "settings.label.when_closing_with_no_tabs",
        "On New Window" => "settings.label.on_new_window",
        "On Last Window Closed" => "settings.label.on_last_window_closed",
        "Use System Path Prompts" => "settings.label.use_system_path_prompts",
        "Use System Prompts" => "settings.label.use_system_prompts",
        "Redact Private Values" => "settings.label.redact_private_values",
        "Private Files" => "settings.label.private_files",
        "CLI Default Open Behavior" => "settings.label.cli_default_open_behavior",
        "Reveal If Open" => "settings.label.reveal_if_open",
        "Default Open Behavior" => "settings.label.default_open_behavior",
        "Trust All Projects By Default" => "settings.label.trust_all_projects_by_default",
        "Restore Unsaved Buffers" => "settings.label.restore_unsaved_buffers",
        "Restore On Startup" => "settings.label.restore_on_startup",
        "Preview Channel" => "settings.label.preview_channel",
        "Settings Profiles" => "settings.label.settings_profiles",
        "Telemetry Diagnostics" => "settings.label.telemetry_diagnostics",
        "Telemetry Metrics" => "settings.label.telemetry_metrics",
        "Anthropic Data Retention" => "settings.label.anthropic_data_retention",
        "Theme Mode" => "settings.label.theme_mode",
        "Theme Name" => "settings.label.theme_name",
        "Mode" => "settings.label.mode",
        "Light Theme" => "settings.label.light_theme",
        "Dark Theme" => "settings.label.dark_theme",
        "Icon Theme" => "settings.label.icon_theme",
        "Icon Theme Name" => "settings.label.icon_theme_name",
        "Light Icon Theme" => "settings.label.light_icon_theme",
        "Dark Icon Theme" => "settings.label.dark_icon_theme",
        "Font Family" => "settings.label.font_family",
        "Font Size" => "settings.label.font_size",
        "Font Weight" => "settings.label.font_weight",
        "Line Height" => "settings.label.line_height",
        "Custom Line Height" => "settings.label.custom_line_height",
        "Font Features" => "settings.label.font_features",
        "Font Fallbacks" => "settings.label.font_fallbacks",
        "UI Font Family" => "settings.label.ui_font_family",
        "UI Font Size" => "settings.label.ui_font_size",
        "Buffer Font Family" => "settings.label.buffer_font_family",
        "Buffer Font Size" => "settings.label.buffer_font_size",
        "Code Font Family" => "settings.label.code_font_family",
        "Text Rendering Mode" => "settings.label.text_rendering_mode",
        "Multi Cursor Modifier" => "settings.label.multi_cursor_modifier",
        "Cursor Blink" => "settings.label.cursor_blink",
        "Cursor Animation" => "settings.label.cursor_animation",
        "Cursor Shape" => "settings.label.cursor_shape",
        "Hide Mouse" => "settings.label.hide_mouse",
        "Reduce Motion" => "settings.label.reduce_motion",
        "Unnecessary Code Fade" => "settings.label.unnecessary_code_fade",
        "Current Line Highlight" => "settings.label.current_line_highlight",
        "Selection Highlight" => "settings.label.selection_highlight",
        "Rounded Selection" => "settings.label.rounded_selection",
        "Minimum Contrast For Highlights" => "settings.label.minimum_contrast_for_highlights",
        "Show Wrap Guides" => "settings.label.show_wrap_guides",
        "Wrap Guides" => "settings.label.wrap_guides",
        "Vim Mode" => "settings.label.vim_mode",
        "Helix Mode" => "settings.label.helix_mode",
        "Tab Size" => "settings.label.tab_size",
        "Hard Tabs" => "settings.label.hard_tabs",
        "Soft Wrap" => "settings.label.soft_wrap",
        "Preferred Line Length" => "settings.label.preferred_line_length",
        "Format On Save" => "settings.label.format_on_save",
        "Remove Trailing Whitespace On Save" => "settings.label.remove_trailing_whitespace_on_save",
        "Ensure Final Newline On Save" => "settings.label.ensure_final_newline_on_save",
        "Search settings…" => "settings.label.search_settings_placeholder",
        "Search Settings" => "settings.label.search_settings",
        "Settings Navigation" => "settings.label.settings_navigation",
        "Settings Content" => "settings.label.settings_content",
        "Settings File" => "settings.label.settings_file",
        "Edit in settings.json" => "settings.label.edit_in_settings_json",
        "Fix in settings.json" => "settings.label.fix_in_settings_json",
        "Configure" => "settings.label.configure",
        "Reset to Default" => "settings.label.reset_to_default",
        "Copy Link" => "settings.label.copy_link",
        "View Other Projects" => "settings.label.view_other_projects",
        "Change Scope" => "settings.label.change_scope",
        "Scope" => "settings.label.scope",
        "Clear" => "settings.label.clear",
        "No Results" => "settings.label.no_results",
        "Create Skill" => "settings.label.create_skill",
        "Restricted Mode" => "settings.label.restricted_mode",
        "Manage Trust" => "settings.label.manage_trust",
        "User" => "settings.label.user",
        "Focus Navbar" => "settings.label.focus_navbar",
        "Focus Search" => "settings.label.focus_search",
        "Focus Content" => "settings.label.focus_content",
        "Zed — Settings" => "settings.label.zed_settings",
        "On" => "settings.label.on",
        "Off" => "settings.label.off",
        "Always" => "settings.label.always",
        "Never" => "settings.label.never",
        "None" => "settings.label.none",
        "All" => "settings.label.all",
        "Default" => "settings.label.default",
        "Platform Default" => "settings.label.platform_default",
        "System" => "settings.label.system",
        "Light" => "settings.label.light",
        "Dark" => "settings.label.dark",
        "Static" => "settings.label.static",
        "Dynamic" => "settings.label.dynamic",
        "Existing Window" => "settings.label.existing_window",
        "New Window" => "settings.label.new_window",
        "New Tab" => "settings.label.new_tab",
        "Last Workspace" => "settings.label.last_workspace",
        "Last Session" => "settings.label.last_session",
        "Launchpad" => "settings.label.launchpad",
        "Welcome Page" => "settings.label.welcome_page",
        "Empty" => "settings.label.empty",
        "Quit App" => "settings.label.quit_app",
        "Close Window" => "settings.label.close_window",
        "Close" => "settings.label.close",
        "Enabled" => "settings.label.enabled",
        "Disabled" => "settings.label.disabled",
        "Compact" => "settings.label.compact",
        "Comfortable" => "settings.label.comfortable",
        "Standard" => "settings.label.standard",
        "Editor Width" => "settings.label.editor_width",
        "Bounded" => "settings.label.bounded",
        "Left" => "settings.label.left",
        "Right" => "settings.label.right",
        "Bottom" => "settings.label.bottom",
        "Top" => "settings.label.top",
        "Selection" => "settings.label.selection",
        "Line" => "settings.label.line",
        "Block" => "settings.label.block",
        "Bar" => "settings.label.bar",
        "Underline" => "settings.label.underline",
        "Hollow" => "settings.label.hollow",
        "Typing" => "settings.label.typing",
        "Typing And Movement" => "settings.label.typing_and_movement",
        "Inherit" => "settings.label.inherit",
        "After Delay" => "settings.label.after_delay",
        "On Focus Change" => "settings.label.on_focus_change",
        "On Window Change" => "settings.label.on_window_change",
        "Save" => "settings.label.save",
        "Cancel" => "settings.label.cancel",
        "Confirm" => "settings.label.confirm",
        "Add" => "settings.label.add",
        "Remove" => "settings.label.remove",
        "Reset" => "settings.label.reset",
        "Select Font…" => "settings.label.select_font_placeholder",
        "Select Theme…" => "settings.label.select_theme_placeholder",
        "Select Icon Theme…" => "settings.label.select_icon_theme_placeholder",
        "Edit Keybindings" => "settings.label.edit_keybindings",
        "Auto Save Mode" => "settings.label.auto_save_mode",
        "Delay (milliseconds)" => "settings.label.delay_milliseconds",
        "Show Which-key Menu" => "settings.label.show_which_key_menu",
        "Menu Delay" => "settings.label.menu_delay",
        "Enter to Confirm" => "settings.label.enter_to_confirm",
        "Optimize Zed's interface for assistive technology such as screen readers. When enabled, otherwise-collapsed controls stay expanded and keyboard-reachable." => {
            "settings.description.optimize_zed_s_interface_for_assistive_technology_such_as_screen_readers_when_enabled_otherwise"
        }
        "What to do when using the 'close active item' action with no tabs." => {
            "settings.description.what_to_do_when_using_the_close_active_item_action_with_no_tabs"
        }
        "What to show when opening a new window." => {
            "settings.description.what_to_show_when_opening_a_new_window"
        }
        "What to do when the last window is closed." => {
            "settings.description.what_to_do_when_the_last_window_is_closed"
        }
        "Use native OS dialogs for 'Open' and 'Save As'." => {
            "settings.description.use_native_os_dialogs_for_open_and_save_as"
        }
        "Use native OS dialogs for confirmations." => {
            "settings.description.use_native_os_dialogs_for_confirmations"
        }
        "Hide the values of variables in private files." => {
            "settings.description.hide_the_values_of_variables_in_private_files"
        }
        "Globs to match against file paths to determine if a file is private." => {
            "settings.description.globs_to_match_against_file_paths_to_determine_if_a_file_is_private"
        }
        "How `zed <path>` opens directories when no flag is specified." => {
            "settings.description.how_zed_path_opens_directories_when_no_flag_is_specified"
        }
        "when enabled, zed will prefer already-open buffers." => {
            "settings.description.when_enabled_zed_will_prefer_already_open_buffers"
        }
        "How projects open from the UI by default." => {
            "settings.description.how_projects_open_from_the_ui_by_default"
        }
        "When opening Zed, avoid Restricted Mode by auto-trusting all projects, enabling use of all features without having to give permission to each new project." => {
            "settings.description.when_opening_zed_avoid_restricted_mode_by_auto_trusting_all_projects_enabling_use_of"
        }
        "Whether or not to restore unsaved buffers on restart." => {
            "settings.description.whether_or_not_to_restore_unsaved_buffers_on_restart"
        }
        "What to restore from the previous session when opening Zed." => {
            "settings.description.what_to_restore_from_the_previous_session_when_opening_zed"
        }
        "Which settings should be activated only in Preview build of Zed." => {
            "settings.description.which_settings_should_be_activated_only_in_preview_build_of_zed"
        }
        "Any number of settings profiles that are temporarily applied on top of your existing user settings." => {
            "settings.description.any_number_of_settings_profiles_that_are_temporarily_applied_on_top_of_your_existing"
        }
        "Send debug information like crash reports." => {
            "settings.description.send_debug_information_like_crash_reports"
        }
        "Send anonymized usage data like what languages you're using Zed with." => {
            "settings.description.send_anonymized_usage_data_like_what_languages_you_re_using_zed_with"
        }
        "Allow sending requests to Anthropic models that cannot be offered with Zero Data Retention." => {
            "settings.description.allow_sending_requests_to_anthropic_models_that_cannot_be_offered_with_zero_data_retention"
        }
        "Whether or not to automatically check for updates." => {
            "settings.description.whether_or_not_to_automatically_check_for_updates"
        }
        "Choose a static, fixed theme or dynamically select themes based on appearance and light/dark modes." => {
            "settings.description.choose_a_static_fixed_theme_or_dynamically_select_themes_based_on_appearance_and_light"
        }
        "The name of your selected theme." => {
            "settings.description.the_name_of_your_selected_theme"
        }
        "Choose whether to use the selected light or dark theme or to follow your OS appearance configuration." => {
            "settings.description.choose_whether_to_use_the_selected_light_or_dark_theme_or_to_follow_your"
        }
        "The theme to use when mode is set to light, or when mode is set to system and it is in light mode." => {
            "settings.description.the_theme_to_use_when_mode_is_set_to_light_or_when_mode_is"
        }
        "The theme to use when mode is set to dark, or when mode is set to system and it is in dark mode." => {
            "settings.description.the_theme_to_use_when_mode_is_set_to_dark_or_when_mode_is"
        }
        "The custom set of icons Zed will associate with files and directories." => {
            "settings.description.the_custom_set_of_icons_zed_will_associate_with_files_and_directories"
        }
        "The name of your selected icon theme." => {
            "settings.description.the_name_of_your_selected_icon_theme"
        }
        "Choose whether to use the selected light or dark icon theme or to follow your OS appearance configuration." => {
            "settings.description.choose_whether_to_use_the_selected_light_or_dark_icon_theme_or_to_follow"
        }
        "The icon theme to use when mode is set to light, or when mode is set to system and it is in light mode." => {
            "settings.description.the_icon_theme_to_use_when_mode_is_set_to_light_or_when_mode"
        }
        "The icon theme to use when mode is set to dark, or when mode is set to system and it is in dark mode." => {
            "settings.description.the_icon_theme_to_use_when_mode_is_set_to_dark_or_when_mode"
        }
        "Font family for editor text." => "settings.description.font_family_for_editor_text",
        "Font size for editor text." => "settings.description.font_size_for_editor_text",
        "Font weight for editor text (100-900)." => {
            "settings.description.font_weight_for_editor_text_100_900"
        }
        "Line height for editor text." => "settings.description.line_height_for_editor_text",
        "Custom line height value (must be at least 1.0)." => {
            "settings.description.custom_line_height_value_must_be_at_least_1_0"
        }
        "The OpenType features to enable for rendering in text buffers." => {
            "settings.description.the_opentype_features_to_enable_for_rendering_in_text_buffers"
        }
        "The font fallbacks to use for rendering in text buffers." => {
            "settings.description.the_font_fallbacks_to_use_for_rendering_in_text_buffers"
        }
        "Font family for UI elements." => "settings.description.font_family_for_ui_elements",
        "Font size for UI elements." => "settings.description.font_size_for_ui_elements",
        "Font weight for UI elements (100-900)." => {
            "settings.description.font_weight_for_ui_elements_100_900"
        }
        "The OpenType features to enable for rendering in UI elements." => {
            "settings.description.the_opentype_features_to_enable_for_rendering_in_ui_elements"
        }
        "The font fallbacks to use for rendering in the UI." => {
            "settings.description.the_font_fallbacks_to_use_for_rendering_in_the_ui"
        }
        "Font family for agent response text in the agent panel. Falls back to the regular UI font family." => {
            "settings.description.font_family_for_agent_response_text_in_the_agent_panel_falls_back_to_the"
        }
        "Font size for agent response text in the agent panel. Falls back to the regular UI font size." => {
            "settings.description.font_size_for_agent_response_text_in_the_agent_panel_falls_back_to_the"
        }
        "Font family for user messages in the agent panel. Falls back to the regular buffer font family." => {
            "settings.description.font_family_for_user_messages_in_the_agent_panel_falls_back_to_the_regular"
        }
        "Font size for user messages text in the agent panel." => {
            "settings.description.font_size_for_user_messages_text_in_the_agent_panel"
        }
        "Font family for the markdown preview. Falls back to the UI font family." => {
            "settings.description.font_family_for_the_markdown_preview_falls_back_to_the_ui_font_family"
        }
        "Font family for code blocks in the markdown preview. Falls back to the editor font family." => {
            "settings.description.font_family_for_code_blocks_in_the_markdown_preview_falls_back_to_the_editor"
        }
        "Font size for the markdown preview. Falls back to the editor font size." => {
            "settings.description.font_size_for_the_markdown_preview_falls_back_to_the_editor_font_size"
        }
        "The text rendering mode to use." => "settings.description.the_text_rendering_mode_to_use",
        "Modifier key for adding multiple cursors." => {
            "settings.description.modifier_key_for_adding_multiple_cursors"
        }
        "Whether the cursor blinks in the editor." => {
            "settings.description.whether_the_cursor_blinks_in_the_editor"
        }
        "Whether the cursor smoothly animates when moving around the editor." => {
            "settings.description.whether_the_cursor_smoothly_animates_when_moving_around_the_editor"
        }
        "Cursor shape for the editor." => "settings.description.cursor_shape_for_the_editor",
        "When to hide the mouse cursor." => "settings.description.when_to_hide_the_mouse_cursor",
        "Whether to reduce non-essential motion, such as loading spinners, by rendering them in a static state." => {
            "settings.description.whether_to_reduce_non_essential_motion_such_as_loading_spinners_by_rendering_them_in"
        }
        "How much to fade out unused code (0.0 - 0.9)." => {
            "settings.description.how_much_to_fade_out_unused_code_0_0_0_9"
        }
        "How to highlight the current line." => {
            "settings.description.how_to_highlight_the_current_line"
        }
        "Highlight all occurrences of selected text." => {
            "settings.description.highlight_all_occurrences_of_selected_text"
        }
        "Whether the text selection should have rounded corners." => {
            "settings.description.whether_the_text_selection_should_have_rounded_corners"
        }
        "The minimum APCA perceptual contrast to maintain when rendering text over highlight backgrounds." => {
            "settings.description.the_minimum_apca_perceptual_contrast_to_maintain_when_rendering_text_over_highlight_backgrounds"
        }
        "Show wrap guides (vertical rulers)." => {
            "settings.description.show_wrap_guides_vertical_rulers"
        }
        "Character counts at which to show wrap guides." => {
            "settings.description.character_counts_at_which_to_show_wrap_guides"
        }
        "This project is in restricted mode. Some project settings may not apply." => {
            "settings.description.this_project_is_in_restricted_mode_some_project_settings_may_not_apply"
        }
        "The name of a base set of key bindings to use." => {
            "settings.description.the_name_of_a_base_set_of_key_bindings_to_use"
        }
        "Customize keybindings in the keymap editor." => {
            "settings.description.customize_keybindings_in_the_keymap_editor"
        }
        "Enable Vim mode and key bindings." => {
            "settings.description.enable_vim_mode_and_key_bindings"
        }
        "Enable Helix mode and key bindings." => {
            "settings.description.enable_helix_mode_and_key_bindings"
        }
        "When to auto save buffer changes." => {
            "settings.description.when_to_auto_save_buffer_changes"
        }
        "Save after inactivity period (in milliseconds)." => {
            "settings.description.save_after_inactivity_period_in_milliseconds"
        }
        "Delay in milliseconds before the which-key menu appears." => {
            "settings.description.delay_in_milliseconds_before_the_which_key_menu_appears"
        }
        "Display the which-key menu with matching bindings while a multi-stroke binding is pending. The pending keystrokes indicator remains visible, but its binding preview popover is disabled." => {
            "settings.description.display_the_which_key_menu_with_matching_bindings_while_a_multi_stroke_binding_is"
        }
        "Restart Suzuri to apply. Translates menus, welcome screens, settings navigation and common settings; advanced descriptions may remain in English." => {
            "settings.description.restart_suzuri_to_apply_translates_menus_welcome_screens_settings_navigation_and_common_settings_advanced"
        }
        "How many columns a tab should occupy." => "settings.common.tab_size",
        "Whether to indent lines using tab characters, as opposed to multiple spaces." => {
            "settings.common.hard_tabs"
        }
        "How to soft-wrap long lines of text." => "settings.common.soft_wrap",
        "The column at which to soft-wrap lines, for buffers where soft-wrap is enabled." => {
            "settings.common.preferred_line_length"
        }
        "On: format the whole buffer.\nOff: do not format.\nModifications: format only lines with unstaged changes; skips formatting when a git diff or LSP range formatting is unavailable.\nModifications If Available: same, but falls back to formatting the whole buffer." => {
            "settings.common.format_on_save"
        }
        "Whether or not to remove any trailing whitespace from lines of a buffer before saving it." => {
            "settings.common.trailing_whitespace"
        }
        "Whether or not to ensure there's a single newline at the end of a buffer when saving it." => {
            "settings.common.final_newline"
        }
        "Modifications" => "settings.common.modifications",
        "Modifications If Available" => "settings.common.modifications_if_available",
        "Open Keymap" => "settings.common.open_keymap",
        "Test Audio" => "settings.common.test_audio",
        "Whole Word" => "settings.search.whole_word",
        "Search for whole words by default." => "settings.search.whole_word_description",
        "Case Sensitive" => "settings.search.case_sensitive",
        "Search case-sensitively by default." => "settings.search.case_sensitive_description",
        "Use Smartcase Search" => "settings.search.smartcase",
        "Whether to automatically enable case-sensitive search based on the search query." => {
            "settings.search.smartcase_description"
        }
        "Include Ignored" => "settings.search.include_ignored",
        "Include ignored files in search results by default." => {
            "settings.search.include_ignored_description"
        }
        "Regex" => "settings.search.regex",
        "Use regex search by default." => "settings.search.regex_description",
        "Search Wrap" => "settings.search.wrap",
        "Whether the editor search results will loop." => "settings.search.wrap_description",
        "Center on Match" => "settings.search.center_on_match",
        "Whether to center the current match in the editor" => {
            "settings.search.center_on_match_description"
        }
        "Search on Type" => "settings.search.on_type",
        "Start searching as you type in project search, without pressing Enter." => {
            "settings.search.on_type_description"
        }
        "Seed Search Query From Cursor" => "settings.search.seed_from_cursor",
        "When to populate a new search's query based on the text under the cursor." => {
            "settings.search.seed_from_cursor_description"
        }

        "Include Ignored in Search" => "settings.search.file_finder_include_ignored",
        "Use gitignored files when searching." => {
            "settings.search.file_finder_include_ignored_description"
        }
        "File Icons" => "settings.search.file_icons",
        "Show file icons in the file finder." => "settings.search.file_icons_description",
        "Skip Focus For Active In Search" => "settings.search.skip_active_file",
        "Whether the file finder should skip focus for the active file in search results." => {
            "settings.search.skip_active_file_description"
        }

        "File Scan Exclusions" => "settings.search.file_scan_exclusions",
        "Files or globs of files that will be excluded by Zed entirely. They will be skipped during file scans, file searches, and not be displayed in the project file tree. Takes precedence over \"File Scan Inclusions\"" => {
            "settings.search.file_scan_exclusions_description"
        }
        "File Scan Inclusions" => "settings.search.file_scan_inclusions",
        "Files or globs of files that will be included by Zed, even when ignored by git. This is useful for files that are not tracked by git, but are still important to your project. Note that globs that are overly broad can slow down Zed's file scanning. \"File Scan Exclusions\" takes precedence over these inclusions" => {
            "settings.search.file_scan_inclusions_description"
        }
        "File Scan Depth" => "settings.search.file_scan_depth",
        "Maximum directory depth to eagerly index outside of git repositories; contents of directories at this depth or deeper are indexed on demand. Repositories rooted shallower than this depth are always indexed fully. In projects that are not rooted at a git repository, repositories directly inside a root folder activate their git features immediately; deeper ones activate on first use. 0 means no limit and activates all git repositories immediately" => {
            "settings.search.file_scan_depth_description"
        }
        "Scan Symbolic Links" => "settings.search.scan_symbolic_links",
        "When to scan content of linked directories" => {
            "settings.search.scan_symbolic_links_description"
        }
        "Restore File State" => "settings.search.restore_file_state",
        "Restore previous file state when reopening." => {
            "settings.search.restore_file_state_description"
        }
        "Close on File Delete" => "settings.search.close_on_delete",
        "Automatically close files that have been deleted." => {
            "settings.search.close_on_delete_description"
        }
        "Collect timing data for foreground and background executor tasks so they can be inspected via `zed: open performance profiler`. May lead to increased memory usage." => {
            "settings.detail.collect_timing_data_for_foreground_and_background_executor_tasks_so_they_can_be_inspected_via_zed"
        }
        "Double Click In Multibuffer" => "settings.detail.double_click_in_multibuffer",
        "What to do when multibuffer is double-clicked in some of its excerpts." => {
            "settings.detail.what_to_do_when_multibuffer_is_double_clicked_in_some_of_its_excerpts"
        }
        "Expand Excerpt Lines" => "settings.detail.expand_excerpt_lines",
        "How many lines to expand the multibuffer excerpts by default." => {
            "settings.detail.how_many_lines_to_expand_the_multibuffer_excerpts_by_default"
        }
        "Excerpt Context Lines" => "settings.detail.excerpt_context_lines",
        "How many lines of context to provide in multibuffer excerpts by default." => {
            "settings.detail.how_many_lines_of_context_to_provide_in_multibuffer_excerpts_by_default"
        }
        "Expand Outlines With Depth" => "settings.detail.expand_outlines_with_depth",
        "Default depth to expand outline items in the current file." => {
            "settings.detail.default_depth_to_expand_outline_items_in_the_current_file"
        }
        "Diff View Style" => "settings.detail.diff_view_style",
        "How to display diffs in the editor." => {
            "settings.detail.how_to_display_diffs_in_the_editor"
        }
        "Minimum Split Diff Width" => "settings.detail.minimum_split_diff_width",
        "The minimum width (in columns) at which the split diff view is used. When the editor is narrower, the diff view automatically switches to unified mode. Set to 0 to disable." => {
            "settings.detail.the_minimum_width_in_columns_at_which_the_split_diff_view_is_used_when_the_editor"
        }
        "Scroll Beyond Last Line" => "settings.detail.scroll_beyond_last_line",
        "Whether the editor will scroll beyond the last line." => {
            "settings.detail.whether_the_editor_will_scroll_beyond_the_last_line"
        }
        "Vertical Scroll Margin" => "settings.detail.vertical_scroll_margin",
        "The number of lines to keep above/below the cursor when auto-scrolling." => {
            "settings.detail.the_number_of_lines_to_keep_above_below_the_cursor_when_auto_scrolling"
        }
        "Horizontal Scroll Margin" => "settings.detail.horizontal_scroll_margin",
        "The number of characters to keep on either side when scrolling with the mouse." => {
            "settings.detail.the_number_of_characters_to_keep_on_either_side_when_scrolling_with_the_mouse"
        }
        "Scroll Sensitivity" => "settings.detail.scroll_sensitivity",
        "Scroll sensitivity multiplier for both horizontal and vertical scrolling." => {
            "settings.detail.scroll_sensitivity_multiplier_for_both_horizontal_and_vertical_scrolling"
        }
        "Mouse Wheel Zoom" => "settings.detail.mouse_wheel_zoom",
        "Whether to zoom the editor font size with the mouse wheel while holding the primary modifier key." => {
            "settings.detail.whether_to_zoom_the_editor_font_size_with_the_mouse_wheel_while_holding_the_primary_modifier"
        }
        "Fast Scroll Sensitivity" => "settings.detail.fast_scroll_sensitivity",
        "Fast scroll sensitivity multiplier for both horizontal and vertical scrolling." => {
            "settings.detail.fast_scroll_sensitivity_multiplier_for_both_horizontal_and_vertical_scrolling"
        }
        "Autoscroll On Clicks" => "settings.detail.autoscroll_on_clicks",
        "Whether to scroll when clicking near the edge of the visible text area." => {
            "settings.detail.whether_to_scroll_when_clicking_near_the_edge_of_the_visible_text_area"
        }
        "Sticky Scroll" => "settings.detail.sticky_scroll",
        "Whether to stick scopes to the top of the editor" => {
            "settings.detail.whether_to_stick_scopes_to_the_top_of_the_editor"
        }
        "Auto Signature Help" => "settings.detail.auto_signature_help",
        "Automatically show a signature help pop-up." => {
            "settings.detail.automatically_show_a_signature_help_pop_up"
        }
        "Show Signature Help After Edits" => "settings.detail.show_signature_help_after_edits",
        "Show the signature help pop-up after completions or bracket pairs are inserted." => {
            "settings.detail.show_the_signature_help_pop_up_after_completions_or_bracket_pairs_are_inserted"
        }
        "Snippet Sort Order" => "settings.detail.snippet_sort_order",
        "Determines how snippets are sorted relative to other completion items." => {
            "settings.detail.determines_how_snippets_are_sorted_relative_to_other_completion_items"
        }
        "Show the informational hover box when moving the mouse over symbols in the editor." => {
            "settings.detail.show_the_informational_hover_box_when_moving_the_mouse_over_symbols_in_the_editor"
        }
        "Delay" => "settings.detail.delay",
        "Time to wait in milliseconds before showing the informational hover box." => {
            "settings.detail.time_to_wait_in_milliseconds_before_showing_the_informational_hover_box"
        }
        "Sticky" => "settings.detail.sticky",
        "Whether the hover popover sticks when the mouse moves toward it, allowing interaction with its contents." => {
            "settings.detail.whether_the_hover_popover_sticks_when_the_mouse_moves_toward_it_allowing_interaction_with_its_contents"
        }
        "Hiding Delay" => "settings.detail.hiding_delay",
        "Time to wait in milliseconds before hiding the hover popover after the mouse moves away." => {
            "settings.detail.time_to_wait_in_milliseconds_before_hiding_the_hover_popover_after_the_mouse_moves_away"
        }
        "Enable drag and drop selection." => "settings.detail.enable_drag_and_drop_selection",
        "Delay in milliseconds before drag and drop selection starts." => {
            "settings.detail.delay_in_milliseconds_before_drag_and_drop_selection_starts"
        }
        "Show Line Numbers" => "settings.detail.show_line_numbers",
        "Show line numbers in the gutter." => "settings.detail.show_line_numbers_in_the_gutter",
        "Relative Line Numbers" => "settings.detail.relative_line_numbers",
        "Controls line number display in the editor's gutter. \"disabled\" shows absolute line numbers, \"enabled\" shows relative line numbers for each absolute line, and \"wrapped\" shows relative line numbers for every line, absolute or wrapped." => {
            "settings.detail.controls_line_number_display_in_the_editor_s_gutter_disabled_shows_absolute_line_numbers_enabled_shows"
        }
        "Show Runnables" => "settings.detail.show_runnables",
        "Show runnable buttons in the gutter." => {
            "settings.detail.show_runnable_buttons_in_the_gutter"
        }
        "Show Breakpoints" => "settings.detail.show_breakpoints",
        "Show breakpoints in the gutter." => "settings.detail.show_breakpoints_in_the_gutter",
        "Show Bookmarks" => "settings.detail.show_bookmarks",
        "Show bookmarks in the gutter." => "settings.detail.show_bookmarks_in_the_gutter",
        "Show Folds" => "settings.detail.show_folds",
        "Show code folding controls in the gutter." => {
            "settings.detail.show_code_folding_controls_in_the_gutter"
        }
        "Min Line Number Digits" => "settings.detail.min_line_number_digits",
        "Minimum number of characters to reserve space for in the gutter." => {
            "settings.detail.minimum_number_of_characters_to_reserve_space_for_in_the_gutter"
        }
        "Git Gutter Width" => "settings.detail.git_gutter_width",
        "Width of the git diff indicators in the gutter. Default scales with the buffer font size." => {
            "settings.detail.width_of_the_git_diff_indicators_in_the_gutter_default_scales_with_the_buffer_font_size"
        }
        "Custom Width" => "settings.detail.custom_width",
        "Width in pixels of the git diff indicators." => {
            "settings.detail.width_in_pixels_of_the_git_diff_indicators"
        }
        "Inline Code Actions" => "settings.detail.inline_code_actions",
        "Show code action button at start of buffer line." => {
            "settings.detail.show_code_action_button_at_start_of_buffer_line"
        }
        "Show" => "settings.detail.show",
        "When to show the scrollbar in the editor." => {
            "settings.detail.when_to_show_the_scrollbar_in_the_editor"
        }
        "Cursors" => "settings.detail.cursors",
        "Show cursor positions in the scrollbar." => {
            "settings.detail.show_cursor_positions_in_the_scrollbar"
        }
        "Git Diff" => "settings.detail.git_diff",
        "Show Git diff indicators in the scrollbar." => {
            "settings.detail.show_git_diff_indicators_in_the_scrollbar"
        }
        "Search Results" => "settings.detail.search_results",
        "Show buffer search result indicators in the scrollbar." => {
            "settings.detail.show_buffer_search_result_indicators_in_the_scrollbar"
        }
        "Selected Text" => "settings.detail.selected_text",
        "Show selected text occurrences in the scrollbar." => {
            "settings.detail.show_selected_text_occurrences_in_the_scrollbar"
        }
        "Selected Symbol" => "settings.detail.selected_symbol",
        "Show selected symbol occurrences in the scrollbar." => {
            "settings.detail.show_selected_symbol_occurrences_in_the_scrollbar"
        }
        "Which diagnostic indicators to show in the scrollbar." => {
            "settings.detail.which_diagnostic_indicators_to_show_in_the_scrollbar"
        }
        "Horizontal Scrollbar" => "settings.detail.horizontal_scrollbar",
        "When false, forcefully disables the horizontal scrollbar." => {
            "settings.detail.when_false_forcefully_disables_the_horizontal_scrollbar"
        }
        "Vertical Scrollbar" => "settings.detail.vertical_scrollbar",
        "When false, forcefully disables the vertical scrollbar." => {
            "settings.detail.when_false_forcefully_disables_the_vertical_scrollbar"
        }
        "When to show the minimap in the editor." => {
            "settings.detail.when_to_show_the_minimap_in_the_editor"
        }
        "Display In" => "settings.detail.display_in",
        "Where to show the minimap in the editor." => {
            "settings.detail.where_to_show_the_minimap_in_the_editor"
        }
        "Thumb" => "settings.detail.thumb",
        "When to show the minimap thumb." => "settings.detail.when_to_show_the_minimap_thumb",
        "Thumb Border" => "settings.detail.thumb_border",
        "Border style for the minimap's scrollbar thumb." => {
            "settings.detail.border_style_for_the_minimap_s_scrollbar_thumb"
        }
        "How to highlight the current line in the minimap." => {
            "settings.detail.how_to_highlight_the_current_line_in_the_minimap"
        }
        "Max Width Columns" => "settings.detail.max_width_columns",
        "Maximum number of columns to display in the minimap." => {
            "settings.detail.maximum_number_of_columns_to_display_in_the_minimap"
        }
        "Breadcrumbs" => "settings.detail.breadcrumbs",
        "Show breadcrumbs." => "settings.detail.show_breadcrumbs",
        "Quick Actions" => "settings.detail.quick_actions",
        "Show quick action buttons (e.g., search, selection, editor controls, etc.)." => {
            "settings.detail.show_quick_action_buttons_e_g_search_selection_editor_controls_etc"
        }
        "Selections Menu" => "settings.detail.selections_menu",
        "Show the selections menu in the editor toolbar." => {
            "settings.detail.show_the_selections_menu_in_the_editor_toolbar"
        }
        "Agent Review" => "settings.detail.agent_review",
        "Show agent review buttons in the editor toolbar." => {
            "settings.detail.show_agent_review_buttons_in_the_editor_toolbar"
        }
        "Code Actions" => "settings.detail.code_actions",
        "Show code action buttons in the editor toolbar." => {
            "settings.detail.show_code_action_buttons_in_the_editor_toolbar"
        }
        "Default Mode" => "settings.detail.default_mode",
        "The default mode when Vim starts." => "settings.detail.the_default_mode_when_vim_starts",
        "Toggle Relative Line Numbers" => "settings.detail.toggle_relative_line_numbers",
        "Toggle relative line numbers in Vim mode." => {
            "settings.detail.toggle_relative_line_numbers_in_vim_mode"
        }
        "Use System Clipboard" => "settings.detail.use_system_clipboard",
        "Controls when to use system clipboard in Vim mode." => {
            "settings.detail.controls_when_to_use_system_clipboard_in_vim_mode"
        }
        "Use Smartcase Find" => "settings.detail.use_smartcase_find",
        "Enable smartcase searching in Vim mode." => {
            "settings.detail.enable_smartcase_searching_in_vim_mode"
        }
        "Global Substitution Default" => "settings.detail.global_substitution_default",
        "When enabled, the :substitute command replaces all matches in a line by default. The 'g' flag then toggles this behavior." => {
            "settings.detail.when_enabled_the_substitute_command_replaces_all_matches_in_a_line_by_default_the_g_flag"
        }
        "Highlight on Yank Duration" => "settings.detail.highlight_on_yank_duration",
        "Duration in milliseconds to highlight yanked text in Vim mode." => {
            "settings.detail.duration_in_milliseconds_to_highlight_yanked_text_in_vim_mode"
        }
        "Regex Search" => "settings.detail.regex_search",
        "Use regex search by default in Vim search." => {
            "settings.detail.use_regex_search_by_default_in_vim_search"
        }
        "Show Edit Predictions in Normal Mode" => {
            "settings.detail.show_edit_predictions_in_normal_mode"
        }
        "Whether edit predictions are shown in normal mode. By default, edit predictions are only shown in insert and replace modes." => {
            "settings.detail.whether_edit_predictions_are_shown_in_normal_mode_by_default_edit_predictions_are_only_shown_in"
        }
        "Cursor Shape - Normal Mode" => "settings.detail.cursor_shape_normal_mode",
        "Cursor shape for normal mode." => "settings.detail.cursor_shape_for_normal_mode",
        "Cursor Shape - Insert Mode" => "settings.detail.cursor_shape_insert_mode",
        "Cursor shape for insert mode. Inherit uses the editor's cursor shape." => {
            "settings.detail.cursor_shape_for_insert_mode_inherit_uses_the_editor_s_cursor_shape"
        }
        "Cursor Shape - Replace Mode" => "settings.detail.cursor_shape_replace_mode",
        "Cursor shape for replace mode." => "settings.detail.cursor_shape_for_replace_mode",
        "Cursor Shape - Visual Mode" => "settings.detail.cursor_shape_visual_mode",
        "Cursor shape for visual mode." => "settings.detail.cursor_shape_for_visual_mode",
        "Custom Digraphs" => "settings.detail.custom_digraphs",
        "Custom digraph mappings for Vim mode." => {
            "settings.detail.custom_digraph_mappings_for_vim_mode"
        }
        "File Type Associations" => "settings.detail.file_type_associations",
        "A mapping from languages to files and file extensions that should be treated as that language." => {
            "settings.detail.a_mapping_from_languages_to_files_and_file_extensions_that_should_be_treated_as_that_language"
        }
        "Max Severity" => "settings.detail.max_severity",
        "Which level to use to filter out diagnostics displayed in the editor." => {
            "settings.detail.which_level_to_use_to_filter_out_diagnostics_displayed_in_the_editor"
        }
        "Include Warnings" => "settings.detail.include_warnings",
        "Whether to show warnings or not by default." => {
            "settings.detail.whether_to_show_warnings_or_not_by_default"
        }
        "Whether to show diagnostics inline or not." => {
            "settings.detail.whether_to_show_diagnostics_inline_or_not"
        }
        "Update Debounce" => "settings.detail.update_debounce",
        "The delay in milliseconds to show inline diagnostics after the last diagnostic update." => {
            "settings.detail.the_delay_in_milliseconds_to_show_inline_diagnostics_after_the_last_diagnostic_update"
        }
        "Padding" => "settings.detail.padding",
        "The amount of padding between the end of the source line and the start of the inline diagnostic." => {
            "settings.detail.the_amount_of_padding_between_the_end_of_the_source_line_and_the_start_of_the"
        }
        "Minimum Column" => "settings.detail.minimum_column",
        "The minimum column at which to display inline diagnostics." => {
            "settings.detail.the_minimum_column_at_which_to_display_inline_diagnostics"
        }
        "Whether to pull for language server-powered diagnostics or not." => {
            "settings.detail.whether_to_pull_for_language_server_powered_diagnostics_or_not"
        }
        "Debounce" => "settings.detail.debounce",
        "Minimum time to wait before pulling diagnostics from the language server(s)." => {
            "settings.detail.minimum_time_to_wait_before_pulling_diagnostics_from_the_language_server_s"
        }
        "The debounce delay before querying highlights from the language." => {
            "settings.detail.the_debounce_delay_before_querying_highlights_from_the_language"
        }
        "Project Panel Button" => "settings.detail.project_panel_button",
        "Show the project panel button in the status bar." => {
            "settings.detail.show_the_project_panel_button_in_the_status_bar"
        }
        "Active Language Button" => "settings.detail.active_language_button",
        "Show the active language button in the status bar." => {
            "settings.detail.show_the_active_language_button_in_the_status_bar"
        }
        "Active Encoding Button" => "settings.detail.active_encoding_button",
        "Control when to show the active encoding in the status bar." => {
            "settings.detail.control_when_to_show_the_active_encoding_in_the_status_bar"
        }
        "Cursor Position Button" => "settings.detail.cursor_position_button",
        "Show the cursor position button in the status bar." => {
            "settings.detail.show_the_cursor_position_button_in_the_status_bar"
        }
        "Line Endings Button" => "settings.detail.line_endings_button",
        "Show the active line endings button in the status bar." => {
            "settings.detail.show_the_active_line_endings_button_in_the_status_bar"
        }
        "Pending Keystrokes Indicator" => "settings.detail.pending_keystrokes_indicator",
        "Show an indicator with a countdown while a multi-stroke key binding is pending. Its binding preview popover is disabled when the which-key menu is enabled." => {
            "settings.detail.show_an_indicator_with_a_countdown_while_a_multi_stroke_key_binding_is_pending_its_binding"
        }
        "Terminal Button" => "settings.detail.terminal_button",
        "Show the terminal button in the status bar." => {
            "settings.detail.show_the_terminal_button_in_the_status_bar"
        }
        "Diagnostics Button" => "settings.detail.diagnostics_button",
        "Show the project diagnostics button in the status bar." => {
            "settings.detail.show_the_project_diagnostics_button_in_the_status_bar"
        }
        "Project Search Button" => "settings.detail.project_search_button",
        "Show the project search button in the status bar." => {
            "settings.detail.show_the_project_search_button_in_the_status_bar"
        }
        "Debugger Button" => "settings.detail.debugger_button",
        "Show the debugger button in the status bar." => {
            "settings.detail.show_the_debugger_button_in_the_status_bar"
        }
        "Active File Name" => "settings.detail.active_file_name",
        "Show the name of the active file in the status bar." => {
            "settings.detail.show_the_name_of_the_active_file_in_the_status_bar"
        }
        "Show Branch Status Icon" => "settings.detail.show_branch_status_icon",
        "Show git status indicators on the branch icon in the titlebar." => {
            "settings.detail.show_git_status_indicators_on_the_branch_icon_in_the_titlebar"
        }
        "Show Branch Name" => "settings.detail.show_branch_name",
        "Show the branch name button in the titlebar." => {
            "settings.detail.show_the_branch_name_button_in_the_titlebar"
        }
        "Show Worktree Name" => "settings.detail.show_worktree_name",
        "Show the worktree name button in the titlebar." => {
            "settings.detail.show_the_worktree_name_button_in_the_titlebar"
        }
        "Show Project Items" => "settings.detail.show_project_items",
        "Show the project host and name in the titlebar." => {
            "settings.detail.show_the_project_host_and_name_in_the_titlebar"
        }
        "Show Onboarding Banner" => "settings.detail.show_onboarding_banner",
        "Show banners announcing new features in the titlebar." => {
            "settings.detail.show_banners_announcing_new_features_in_the_titlebar"
        }
        "Show Sign In" => "settings.detail.show_sign_in",
        "Show the sign in button in the titlebar." => {
            "settings.detail.show_the_sign_in_button_in_the_titlebar"
        }
        "Show User Menu" => "settings.detail.show_user_menu",
        "Show the user menu button in the titlebar." => {
            "settings.detail.show_the_user_menu_button_in_the_titlebar"
        }
        "Show User Picture" => "settings.detail.show_user_picture",
        "Show user picture in the titlebar." => "settings.detail.show_user_picture_in_the_titlebar",
        "Show Menus" => "settings.detail.show_menus",
        "Show the menus in the titlebar." => "settings.detail.show_the_menus_in_the_titlebar",
        "Button Layout" => "settings.detail.button_layout",
        "(Linux only) choose how window control buttons are laid out in the titlebar." => {
            "settings.detail.linux_only_choose_how_window_control_buttons_are_laid_out_in_the_titlebar"
        }
        "Custom Button Layout" => "settings.detail.custom_button_layout",
        "GNOME-style layout string such as \"close:minimize,maximize\"." => {
            "settings.detail.gnome_style_layout_string_such_as_close_minimize_maximize"
        }
        "Show Tab Bar" => "settings.detail.show_tab_bar",
        "Show the tab bar in the editor." => "settings.detail.show_the_tab_bar_in_the_editor",
        "Show Git Status In Tabs" => "settings.detail.show_git_status_in_tabs",
        "Show the Git file status on a tab item." => {
            "settings.detail.show_the_git_file_status_on_a_tab_item"
        }
        "Show File Icons In Tabs" => "settings.detail.show_file_icons_in_tabs",
        "Show the file icon for a tab." => "settings.detail.show_the_file_icon_for_a_tab",
        "Tab Close Position" => "settings.detail.tab_close_position",
        "Position of the close button in a tab." => {
            "settings.detail.position_of_the_close_button_in_a_tab"
        }
        "Maximum Tabs" => "settings.detail.maximum_tabs",
        "Maximum open tabs in a pane. Will not close an unsaved tab." => {
            "settings.detail.maximum_open_tabs_in_a_pane_will_not_close_an_unsaved_tab"
        }
        "Show Navigation History Buttons" => "settings.detail.show_navigation_history_buttons",
        "Show the navigation history buttons in the tab bar." => {
            "settings.detail.show_the_navigation_history_buttons_in_the_tab_bar"
        }
        "Show Tab Bar Buttons" => "settings.detail.show_tab_bar_buttons",
        "Show the tab bar buttons (New, Split Pane, Zoom)." => {
            "settings.detail.show_the_tab_bar_buttons_new_split_pane_zoom"
        }
        "Pinned Tabs Layout" => "settings.detail.pinned_tabs_layout",
        "Show pinned tabs in a separate row above unpinned tabs." => {
            "settings.detail.show_pinned_tabs_in_a_separate_row_above_unpinned_tabs"
        }
        "Activate On Close" => "settings.detail.activate_on_close",
        "What to do after closing the current tab." => {
            "settings.detail.what_to_do_after_closing_the_current_tab"
        }
        "Tab Show Diagnostics" => "settings.detail.tab_show_diagnostics",
        "Which files containing diagnostic errors/warnings to mark in the tabs." => {
            "settings.detail.which_files_containing_diagnostic_errors_warnings_to_mark_in_the_tabs"
        }
        "Show Close Button" => "settings.detail.show_close_button",
        "Controls the appearance behavior of the tab's close button." => {
            "settings.detail.controls_the_appearance_behavior_of_the_tab_s_close_button"
        }
        "Preview Tabs Enabled" => "settings.detail.preview_tabs_enabled",
        "Show opened editors as preview tabs." => {
            "settings.detail.show_opened_editors_as_preview_tabs"
        }
        "Enable Preview From Project Panel" => "settings.detail.enable_preview_from_project_panel",
        "Whether to open tabs in preview mode when opened from the project panel with a single click or the Open action." => {
            "settings.detail.whether_to_open_tabs_in_preview_mode_when_opened_from_the_project_panel_with_a_single"
        }
        "Enable Preview From File Finder" => "settings.detail.enable_preview_from_file_finder",
        "Whether to open tabs in preview mode when selected from the file finder." => {
            "settings.detail.whether_to_open_tabs_in_preview_mode_when_selected_from_the_file_finder"
        }
        "Enable Preview From Multibuffer" => "settings.detail.enable_preview_from_multibuffer",
        "Whether to open tabs in preview mode when opened from a multibuffer." => {
            "settings.detail.whether_to_open_tabs_in_preview_mode_when_opened_from_a_multibuffer"
        }
        "Enable Preview Multibuffer From Code Navigation" => {
            "settings.detail.enable_preview_multibuffer_from_code_navigation"
        }
        "Whether to open tabs in preview mode when code navigation is used to open a multibuffer." => {
            "settings.detail.whether_to_open_tabs_in_preview_mode_when_code_navigation_is_used_to_open_a_multibuffer"
        }
        "Enable Preview File From Code Navigation" => {
            "settings.detail.enable_preview_file_from_code_navigation"
        }
        "Whether to open tabs in preview mode when code navigation is used to open a single file." => {
            "settings.detail.whether_to_open_tabs_in_preview_mode_when_code_navigation_is_used_to_open_a_single"
        }
        "Enable Keep Preview On Code Navigation" => {
            "settings.detail.enable_keep_preview_on_code_navigation"
        }
        "Whether to keep tabs in preview mode when code navigation is used to navigate away from them. If `enable_preview_file_from_code_navigation` or `enable_preview_multibuffer_from_code_navigation` is also true, the new tab may replace the existing one." => {
            "settings.detail.whether_to_keep_tabs_in_preview_mode_when_code_navigation_is_used_to_navigate_away_from"
        }
        "Bottom Dock Layout" => "settings.detail.bottom_dock_layout",
        "Layout mode for the bottom dock." => "settings.detail.layout_mode_for_the_bottom_dock",
        "Centered Layout Left Padding" => "settings.detail.centered_layout_left_padding",
        "Left padding for centered layout." => "settings.detail.left_padding_for_centered_layout",
        "Centered Layout Right Padding" => "settings.detail.centered_layout_right_padding",
        "Right padding for centered layout." => "settings.detail.right_padding_for_centered_layout",
        "Focus Follows Mouse" => "settings.detail.focus_follows_mouse",
        "Whether to change focus to a pane when the mouse hovers over it." => {
            "settings.detail.whether_to_change_focus_to_a_pane_when_the_mouse_hovers_over_it"
        }
        "Focus Follows Mouse Debounce ms" => "settings.detail.focus_follows_mouse_debounce_ms",
        "Amount of time to wait before changing focus." => {
            "settings.detail.amount_of_time_to_wait_before_changing_focus"
        }
        "Title Format" => "settings.detail.title_format",
        "Window title template. Available variables are `${projectName}`, `${fileName}`, `${filePath}`, `${relativePath}`, `${fileStem}`, `${remoteName}`, `${remoteHost}`, `${appName}`, `${branch}`, and `${separator}`. `${separator}` is omitted when adjacent variables are empty, but literal text is preserved. The collaboration indicator, when present, is appended after the rendered template. If the template renders to nothing, the default template is used instead." => {
            "settings.detail.window_title_template_available_variables_are_projectname_filename_filepath_relativepath_filestem_remotename_remotehost_appname_branch_and"
        }
        "Title Separator" => "settings.detail.title_separator",
        "String substituted for `${separator}` in the window title format. Include any surrounding whitespace in the value." => {
            "settings.detail.string_substituted_for_separator_in_the_window_title_format_include_any_surrounding_whitespace_in_the_value"
        }
        "Use System Window Tabs" => "settings.detail.use_system_window_tabs",
        "(macOS only) whether to allow Windows to tab together." => {
            "settings.detail.macos_only_whether_to_allow_windows_to_tab_together"
        }
        "Fullscreen Mode" => "settings.detail.fullscreen_mode",
        "(macOS only) which fullscreen mode the toggle fullscreen action enters." => {
            "settings.detail.macos_only_which_fullscreen_mode_the_toggle_fullscreen_action_enters"
        }
        "Window Decorations" => "settings.detail.window_decorations",
        "(Linux only) whether Zed or your compositor should draw window decorations." => {
            "settings.detail.linux_only_whether_zed_or_your_compositor_should_draw_window_decorations"
        }
        "Inactive Opacity" => "settings.detail.inactive_opacity",
        "Opacity of inactive panels (0.0 - 1.0)." => {
            "settings.detail.opacity_of_inactive_panels_0_0_1_0"
        }
        "Border Size" => "settings.detail.border_size",
        "Size of the border surrounding the active pane." => {
            "settings.detail.size_of_the_border_surrounding_the_active_pane"
        }
        "Zoomed Padding" => "settings.detail.zoomed_padding",
        "Show padding for zoomed panes." => "settings.detail.show_padding_for_zoomed_panes",
        "Close Panel on Toggle" => "settings.detail.close_panel_on_toggle",
        "Whether invoking a panel's ToggleFocus action while it's already focused closes the panel, instead of just moving focus back to the editor." => {
            "settings.detail.whether_invoking_a_panel_s_togglefocus_action_while_it_s_already_focused_closes_the_panel_instead"
        }
        "Vertical Split Direction" => "settings.detail.vertical_split_direction",
        "Direction to split vertically." => "settings.detail.direction_to_split_vertically",
        "Horizontal Split Direction" => "settings.detail.horizontal_split_direction",
        "Direction to split horizontally." => "settings.detail.direction_to_split_horizontally",
        "Project Panel Dock" => "settings.detail.project_panel_dock",
        "Where to dock the project panel." => "settings.detail.where_to_dock_the_project_panel",
        "Project Panel Default Width" => "settings.detail.project_panel_default_width",
        "Default width of the project panel in pixels." => {
            "settings.detail.default_width_of_the_project_panel_in_pixels"
        }
        "Project Panel Title Tooltips Delay" => {
            "settings.detail.project_panel_title_tooltips_delay"
        }
        "Delay in milliseconds before tooltips appear for project panel titles." => {
            "settings.detail.delay_in_milliseconds_before_tooltips_appear_for_project_panel_titles"
        }
        "Custom Delay" => "settings.detail.custom_delay",
        "Delay in milliseconds of the project panel title tooltips." => {
            "settings.detail.delay_in_milliseconds_of_the_project_panel_title_tooltips"
        }
        "Hide .gitignore" => "settings.detail.hide_gitignore",
        "Whether to hide the gitignore entries in the project panel." => {
            "settings.detail.whether_to_hide_the_gitignore_entries_in_the_project_panel"
        }
        "Entry Spacing" => "settings.detail.entry_spacing",
        "Spacing between worktree entries in the project panel." => {
            "settings.detail.spacing_between_worktree_entries_in_the_project_panel"
        }
        "Show file icons in the project panel." => {
            "settings.detail.show_file_icons_in_the_project_panel"
        }
        "Folder Indicator" => "settings.detail.folder_indicator",
        "What to show for directories in the project panel." => {
            "settings.detail.what_to_show_for_directories_in_the_project_panel"
        }
        "Git Status" => "settings.detail.git_status",
        "Show the Git status in the project panel." => {
            "settings.detail.show_the_git_status_in_the_project_panel"
        }
        "Indent Size" => "settings.detail.indent_size",
        "Amount of indentation for nested items." => {
            "settings.detail.amount_of_indentation_for_nested_items"
        }
        "Auto Reveal Entries" => "settings.detail.auto_reveal_entries",
        "Whether to reveal entries in the project panel automatically when a corresponding project entry becomes active." => {
            "settings.detail.whether_to_reveal_entries_in_the_project_panel_automatically_when_a_corresponding_project_entry_becomes_active"
        }
        "Starts Open" => "settings.detail.starts_open",
        "Whether the project panel should open on startup." => {
            "settings.detail.whether_the_project_panel_should_open_on_startup"
        }
        "Auto Fold Directories" => "settings.detail.auto_fold_directories",
        "Whether to fold directories automatically and show compact folders when a directory has only one subdirectory inside." => {
            "settings.detail.whether_to_fold_directories_automatically_and_show_compact_folders_when_a_directory_has_only_one_subdirectory"
        }
        "Bold Folder Labels" => "settings.detail.bold_folder_labels",
        "Whether to show folder names with bold text in the project panel." => {
            "settings.detail.whether_to_show_folder_names_with_bold_text_in_the_project_panel"
        }
        "Show Scrollbar" => "settings.detail.show_scrollbar",
        "Show the scrollbar in the project panel." => {
            "settings.detail.show_the_scrollbar_in_the_project_panel"
        }
        "Horizontal Scroll" => "settings.detail.horizontal_scroll",
        "Whether to allow horizontal scrolling in the project panel. When disabled, the view is always locked to the leftmost position and long file names are clipped." => {
            "settings.detail.whether_to_allow_horizontal_scrolling_in_the_project_panel_when_disabled_the_view_is_always_locked"
        }
        "Show Diagnostics" => "settings.detail.show_diagnostics",
        "Which files containing diagnostic errors/warnings to mark in the project panel." => {
            "settings.detail.which_files_containing_diagnostic_errors_warnings_to_mark_in_the_project_panel"
        }
        "Diagnostic Badges" => "settings.detail.diagnostic_badges",
        "Show error and warning count badges next to file names in the project panel." => {
            "settings.detail.show_error_and_warning_count_badges_next_to_file_names_in_the_project_panel"
        }
        "Git Status Indicator" => "settings.detail.git_status_indicator",
        "Show a git status indicator next to file names in the project panel." => {
            "settings.detail.show_a_git_status_indicator_next_to_file_names_in_the_project_panel"
        }
        "Whether to stick parent directories at top of the project panel." => {
            "settings.detail.whether_to_stick_parent_directories_at_top_of_the_project_panel"
        }
        "Show Indent Guides" => "settings.detail.show_indent_guides",
        "Show indent guides in the project panel." => {
            "settings.detail.show_indent_guides_in_the_project_panel"
        }
        "Drag and Drop" => "settings.detail.drag_and_drop",
        "Whether to enable drag-and-drop operations in the project panel." => {
            "settings.detail.whether_to_enable_drag_and_drop_operations_in_the_project_panel"
        }
        "Hide Root" => "settings.detail.hide_root",
        "Whether to hide the root entry when only one folder is open in the window." => {
            "settings.detail.whether_to_hide_the_root_entry_when_only_one_folder_is_open_in_the_window"
        }
        "Hide Hidden" => "settings.detail.hide_hidden",
        "Whether to hide the hidden entries in the project panel." => {
            "settings.detail.whether_to_hide_the_hidden_entries_in_the_project_panel"
        }
        "Sort Mode" => "settings.detail.sort_mode",
        "Sort order for entries in the project panel." => {
            "settings.detail.sort_order_for_entries_in_the_project_panel"
        }
        "Sort Order" => "settings.detail.sort_order",
        "Whether to sort file and folder names case-sensitively in the project panel." => {
            "settings.detail.whether_to_sort_file_and_folder_names_case_sensitively_in_the_project_panel"
        }
        "Sort Direction" => "settings.detail.sort_direction",
        "Whether sibling entries sort ascending or descending in the project panel." => {
            "settings.detail.whether_sibling_entries_sort_ascending_or_descending_in_the_project_panel"
        }
        "Auto Open Files On Create" => "settings.detail.auto_open_files_on_create",
        "Whether to automatically open newly created files in the editor." => {
            "settings.detail.whether_to_automatically_open_newly_created_files_in_the_editor"
        }
        "Auto Open Files On Paste" => "settings.detail.auto_open_files_on_paste",
        "Whether to automatically open files after pasting or duplicating them." => {
            "settings.detail.whether_to_automatically_open_files_after_pasting_or_duplicating_them"
        }
        "Auto Open Files On Drop" => "settings.detail.auto_open_files_on_drop",
        "Whether to automatically open files dropped from external sources." => {
            "settings.detail.whether_to_automatically_open_files_dropped_from_external_sources"
        }
        "Hidden Files" => "settings.detail.hidden_files",
        "Globs to match files that will be considered \"hidden\" and can be hidden from the project panel." => {
            "settings.detail.globs_to_match_files_that_will_be_considered_hidden_and_can_be_hidden_from_the_project"
        }
        "Terminal Dock" => "settings.detail.terminal_dock",
        "Where to dock the terminal panel." => "settings.detail.where_to_dock_the_terminal_panel",
        "Whether the terminal panel should open on startup." => {
            "settings.detail.whether_the_terminal_panel_should_open_on_startup"
        }
        "Terminal Panel Flexible Sizing" => "settings.detail.terminal_panel_flexible_sizing",
        "Whether the terminal panel should use flexible (proportional) sizing when docked to the left or right." => {
            "settings.detail.whether_the_terminal_panel_should_use_flexible_proportional_sizing_when_docked_to_the_left_or_right"
        }
        "Show Count Badge" => "settings.detail.show_count_badge",
        "Show a badge on the terminal panel icon with the count of open terminals." => {
            "settings.detail.show_a_badge_on_the_terminal_panel_icon_with_the_count_of_open_terminals"
        }
        "Outline Panel Button" => "settings.detail.outline_panel_button",
        "Show the outline panel button in the status bar." => {
            "settings.detail.show_the_outline_panel_button_in_the_status_bar"
        }
        "Outline Panel Dock" => "settings.detail.outline_panel_dock",
        "Where to dock the outline panel." => "settings.detail.where_to_dock_the_outline_panel",
        "Outline Panel Default Width" => "settings.detail.outline_panel_default_width",
        "Default width of the outline panel in pixels." => {
            "settings.detail.default_width_of_the_outline_panel_in_pixels"
        }
        "Show file icons in the outline panel." => {
            "settings.detail.show_file_icons_in_the_outline_panel"
        }
        "What to show for directories in the outline panel." => {
            "settings.detail.what_to_show_for_directories_in_the_outline_panel"
        }
        "Show the Git status in the outline panel." => {
            "settings.detail.show_the_git_status_in_the_outline_panel"
        }
        "Whether to reveal when a corresponding outline entry becomes active." => {
            "settings.detail.whether_to_reveal_when_a_corresponding_outline_entry_becomes_active"
        }
        "Whether to fold directories automatically when a directory contains only one subdirectory." => {
            "settings.detail.whether_to_fold_directories_automatically_when_a_directory_contains_only_one_subdirectory"
        }
        "When to show indent guides in the outline panel." => {
            "settings.detail.when_to_show_indent_guides_in_the_outline_panel"
        }
        "Hide Symbols in Multi-Buffers" => "settings.detail.hide_symbols_in_multi_buffers",
        "Whether to hide symbols, excerpts and search matches in the outline panel when a multi-buffer view is active." => {
            "settings.detail.whether_to_hide_symbols_excerpts_and_search_matches_in_the_outline_panel_when_a_multi_buffer"
        }
        "Git Panel Button" => "settings.detail.git_panel_button",
        "Show the Git panel button in the status bar." => {
            "settings.detail.show_the_git_panel_button_in_the_status_bar"
        }
        "Git Panel Dock" => "settings.detail.git_panel_dock",
        "Where to dock the Git panel." => "settings.detail.where_to_dock_the_git_panel",
        "Whether the git panel should open on startup." => {
            "settings.detail.whether_the_git_panel_should_open_on_startup"
        }
        "Git Panel Default Width" => "settings.detail.git_panel_default_width",
        "Default width of the Git panel in pixels." => {
            "settings.detail.default_width_of_the_git_panel_in_pixels"
        }
        "Git Panel Status Style" => "settings.detail.git_panel_status_style",
        "How entry statuses are displayed." => "settings.detail.how_entry_statuses_are_displayed",
        "Fallback Branch Name" => "settings.detail.fallback_branch_name",
        "Default branch name will be when init.defaultbranch is not set in Git." => {
            "settings.detail.default_branch_name_will_be_when_init_defaultbranch_is_not_set_in_git"
        }
        "Sort By" => "settings.detail.sort_by",
        "How to sort entries in the git panel." => {
            "settings.detail.how_to_sort_entries_in_the_git_panel"
        }
        "Group By" => "settings.detail.group_by",
        "How to group entries in the git panel." => {
            "settings.detail.how_to_group_entries_in_the_git_panel"
        }
        "Collapse Untracked Diff" => "settings.detail.collapse_untracked_diff",
        "Whether to collapse untracked files in the diff panel." => {
            "settings.detail.whether_to_collapse_untracked_files_in_the_diff_panel"
        }
        "Tree View" => "settings.detail.tree_view",
        "Enable to show entries in tree view list, disable to show in flat view list." => {
            "settings.detail.enable_to_show_entries_in_tree_view_list_disable_to_show_in_flat_view_list"
        }
        "Show file icons next to the Git status icon." => {
            "settings.detail.show_file_icons_next_to_the_git_status_icon"
        }
        "What to show for directories in the git panel." => {
            "settings.detail.what_to_show_for_directories_in_the_git_panel"
        }
        "Diff Stats" => "settings.detail.diff_stats",
        "Whether to show the addition/deletion change count next to each file in the Git panel." => {
            "settings.detail.whether_to_show_the_addition_deletion_change_count_next_to_each_file_in_the_git_panel"
        }
        "Primary Click Behavior" => "settings.detail.primary_click_behavior",
        "Default action when clicking a changed file in the Git panel." => {
            "settings.detail.default_action_when_clicking_a_changed_file_in_the_git_panel"
        }
        "Whether to show a badge on the git panel icon with the count of uncommitted changes." => {
            "settings.detail.whether_to_show_a_badge_on_the_git_panel_icon_with_the_count_of_uncommitted_changes"
        }
        "Commit Title Max Length" => "settings.detail.commit_title_max_length",
        "Maximum length of the commit message title before a warning is shown. Set to 0 to disable." => {
            "settings.detail.maximum_length_of_the_commit_message_title_before_a_warning_is_shown_set_to_0_to"
        }
        "Scroll Bar" => "settings.detail.scroll_bar",
        "How and when the scrollbar should be displayed." => {
            "settings.detail.how_and_when_the_scrollbar_should_be_displayed"
        }
        "Debugger Panel Dock" => "settings.detail.debugger_panel_dock",
        "The dock position of the debug panel." => {
            "settings.detail.the_dock_position_of_the_debug_panel"
        }
        "Collaboration Panel Button" => "settings.detail.collaboration_panel_button",
        "Show the collaboration panel button in the status bar." => {
            "settings.detail.show_the_collaboration_panel_button_in_the_status_bar"
        }
        "Collaboration Panel Dock" => "settings.detail.collaboration_panel_dock",
        "Where to dock the collaboration panel." => {
            "settings.detail.where_to_dock_the_collaboration_panel"
        }
        "Collaboration Panel Default Width" => "settings.detail.collaboration_panel_default_width",
        "Default width of the collaboration panel in pixels." => {
            "settings.detail.default_width_of_the_collaboration_panel_in_pixels"
        }
        "Agent Panel Button" => "settings.detail.agent_panel_button",
        "Whether to show the agent panel button in the status bar." => {
            "settings.detail.whether_to_show_the_agent_panel_button_in_the_status_bar"
        }
        "Agent Panel Dock" => "settings.detail.agent_panel_dock",
        "Where to dock the agent panel." => "settings.detail.where_to_dock_the_agent_panel",
        "Agent Panel Flexible Sizing" => "settings.detail.agent_panel_flexible_sizing",
        "Whether the agent panel should use flexible (proportional) sizing when docked to the left or right." => {
            "settings.detail.whether_the_agent_panel_should_use_flexible_proportional_sizing_when_docked_to_the_left_or_right"
        }
        "Agent Panel Default Width" => "settings.detail.agent_panel_default_width",
        "Default width when the agent panel is docked to the left or right." => {
            "settings.detail.default_width_when_the_agent_panel_is_docked_to_the_left_or_right"
        }
        "Agent Panel Default Height" => "settings.detail.agent_panel_default_height",
        "Default height when the agent panel is docked to the bottom." => {
            "settings.detail.default_height_when_the_agent_panel_is_docked_to_the_bottom"
        }
        "Limit Content Width" => "settings.detail.limit_content_width",
        "Whether to constrain the agent panel content to a maximum width, centering it when the panel is wider, for optimal readability." => {
            "settings.detail.whether_to_constrain_the_agent_panel_content_to_a_maximum_width_centering_it_when_the_panel"
        }
        "Max Content Width" => "settings.detail.max_content_width",
        "Maximum content width in pixels. Content will be centered when the panel is wider than this value." => {
            "settings.detail.maximum_content_width_in_pixels_content_will_be_centered_when_the_panel_is_wider_than_this"
        }
        "Stepping Granularity" => "settings.detail.stepping_granularity",
        "Determines the stepping granularity for debug operations." => {
            "settings.detail.determines_the_stepping_granularity_for_debug_operations"
        }
        "Save Breakpoints" => "settings.detail.save_breakpoints",
        "Whether breakpoints should be reused across Zed sessions." => {
            "settings.detail.whether_breakpoints_should_be_reused_across_zed_sessions"
        }
        "Timeout" => "settings.detail.timeout",
        "Time in milliseconds until timeout error when connecting to a TCP debug adapter." => {
            "settings.detail.time_in_milliseconds_until_timeout_error_when_connecting_to_a_tcp_debug_adapter"
        }
        "Log DAP Communications" => "settings.detail.log_dap_communications",
        "Whether to log messages between active debug adapters and Zed." => {
            "settings.detail.whether_to_log_messages_between_active_debug_adapters_and_zed"
        }
        "Format DAP Log Messages" => "settings.detail.format_dap_log_messages",
        "Whether to format DAP messages when adding them to debug adapter logger." => {
            "settings.detail.whether_to_format_dap_messages_when_adding_them_to_debug_adapter_logger"
        }
        "Shell" => "settings.detail.shell",
        "What shell to use when opening a terminal." => {
            "settings.detail.what_shell_to_use_when_opening_a_terminal"
        }
        "Program" => "settings.detail.program",
        "The shell program to use." => "settings.detail.the_shell_program_to_use",
        "The shell program to run." => "settings.detail.the_shell_program_to_run",
        "Arguments" => "settings.detail.arguments",
        "The arguments to pass to the shell program." => {
            "settings.detail.the_arguments_to_pass_to_the_shell_program"
        }
        "Title Override" => "settings.detail.title_override",
        "An optional string to override the title of the terminal tab." => {
            "settings.detail.an_optional_string_to_override_the_title_of_the_terminal_tab"
        }
        "Working Directory" => "settings.detail.working_directory",
        "What working directory to use when launching the terminal." => {
            "settings.detail.what_working_directory_to_use_when_launching_the_terminal"
        }
        "Directory" => "settings.detail.directory",
        "The directory path to use (will be shell expanded)." => {
            "settings.detail.the_directory_path_to_use_will_be_shell_expanded"
        }
        "Environment Variables" => "settings.detail.environment_variables",
        "Key-value pairs to add to the terminal's environment." => {
            "settings.detail.key_value_pairs_to_add_to_the_terminal_s_environment"
        }
        "Detect Virtual Environment" => "settings.detail.detect_virtual_environment",
        "Activates the Python virtual environment, if one is found, in the terminal's working directory." => {
            "settings.detail.activates_the_python_virtual_environment_if_one_is_found_in_the_terminal_s_working_directory"
        }
        "Font size for terminal text. If not set, defaults to buffer font size." => {
            "settings.detail.font_size_for_terminal_text_if_not_set_defaults_to_buffer_font_size"
        }
        "Font family for terminal text. If not set, defaults to buffer font family." => {
            "settings.detail.font_family_for_terminal_text_if_not_set_defaults_to_buffer_font_family"
        }
        "Font fallbacks for terminal text. If not set, defaults to buffer font fallbacks." => {
            "settings.detail.font_fallbacks_for_terminal_text_if_not_set_defaults_to_buffer_font_fallbacks"
        }
        "Font weight for terminal text in CSS weight units (100-900)." => {
            "settings.detail.font_weight_for_terminal_text_in_css_weight_units_100_900"
        }
        "Font features for terminal text." => "settings.detail.font_features_for_terminal_text",
        "Line height for terminal text." => "settings.detail.line_height_for_terminal_text",
        "Default cursor shape for the terminal (bar, block, underline, or hollow)." => {
            "settings.detail.default_cursor_shape_for_the_terminal_bar_block_underline_or_hollow"
        }
        "Cursor Blinking" => "settings.detail.cursor_blinking",
        "Sets the cursor blinking behavior in the terminal." => {
            "settings.detail.sets_the_cursor_blinking_behavior_in_the_terminal"
        }
        "Alternate Scroll" => "settings.detail.alternate_scroll",
        "Whether alternate scroll mode is active by default (converts mouse scroll to arrow keys in apps like Vim)." => {
            "settings.detail.whether_alternate_scroll_mode_is_active_by_default_converts_mouse_scroll_to_arrow_keys_in_apps"
        }
        "Minimum Contrast" => "settings.detail.minimum_contrast",
        "The minimum APCA perceptual contrast between foreground and background colors (0-106)." => {
            "settings.detail.the_minimum_apca_perceptual_contrast_between_foreground_and_background_colors_0_106"
        }
        "Option As Meta" => "settings.detail.option_as_meta",
        "Whether the option key behaves as the meta key." => {
            "settings.detail.whether_the_option_key_behaves_as_the_meta_key"
        }
        "Copy On Select" => "settings.detail.copy_on_select",
        "Whether selecting text in the terminal automatically copies to the system clipboard." => {
            "settings.detail.whether_selecting_text_in_the_terminal_automatically_copies_to_the_system_clipboard"
        }
        "Keep Selection On Copy" => "settings.detail.keep_selection_on_copy",
        "Whether to keep the text selection after copying it to the clipboard." => {
            "settings.detail.whether_to_keep_the_text_selection_after_copying_it_to_the_clipboard"
        }
        "Open Links In Mouse Mode" => "settings.detail.open_links_in_mouse_mode",
        "Whether cmd-click (ctrl-click on Linux and Windows) opens hyperlinks even when the terminal application has enabled mouse reporting. When disabled, these clicks are forwarded to the application; links can still be opened with shift-cmd-click." => {
            "settings.detail.whether_cmd_click_ctrl_click_on_linux_and_windows_opens_hyperlinks_even_when_the_terminal_application"
        }
        "Audible Bell" => "settings.detail.audible_bell",
        "Whether to play a sound when the BEL character (`\\a`, `0x07`) is printed" => {
            "settings.detail.whether_to_play_a_sound_when_the_bel_character_a_0x07_is_printed"
        }
        "Default Width" => "settings.detail.default_width",
        "Default width when the terminal is docked to the left or right (in pixels)." => {
            "settings.detail.default_width_when_the_terminal_is_docked_to_the_left_or_right_in_pixels"
        }
        "Default Height" => "settings.detail.default_height",
        "Default height when the terminal is docked to the bottom (in pixels)." => {
            "settings.detail.default_height_when_the_terminal_is_docked_to_the_bottom_in_pixels"
        }
        "Max Scroll History Lines" => "settings.detail.max_scroll_history_lines",
        "Maximum number of lines to keep in scrollback history (max: 100,000; 0 disables scrolling)." => {
            "settings.detail.maximum_number_of_lines_to_keep_in_scrollback_history_max_100_000_0_disables_scrolling"
        }
        "Scroll Multiplier" => "settings.detail.scroll_multiplier",
        "The multiplier for scrolling in the terminal with the mouse wheel" => {
            "settings.detail.the_multiplier_for_scrolling_in_the_terminal_with_the_mouse_wheel"
        }
        "Display the terminal title in breadcrumbs inside the terminal pane." => {
            "settings.detail.display_the_terminal_title_in_breadcrumbs_inside_the_terminal_pane"
        }
        "When to show the scrollbar in the terminal." => {
            "settings.detail.when_to_show_the_scrollbar_in_the_terminal"
        }
        "Disable Git Integration" => "settings.detail.disable_git_integration",
        "Disable all Git integration features in Zed." => {
            "settings.detail.disable_all_git_integration_features_in_zed"
        }
        "Enable Git Status" => "settings.detail.enable_git_status",
        "Show Git status information in the editor." => {
            "settings.detail.show_git_status_information_in_the_editor"
        }
        "Enable Git Diff" => "settings.detail.enable_git_diff",
        "Show Git diff information in the editor." => {
            "settings.detail.show_git_diff_information_in_the_editor"
        }
        "Visibility" => "settings.detail.visibility",
        "Control whether Git status is shown in the editor's gutter." => {
            "settings.detail.control_whether_git_status_is_shown_in_the_editor_s_gutter"
        }
        "Debounce threshold in milliseconds after which changes are reflected in the Git gutter." => {
            "settings.detail.debounce_threshold_in_milliseconds_after_which_changes_are_reflected_in_the_git_gutter"
        }
        "Whether or not to show Git blame data for the currently focused line." => {
            "settings.detail.whether_or_not_to_show_git_blame_data_for_the_currently_focused_line"
        }
        "Location" => "settings.detail.location",
        "Where to render Git blame when it is enabled." => {
            "settings.detail.where_to_render_git_blame_when_it_is_enabled"
        }
        "The delay after which the inline blame information is shown." => {
            "settings.detail.the_delay_after_which_the_inline_blame_information_is_shown"
        }
        "Padding between the end of the source line and the start of the inline blame in columns." => {
            "settings.detail.padding_between_the_end_of_the_source_line_and_the_start_of_the_inline_blame_in"
        }
        "The minimum column number at which to show the inline blame information." => {
            "settings.detail.the_minimum_column_number_at_which_to_show_the_inline_blame_information"
        }
        "Show Commit Summary" => "settings.detail.show_commit_summary",
        "Show commit summary as part of the inline blame." => {
            "settings.detail.show_commit_summary_as_part_of_the_inline_blame"
        }
        "Show Avatar" => "settings.detail.show_avatar",
        "Show the avatar of the author of the commit." => {
            "settings.detail.show_the_avatar_of_the_author_of_the_commit"
        }
        "Show Author Name" => "settings.detail.show_author_name",
        "Show author name as part of the commit information in branch picker." => {
            "settings.detail.show_author_name_as_part_of_the_commit_information_in_branch_picker"
        }
        "Hunk Style" => "settings.detail.hunk_style",
        "How Git hunks are displayed visually in the editor." => {
            "settings.detail.how_git_hunks_are_displayed_visually_in_the_editor"
        }
        "Diff Base" => "settings.detail.diff_base",
        "Whether git features show changes relative to HEAD (uncommitted changes) or to the default branch (all changes on the current branch)." => {
            "settings.detail.whether_git_features_show_changes_relative_to_head_uncommitted_changes_or_to_the_default_branch_all"
        }
        "Path Style" => "settings.detail.path_style",
        "Should the name or path be displayed first in the git view." => {
            "settings.detail.should_the_name_or_path_be_displayed_first_in_the_git_view"
        }
        "Show Stage/Restore Buttons" => "settings.detail.show_stage_restore_buttons",
        "Whether to show the stage and restore buttons on diff hunks." => {
            "settings.detail.whether_to_show_the_stage_and_restore_buttons_on_diff_hunks"
        }
        "Show Full File by Default" => "settings.detail.show_full_file_by_default",
        "Whether newly opened file diffs show the full file instead of changes only." => {
            "settings.detail.whether_newly_opened_file_diffs_show_the_full_file_instead_of_changes_only"
        }
        "Mute On Join" => "settings.detail.mute_on_join",
        "Whether the microphone should be muted when joining a channel or a call." => {
            "settings.detail.whether_the_microphone_should_be_muted_when_joining_a_channel_or_a_call"
        }
        "Share On Join" => "settings.detail.share_on_join",
        "Whether your current project should be shared when joining an empty channel." => {
            "settings.detail.whether_your_current_project_should_be_shared_when_joining_an_empty_channel"
        }
        "Output Audio Device" => "settings.detail.output_audio_device",
        "Select output audio device" => "settings.detail.select_output_audio_device",
        "Input Audio Device" => "settings.detail.input_audio_device",
        "Select input audio device" => "settings.detail.select_input_audio_device",
        "Disable AI" => "settings.detail.disable_ai",
        "Whether to disable all AI features in Zed." => {
            "settings.detail.whether_to_disable_all_ai_features_in_zed"
        }
        "Threads Sidebar Side" => "settings.detail.threads_sidebar_side",
        "Which side of the window the threads sidebar appears on." => {
            "settings.detail.which_side_of_the_window_the_threads_sidebar_appears_on"
        }
        "LLM Providers" => "settings.detail.llm_providers",
        "External Agents" => "settings.detail.external_agents",
        "MCP Servers" => "settings.detail.mcp_servers",
        "Skills" => "settings.detail.skills",
        "Sandbox" => "settings.detail.sandbox",
        "Tool Permissions" => "settings.detail.tool_permissions",
        "Single File Review" => "settings.detail.single_file_review",
        "When enabled, agent edits will also be displayed in single-file buffers for review." => {
            "settings.detail.when_enabled_agent_edits_will_also_be_displayed_in_single_file_buffers_for_review"
        }
        "Enable Feedback" => "settings.detail.enable_feedback",
        "Show voting thumbs up/down icon buttons for feedback on agent edits." => {
            "settings.detail.show_voting_thumbs_up_down_icon_buttons_for_feedback_on_agent_edits"
        }
        "Notify When Agent Waiting" => "settings.detail.notify_when_agent_waiting",
        "Where to show notifications when the agent has completed its response or needs confirmation before running a tool action." => {
            "settings.detail.where_to_show_notifications_when_the_agent_has_completed_its_response_or_needs_confirmation_before_running"
        }
        "Play Sound When Agent Done" => "settings.detail.play_sound_when_agent_done",
        "When to play a sound when the agent has either completed its response, or needs user input." => {
            "settings.detail.when_to_play_a_sound_when_the_agent_has_either_completed_its_response_or_needs_user"
        }
        "Prevent Idle Sleep" => "settings.detail.prevent_idle_sleep",
        "Whether to keep the system awake while agent threads are running." => {
            "settings.detail.whether_to_keep_the_system_awake_while_agent_threads_are_running"
        }
        "Expand Edit Card" => "settings.detail.expand_edit_card",
        "Whether to have edit cards in the agent panel expanded, showing a Preview of the diff." => {
            "settings.detail.whether_to_have_edit_cards_in_the_agent_panel_expanded_showing_a_preview_of_the_diff"
        }
        "Expand Terminal Card" => "settings.detail.expand_terminal_card",
        "Whether to have terminal cards in the agent panel expanded, showing the whole command output." => {
            "settings.detail.whether_to_have_terminal_cards_in_the_agent_panel_expanded_showing_the_whole_command_output"
        }
        "Terminal Thread Init Command" => "settings.detail.terminal_thread_init_command",
        "Command to automatically run when Zed creates a Terminal Thread shell in the agent panel. Runs in your configured shell." => {
            "settings.detail.command_to_automatically_run_when_zed_creates_a_terminal_thread_shell_in_the_agent_panel_runs"
        }
        "Thinking Display" => "settings.detail.thinking_display",
        "How thinking blocks should be displayed by default. 'Auto' fully expands during streaming, then auto-collapses when done. 'Preview' auto-expands with a height constraint during streaming. 'Always Expanded' shows full content. 'Always Collapsed' keeps them collapsed." => {
            "settings.detail.how_thinking_blocks_should_be_displayed_by_default_auto_fully_expands_during_streaming_then_auto_collapses"
        }
        "Cancel Generation On Terminal Stop" => {
            "settings.detail.cancel_generation_on_terminal_stop"
        }
        "Whether clicking the stop button on a running terminal tool should also cancel the agent's generation. Note that this only applies to the stop button, not to ctrl+c inside the terminal." => {
            "settings.detail.whether_clicking_the_stop_button_on_a_running_terminal_tool_should_also_cancel_the_agent_s"
        }
        "Use Modifier To Send" => "settings.detail.use_modifier_to_send",
        "Whether to always use cmd-enter (or ctrl-enter on Linux or Windows) to send messages." => {
            "settings.detail.whether_to_always_use_cmd_enter_or_ctrl_enter_on_linux_or_windows_to_send_messages"
        }
        "Message Editor Min Lines" => "settings.detail.message_editor_min_lines",
        "Minimum number of lines to display in the agent message editor." => {
            "settings.detail.minimum_number_of_lines_to_display_in_the_agent_message_editor"
        }
        "Show Turn Stats" => "settings.detail.show_turn_stats",
        "Whether to show turn statistics like elapsed time during generation and final turn duration." => {
            "settings.detail.whether_to_show_turn_statistics_like_elapsed_time_during_generation_and_final_turn_duration"
        }
        "Show Merge Conflict Indicator" => "settings.detail.show_merge_conflict_indicator",
        "Whether to show the merge conflict indicator in the status bar that offers to resolve conflicts using the agent." => {
            "settings.detail.whether_to_show_the_merge_conflict_indicator_in_the_status_bar_that_offers_to_resolve_conflicts"
        }
        "Auto Compact" => "settings.detail.auto_compact",
        "Automatically compact the agent's context when it grows too large, summarizing earlier messages to free up room in the model's context window." => {
            "settings.detail.automatically_compact_the_agent_s_context_when_it_grows_too_large_summarizing_earlier_messages_to_free"
        }
        "Auto Compact Threshold" => "settings.detail.auto_compact_threshold",
        "When auto compaction runs. A percentage string like \"90%\" is measured against the context window. A positive integer is the number of used tokens to compact after. A negative integer is the number of tokens remaining in the context window before compacting." => {
            "settings.detail.when_auto_compaction_runs_a_percentage_string_like_90_is_measured_against_the_context_window_a"
        }
        "Display Mode" => "settings.detail.display_mode",
        "When to show edit predictions previews in buffer. The eager mode displays them inline, while the subtle mode displays them only when holding a modifier key." => {
            "settings.detail.when_to_show_edit_predictions_previews_in_buffer_the_eager_mode_displays_them_inline_while_the"
        }
        "Proxy" => "settings.detail.proxy",
        "The proxy to use for network requests." => {
            "settings.detail.the_proxy_to_use_for_network_requests"
        }
        "Server URL" => "settings.detail.server_url",
        "The URL of the Zed server to connect to." => {
            "settings.detail.the_url_of_the_zed_server_to_connect_to"
        }
        "Auto Indent" => "settings.detail.auto_indent",
        "Controls automatic indentation behavior when typing." => {
            "settings.detail.controls_automatic_indentation_behavior_when_typing"
        }
        "Auto Indent On Paste" => "settings.detail.auto_indent_on_paste",
        "Whether indentation of pasted content should be adjusted based on the context." => {
            "settings.detail.whether_indentation_of_pasted_content_should_be_adjusted_based_on_the_context"
        }
        "Show wrap guides in the editor." => "settings.detail.show_wrap_guides_in_the_editor",
        "Character counts at which to show wrap guides in the editor." => {
            "settings.detail.character_counts_at_which_to_show_wrap_guides_in_the_editor"
        }
        "Allow Rewrap" => "settings.detail.allow_rewrap",
        "Controls where the `editor::rewrap` action is allowed for this language." => {
            "settings.detail.controls_where_the_editor_rewrap_action_is_allowed_for_this_language"
        }
        "Display indent guides in the editor." => {
            "settings.detail.display_indent_guides_in_the_editor"
        }
        "Line Width" => "settings.detail.line_width",
        "The width of the indent guides in pixels, between 1 and 10." => {
            "settings.detail.the_width_of_the_indent_guides_in_pixels_between_1_and_10"
        }
        "Active Line Width" => "settings.detail.active_line_width",
        "The width of the active indent guide in pixels, between 1 and 10." => {
            "settings.detail.the_width_of_the_active_indent_guide_in_pixels_between_1_and_10"
        }
        "Coloring" => "settings.detail.coloring",
        "Determines how indent guides are colored." => {
            "settings.detail.determines_how_indent_guides_are_colored"
        }
        "Background Coloring" => "settings.detail.background_coloring",
        "Determines how indent guide backgrounds are colored." => {
            "settings.detail.determines_how_indent_guide_backgrounds_are_colored"
        }
        "Line Ending" => "settings.detail.line_ending",
        "How line endings should be handled for new files and during format and save operations." => {
            "settings.detail.how_line_endings_should_be_handled_for_new_files_and_during_format_and_save_operations"
        }
        "Formatter" => "settings.detail.formatter",
        "How to perform a buffer format." => "settings.detail.how_to_perform_a_buffer_format",
        "Use On Type Format" => "settings.detail.use_on_type_format",
        "Whether to use additional LSP queries to format (and amend) the code after every \"trigger\" symbol input, defined by LSP server capabilities" => {
            "settings.detail.whether_to_use_additional_lsp_queries_to_format_and_amend_the_code_after_every_trigger_symbol"
        }
        "Code Actions On Format" => "settings.detail.code_actions_on_format",
        "Additional code actions to run when formatting." => {
            "settings.detail.additional_code_actions_to_run_when_formatting"
        }
        "Use Autoclose" => "settings.detail.use_autoclose",
        "Whether to automatically type closing characters for you. For example, when you type '(', Zed will automatically add a closing ')' at the correct position." => {
            "settings.detail.whether_to_automatically_type_closing_characters_for_you_for_example_when_you_type_zed_will_automatically"
        }
        "Use Auto Surround" => "settings.detail.use_auto_surround",
        "Whether to automatically surround text with characters for you. For example, when you select text and type '(', Zed will automatically surround text with ()." => {
            "settings.detail.whether_to_automatically_surround_text_with_characters_for_you_for_example_when_you_select_text_and"
        }
        "Always Treat Brackets As Autoclosed" => {
            "settings.detail.always_treat_brackets_as_autoclosed"
        }
        "Controls whether the closing characters are always skipped over and auto-removed no matter how they were inserted." => {
            "settings.detail.controls_whether_the_closing_characters_are_always_skipped_over_and_auto_removed_no_matter_how_they"
        }
        "JSX Tag Auto Close" => "settings.detail.jsx_tag_auto_close",
        "Whether to automatically close JSX tags." => {
            "settings.detail.whether_to_automatically_close_jsx_tags"
        }
        "Show Whitespaces" => "settings.detail.show_whitespaces",
        "Whether to show tabs and spaces in the editor." => {
            "settings.detail.whether_to_show_tabs_and_spaces_in_the_editor"
        }
        "Space Whitespace Indicator" => "settings.detail.space_whitespace_indicator",
        "Visible character used to render space characters when show_whitespaces is enabled (default: \"•\")" => {
            "settings.detail.visible_character_used_to_render_space_characters_when_show_whitespaces_is_enabled_default"
        }
        "Tab Whitespace Indicator" => "settings.detail.tab_whitespace_indicator",
        "Visible character used to render tab characters when show_whitespaces is enabled (default: \"→\")" => {
            "settings.detail.visible_character_used_to_render_tab_characters_when_show_whitespaces_is_enabled_default"
        }
        "Show Completions On Input" => "settings.detail.show_completions_on_input",
        "Whether to pop the completions menu while typing in an editor without explicitly requesting it." => {
            "settings.detail.whether_to_pop_the_completions_menu_while_typing_in_an_editor_without_explicitly_requesting_it"
        }
        "Show Completion Documentation" => "settings.detail.show_completion_documentation",
        "Whether to display inline and alongside documentation for items in the completions menu." => {
            "settings.detail.whether_to_display_inline_and_alongside_documentation_for_items_in_the_completions_menu"
        }
        "Words" => "settings.detail.words",
        "Controls how words are completed." => "settings.detail.controls_how_words_are_completed",
        "Words Min Length" => "settings.detail.words_min_length",
        "How many characters has to be in the completions query to automatically show the words-based completions." => {
            "settings.detail.how_many_characters_has_to_be_in_the_completions_query_to_automatically_show_the_words_based"
        }
        "Completion Menu Scrollbar" => "settings.detail.completion_menu_scrollbar",
        "When to show the scrollbar in the completion menu." => {
            "settings.detail.when_to_show_the_scrollbar_in_the_completion_menu"
        }
        "Completion Detail Alignment" => "settings.detail.completion_detail_alignment",
        "Whether to align detail text in code completions context menus left or right." => {
            "settings.detail.whether_to_align_detail_text_in_code_completions_context_menus_left_or_right"
        }
        "Completion Menu Item Kind" => "settings.detail.completion_menu_item_kind",
        "How to display the LSP item kind (function, method, variable, etc.) of each entry in the completions menu." => {
            "settings.detail.how_to_display_the_lsp_item_kind_function_method_variable_etc_of_each_entry_in_the"
        }
        "Global switch to toggle hints on and off." => {
            "settings.detail.global_switch_to_toggle_hints_on_and_off"
        }
        "Show Value Hints" => "settings.detail.show_value_hints",
        "Global switch to toggle inline values on and off when debugging." => {
            "settings.detail.global_switch_to_toggle_inline_values_on_and_off_when_debugging"
        }
        "Show Type Hints" => "settings.detail.show_type_hints",
        "Whether type hints should be shown." => {
            "settings.detail.whether_type_hints_should_be_shown"
        }
        "Show Parameter Hints" => "settings.detail.show_parameter_hints",
        "Whether parameter hints should be shown." => {
            "settings.detail.whether_parameter_hints_should_be_shown"
        }
        "Show Other Hints" => "settings.detail.show_other_hints",
        "Whether other hints should be shown." => {
            "settings.detail.whether_other_hints_should_be_shown"
        }
        "Show Background" => "settings.detail.show_background",
        "Show a background for inlay hints." => "settings.detail.show_a_background_for_inlay_hints",
        "Edit Debounce Ms" => "settings.detail.edit_debounce_ms",
        "Whether or not to debounce inlay hints updates after buffer edits (set to 0 to disable debouncing)." => {
            "settings.detail.whether_or_not_to_debounce_inlay_hints_updates_after_buffer_edits_set_to_0_to_disable"
        }
        "Scroll Debounce Ms" => "settings.detail.scroll_debounce_ms",
        "Whether or not to debounce inlay hints updates after buffer scrolls (set to 0 to disable debouncing)." => {
            "settings.detail.whether_or_not_to_debounce_inlay_hints_updates_after_buffer_scrolls_set_to_0_to_disable"
        }
        "Toggle On Modifiers Press" => "settings.detail.toggle_on_modifiers_press",
        "Toggles inlay hints (hides or shows) when the user presses the modifiers specified." => {
            "settings.detail.toggles_inlay_hints_hides_or_shows_when_the_user_presses_the_modifiers_specified"
        }
        "Whether tasks are enabled for this language." => {
            "settings.detail.whether_tasks_are_enabled_for_this_language"
        }
        "Variables" => "settings.detail.variables",
        "Extra task variables to set for a particular language." => {
            "settings.detail.extra_task_variables_to_set_for_a_particular_language"
        }
        "Prefer LSP" => "settings.detail.prefer_lsp",
        "Use LSP tasks over Zed language extension tasks." => {
            "settings.detail.use_lsp_tasks_over_zed_language_extension_tasks"
        }
        "Language Detection" => "settings.detail.language_detection",
        "Whether to enable automatic language detection in unsaved buffers." => {
            "settings.detail.whether_to_enable_automatic_language_detection_in_unsaved_buffers"
        }
        "Word Diff Enabled" => "settings.detail.word_diff_enabled",
        "Whether to enable word diff highlighting in the editor. When enabled, changed words within modified lines are highlighted to show exactly what changed." => {
            "settings.detail.whether_to_enable_word_diff_highlighting_in_the_editor_when_enabled_changed_words_within_modified_lines"
        }
        "Preferred debuggers for this language." => {
            "settings.detail.preferred_debuggers_for_this_language"
        }
        "Middle Click Paste" => "settings.detail.middle_click_paste",
        "Enable middle-click paste on Linux." => {
            "settings.detail.enable_middle_click_paste_on_linux"
        }
        "Extend Comment On Newline" => "settings.detail.extend_comment_on_newline",
        "Whether to start a new line with a comment when a previous line is a comment as well." => {
            "settings.detail.whether_to_start_a_new_line_with_a_comment_when_a_previous_line_is_a_comment"
        }
        "Colorize Brackets" => "settings.detail.colorize_brackets",
        "Whether to colorize brackets in the editor." => {
            "settings.detail.whether_to_colorize_brackets_in_the_editor"
        }
        "Vim/Emacs Modeline Support" => "settings.detail.vim_emacs_modeline_support",
        "Number of lines to search for modelines (set to 0 to disable)." => {
            "settings.detail.number_of_lines_to_search_for_modelines_set_to_0_to_disable"
        }
        "Image Viewer" => "settings.detail.image_viewer",
        "The unit for image file sizes." => "settings.detail.the_unit_for_image_file_sizes",
        "Open Markdown Files in Preview" => "settings.detail.open_markdown_files_in_preview",
        "Whether to automatically open Markdown files in the preview." => {
            "settings.detail.whether_to_automatically_open_markdown_files_in_the_preview"
        }
        "Limit Markdown Preview Width" => "settings.detail.limit_markdown_preview_width",
        "Whether to constrain the markdown preview content to a maximum width, centering it when the pane is wider, for optimal readability." => {
            "settings.detail.whether_to_constrain_the_markdown_preview_content_to_a_maximum_width_centering_it_when_the_pane"
        }
        "Max Width" => "settings.detail.max_width",
        "Maximum content width in pixels. Content will be centered when the pane is wider than this value." => {
            "settings.detail.maximum_content_width_in_pixels_content_will_be_centered_when_the_pane_is_wider_than_this"
        }
        "Drop Size Target" => "settings.detail.drop_size_target",
        "Relative size of the drop target in the editor that will open dropped file as a split pane." => {
            "settings.detail.relative_size_of_the_drop_target_in_the_editor_that_will_open_dropped_file_as_a"
        }
        "Code Lens" => "settings.detail.code_lens",
        "Whether and how to display code lenses from language servers." => {
            "settings.detail.whether_and_how_to_display_code_lenses_from_language_servers"
        }
        "LSP Document Colors" => "settings.detail.lsp_document_colors",
        "How to render LSP color previews in the editor." => {
            "settings.detail.how_to_render_lsp_color_previews_in_the_editor"
        }
        "Enable Language Server" => "settings.detail.enable_language_server",
        "Whether to use language servers to provide code intelligence." => {
            "settings.detail.whether_to_use_language_servers_to_provide_code_intelligence"
        }
        "Language Servers" => "settings.detail.language_servers",
        "The list of language servers to use (or disable) for this language." => {
            "settings.detail.the_list_of_language_servers_to_use_or_disable_for_this_language"
        }
        "Linked Edits" => "settings.detail.linked_edits",
        "Whether to perform linked edits of associated ranges, if the LS supports it. For example, when editing opening <html> tag, the contents of the closing </html> tag will be edited as well." => {
            "settings.detail.whether_to_perform_linked_edits_of_associated_ranges_if_the_ls_supports_it_for_example_when"
        }
        "Go To Definition Fallback" => "settings.detail.go_to_definition_fallback",
        "Whether to follow-up empty Go to definition responses from the language server." => {
            "settings.detail.whether_to_follow_up_empty_go_to_definition_responses_from_the_language_server"
        }
        "Go To Definition Scroll Strategy" => "settings.detail.go_to_definition_scroll_strategy",
        "How to scroll the target into view when navigating to a definition or reference." => {
            "settings.detail.how_to_scroll_the_target_into_view_when_navigating_to_a_definition_or_reference"
        }
        "LSP Results Location" => "settings.detail.lsp_results_location",
        "Where to show LSP results that can contain multiple locations (Go to Definition, Go to Implementation, Find All References)." => {
            "settings.detail.where_to_show_lsp_results_that_can_contain_multiple_locations_go_to_definition_go_to_implementation"
        }
        "Semantic Tokens" => "settings.detail.semantic_tokens",
        "LSP Folding Ranges" => "settings.detail.lsp_folding_ranges",
        "When enabled, use folding ranges from the language server instead of indent-based folding." => {
            "settings.detail.when_enabled_use_folding_ranges_from_the_language_server_instead_of_indent_based_folding"
        }
        "LSP Document Symbols" => "settings.detail.lsp_document_symbols",
        "When enabled, use the language server's document symbols for outlines and breadcrumbs instead of tree-sitter." => {
            "settings.detail.when_enabled_use_the_language_server_s_document_symbols_for_outlines_and_breadcrumbs_instead_of_tree"
        }
        "Whether to fetch LSP completions or not." => {
            "settings.detail.whether_to_fetch_lsp_completions_or_not"
        }
        "Fetch Timeout (milliseconds)" => "settings.detail.fetch_timeout_milliseconds",
        "When fetching LSP completions, determines how long to wait for a response of a particular server (set to 0 to wait indefinitely)." => {
            "settings.detail.when_fetching_lsp_completions_determines_how_long_to_wait_for_a_response_of_a_particular_server"
        }
        "Insert Mode" => "settings.detail.insert_mode",
        "Controls how LSP completions are inserted." => {
            "settings.detail.controls_how_lsp_completions_are_inserted"
        }
        "Allowed" => "settings.detail.allowed",
        "Enables or disables formatting with Prettier for a given language." => {
            "settings.detail.enables_or_disables_formatting_with_prettier_for_a_given_language"
        }
        "Parser" => "settings.detail.parser",
        "Forces Prettier integration to use a specific parser name when formatting files with the language." => {
            "settings.detail.forces_prettier_integration_to_use_a_specific_parser_name_when_formatting_files_with_the_language"
        }
        "Plugins" => "settings.detail.plugins",
        "Forces Prettier integration to use specific plugins when formatting files with the language." => {
            "settings.detail.forces_prettier_integration_to_use_specific_plugins_when_formatting_files_with_the_language"
        }
        "Options" => "settings.detail.options",
        "Default Prettier options, in the format as in package.json section for Prettier." => {
            "settings.detail.default_prettier_options_in_the_format_as_in_package_json_section_for_prettier"
        }
        "Configure Providers" => "settings.detail.configure_providers",
        "Data Collection" => "settings.detail.data_collection",
        "Controls whether Zed may collect training data when using Zed's Edit Predictions. Data is only collected for files in projects detected as open source. The default value uses the preference previously set via the status-bar toggle, or false if no preference has been stored." => {
            "settings.detail.controls_whether_zed_may_collect_training_data_when_using_zed_s_edit_predictions_data_is_only"
        }
        "Show Edit Predictions" => "settings.detail.show_edit_predictions",
        "Controls whether edit predictions are shown immediately or manually." => {
            "settings.detail.controls_whether_edit_predictions_are_shown_immediately_or_manually"
        }
        "Disable in Language Scopes" => "settings.detail.disable_in_language_scopes",
        "Controls whether edit predictions are shown in the given language scopes." => {
            "settings.detail.controls_whether_edit_predictions_are_shown_in_the_given_language_scopes"
        }
        "Empty Tab" => "settings.extra.empty_tab",
        "Contained" => "settings.extra.contained",
        "Full" => "settings.extra.full",
        "Left Aligned" => "settings.extra.left_aligned",
        "Right Aligned" => "settings.extra.right_aligned",
        "Add to Existing Window" => "settings.extra.add_to_existing_window",
        "Add To Existing Window" => "settings.extra.add_to_existing_window_2",
        "Open a New Window" => "settings.extra.open_a_new_window",
        "Open A New Window" => "settings.extra.open_a_new_window_2",
        "Keep Window Open" => "settings.extra.keep_window_open",
        "Subpixel" => "settings.extra.subpixel",
        "Grayscale" => "settings.extra.grayscale",
        "Cmd Or Ctrl" => "settings.extra.cmd_or_ctrl",
        "On Typing" => "settings.extra.on_typing",
        "On Typing And Action" => "settings.extra.on_typing_and_action",
        "Boundary" => "settings.extra.boundary",
        "Trailing" => "settings.extra.trailing",
        "Prefer Line" => "settings.extra.prefer_line",
        "Syntax Aware" => "settings.extra.syntax_aware",
        "Preserve Indent" => "settings.extra.preserve_indent",
        "One Page" => "settings.extra.one_page",
        "Inline" => "settings.extra.inline",
        "Tracked Files" => "settings.extra.tracked_files",
        "Hide" => "settings.extra.hide",
        "Staged Hollow" => "settings.extra.staged_hollow",
        "Unstaged Hollow" => "settings.extra.unstaged_hollow",
        "Head" => "settings.extra.head",
        "Default Branch" => "settings.extra.default_branch",
        "File Name First" => "settings.extra.file_name_first",
        "File Path First" => "settings.extra.file_path_first",
        "Error" => "settings.extra.error",
        "Warning" => "settings.extra.warning",
        "Info" => "settings.extra.info",
        "Hint" => "settings.extra.hint",
        "Select" => "settings.extra.select",
        "Open" => "settings.extra.open",
        "Find All References" => "settings.extra.find_all_references",
        "Center" => "settings.extra.center",
        "Minimum" => "settings.extra.minimum",
        "Preserve" => "settings.extra.preserve",
        "Multi Buffer" => "settings.extra.multi_buffer",
        "Picker" => "settings.extra.picker",
        "History" => "settings.extra.history",
        "Neighbour" => "settings.extra.neighbour",
        "Left Neighbour" => "settings.extra.left_neighbour",
        "Errors" => "settings.extra.errors",
        "Hover" => "settings.extra.hover",
        "Hidden" => "settings.extra.hidden",
        "Icon" => "settings.extra.icon",
        "Chevron" => "settings.extra.chevron",
        "Both" => "settings.extra.both",
        "Directories First" => "settings.extra.directories_first",
        "Mixed" => "settings.extra.mixed",
        "Files First" => "settings.extra.files_first",
        "Upper" => "settings.extra.upper",
        "Lower" => "settings.extra.lower",
        "Ascending" => "settings.extra.ascending",
        "Descending" => "settings.extra.descending",
        "In Comments" => "settings.extra.in_comments",
        "In Selections" => "settings.extra.in_selections",
        "Anywhere" => "settings.extra.anywhere",
        "Detect" => "settings.extra.detect",
        "Prefer Lf" => "settings.extra.prefer_lf",
        "Prefer Crlf" => "settings.extra.prefer_crlf",
        "Enforce Lf" => "settings.extra.enforce_lf",
        "Enforce Crlf" => "settings.extra.enforce_crlf",
        "Prefer LF" => "settings.extra.prefer_lf_2",
        "Prefer CRLF" => "settings.extra.prefer_crlf_2",
        "Enforce LF" => "settings.extra.enforce_lf_2",
        "Enforce CRLF" => "settings.extra.enforce_crlf_2",
        "Fixed" => "settings.extra.fixed",
        "Indent Aware" => "settings.extra.indent_aware",
        "Fallback" => "settings.extra.fallback",
        "Insert" => "settings.extra.insert",
        "Replace" => "settings.extra.replace",
        "Replace Subsequence" => "settings.extra.replace_subsequence",
        "Replace Suffix" => "settings.extra.replace_suffix",
        "Symbol" => "settings.extra.symbol",
        "Unified" => "settings.extra.unified",
        "Split" => "settings.extra.split",
        "Terminal Controlled" => "settings.extra.terminal_controlled",
        "Infer" => "settings.extra.infer",
        "Yes" => "settings.extra.yes",
        "No" => "settings.extra.no",
        "Auto" => "settings.extra.auto",
        "Information" => "settings.extra.information",
        "All Editors" => "settings.extra.all_editors",
        "Active Editor" => "settings.extra.active_editor",
        "Left Open" => "settings.extra.left_open",
        "Right Open" => "settings.extra.right_open",
        "Left Only" => "settings.extra.left_only",
        "Normal" => "settings.extra.normal",
        "On Yank" => "settings.extra.on_yank",
        "Statement" => "settings.extra.statement",
        "Instruction" => "settings.extra.instruction",
        "Primary Screen" => "settings.extra.primary_screen",
        "All Screens" => "settings.extra.all_screens",
        "When Hidden" => "settings.extra.when_hidden",
        "Preview" => "settings.extra.preview",
        "Always Expanded" => "settings.extra.always_expanded",
        "Always Collapsed" => "settings.extra.always_collapsed",
        "Binary" => "settings.extra.binary",
        "Decimal" => "settings.extra.decimal",
        "Label Color" => "settings.extra.label_color",
        "Project Diff" => "settings.extra.project_diff",
        "View File" => "settings.extra.view_file",
        "Path" => "settings.extra.path",
        "Name" => "settings.extra.name",
        "Status" => "settings.extra.status",
        "Staging" => "settings.extra.staging",
        "Non Utf8" => "settings.extra.non_utf8",
        "Up" => "settings.extra.up",
        "Down" => "settings.extra.down",
        "Menu" => "settings.extra.menu",
        "Inlay" => "settings.extra.inlay",
        "Border" => "settings.extra.border",
        "Background" => "settings.extra.background",
        "Simplified Chinese" => "settings.extra.simplified_chinese",
        "Custom" => "settings.extra.custom",
        "Current File Directory" => "settings.extra.current_file_directory",
        "Current Project Directory" => "settings.extra.current_project_directory",
        "First Project Directory" => "settings.extra.first_project_directory",
        "Always Home" => "settings.extra.always_home",
        "Indexed" => "settings.extra.indexed",
        "Smart" => "settings.extra.smart",
        "With Arguments" => "settings.extra.with_arguments",
        "Subtle" => "settings.extra.subtle",
        "Eager" => "settings.extra.eager",
        "Wrapped" => "settings.extra.wrapped",
        "Client" => "settings.extra.client",
        "Server" => "settings.extra.server",
        "Native" => "settings.extra.native",
        "Simple" => "settings.extra.simple",
        "Expanded" => "settings.extra.expanded",
        "Combined" => "settings.extra.combined",
        "System Default" => "settings.extra.system_default",
        "Stop Testing" => "settings.extra.stop_testing",
        "Start Testing" => "settings.extra.start_testing",
        "Output Device" => "settings.extra.output_device",
        "Input Device" => "settings.extra.input_device",
        "Audio Test" => "settings.extra.audio_test",
        "OpenAI Compatible API" => "settings.extra.openai_compatible_api",
        "The API key sent as Authorization: Bearer {key}." => {
            "settings.extra.the_api_key_sent_as_authorization_bearer_key"
        }
        "No provider set" => "settings.extra.no_provider_set",
        "Active Provider" => "settings.extra.active_provider",
        "Provider" => "settings.extra.provider",
        "Select which provider to use for edit predictions." => {
            "settings.extra.select_which_provider_to_use_for_edit_predictions"
        }
        "Visit the" => "settings.extra.visit_the",
        "to generate an API key." => "settings.extra.to_generate_an_api_key",
        "API Key Set in Environment Variable" => {
            "settings.extra.api_key_set_in_environment_variable"
        }
        "API Key Configured" => "settings.extra.api_key_configured",
        "Reset Key" => "settings.extra.reset_key",
        "API Key" => "settings.extra.api_key",
        "API URL" => "settings.extra.api_url",
        "The base URL of your Ollama server." => {
            "settings.extra.the_base_url_of_your_ollama_server"
        }
        "Model" => "settings.extra.model",
        "The Ollama model to use for edit predictions." => {
            "settings.extra.the_ollama_model_to_use_for_edit_predictions"
        }
        "Prompt Format" => "settings.extra.prompt_format",
        "The prompt format to use when requesting predictions. Set to Infer to have the format inferred based on the model name." => {
            "settings.extra.the_prompt_format_to_use_when_requesting_predictions_set_to_infer_to_have_the_format_inferred"
        }
        "Max Output Tokens" => "settings.extra.max_output_tokens",
        "The maximum number of tokens to generate." => {
            "settings.extra.the_maximum_number_of_tokens_to_generate"
        }
        "Prediction Debounce" => "settings.extra.prediction_debounce",
        "Delay in milliseconds before automatically requesting a prediction after typing stops. Set to 0 to request predictions immediately." => {
            "settings.extra.delay_in_milliseconds_before_automatically_requesting_a_prediction_after_typing_stops_set_to_0_to_request"
        }
        "The URL of your OpenAI-compatible server's completions API." => {
            "settings.extra.the_url_of_your_openai_compatible_server_s_completions_api"
        }
        "The model string to pass to the OpenAI-compatible server." => {
            "settings.extra.the_model_string_to_pass_to_the_openai_compatible_server"
        }
        "The API URL to use for Codestral." => "settings.extra.the_api_url_to_use_for_codestral",
        "Max Tokens" => "settings.extra.max_tokens",
        "The Codestral model id to use." => "settings.extra.the_codestral_model_id_to_use",
        "Zed Predictions" => "settings.extra.zed_predictions",
        "Agents connected through the Agent Client Protocol." => {
            "settings.extra.agents_connected_through_the_agent_client_protocol"
        }
        "No external agents added yet. Click \"Add Agent\" to get started." => {
            "settings.extra.no_external_agents_added_yet_click_add_agent_to_get_started"
        }
        "No active project found. Open a workspace to manage external agents." => {
            "settings.extra.no_active_project_found_open_a_workspace_to_manage_external_agents"
        }
        "Configure Agent" => "settings.extra.configure_agent",
        "Remove Registry Agent" => "settings.extra.remove_registry_agent",
        "Remove Custom Agent" => "settings.extra.remove_custom_agent",
        "Add Agent" => "settings.extra.add_agent",
        "Install from Registry" => "settings.extra.install_from_registry",
        "Add Custom Agent" => "settings.extra.add_custom_agent",
        "Learn More" => "settings.extra.learn_more",
        "ACP Docs" => "settings.extra.acp_docs",
        "Key" => "settings.extra.key",
        "Value" => "settings.extra.value",
        "Configure External Agent" => "settings.extra.configure_external_agent",
        "Agent Name" => "settings.extra.agent_name",
        "Required. A unique name used to identify this agent." => {
            "settings.extra.required_a_unique_name_used_to_identify_this_agent"
        }
        "Command" => "settings.extra.command",
        "Required. Path to the executable that launches the agent." => {
            "settings.extra.required_path_to_the_executable_that_launches_the_agent"
        }
        "Space-separated arguments passed to the command." => {
            "settings.extra.space_separated_arguments_passed_to_the_command"
        }
        "Environment variables provided to the agent process." => {
            "settings.extra.environment_variables_provided_to_the_agent_process"
        }
        "Agent name is required." => "settings.extra.agent_name_is_required",
        "Command is required." => "settings.extra.command_is_required",
        "Add Provider" => "settings.extra.add_provider",
        "Compatible APIs" => "settings.extra.compatible_apis",
        "Configure Provider" => "settings.extra.configure_provider",
        "To find an API key, visit the" => "settings.extra.to_find_an_api_key_visit_the",
        "This provider will use an OpenAI-compatible API." => {
            "settings.extra.this_provider_will_use_an_openai_compatible_api"
        }
        "This provider will use an Anthropic Messages-compatible API." => {
            "settings.extra.this_provider_will_use_an_anthropic_messages_compatible_api"
        }
        "Provider Name" => "settings.extra.provider_name",
        "A unique name used to identify this provider." => {
            "settings.extra.a_unique_name_used_to_identify_this_provider"
        }
        "The base URL for the compatible API." => {
            "settings.extra.the_base_url_for_the_compatible_api"
        }
        "Stored in the system keychain, not in settings.json." => {
            "settings.extra.stored_in_the_system_keychain_not_in_settings_json"
        }
        "Models" => "settings.extra.models",
        "Add Model" => "settings.extra.add_model",
        "Model Name" => "settings.extra.model_name",
        "The model's name in the provider's API." => {
            "settings.extra.the_model_s_name_in_the_provider_s_api"
        }
        "Max Completion Tokens" => "settings.extra.max_completion_tokens",
        "Maximum completion tokens for OpenAI-compatible requests." => {
            "settings.extra.maximum_completion_tokens_for_openai_compatible_requests"
        }
        "The maximum number of tokens the model can output." => {
            "settings.extra.the_maximum_number_of_tokens_the_model_can_output"
        }
        "The model context window size." => "settings.extra.the_model_context_window_size",
        "Remove Model" => "settings.extra.remove_model",
        "Supports tools" => "settings.extra.supports_tools",
        "Supports images" => "settings.extra.supports_images",
        "Supports parallel_tool_calls" => "settings.extra.supports_parallel_tool_calls",
        "Supports prompt_cache_key" => "settings.extra.supports_prompt_cache_key",
        "Supports /chat/completions" => "settings.extra.supports_chat_completions",
        "Uses max_tokens for output limit" => "settings.extra.uses_max_tokens_for_output_limit",
        "Supports thinking" => "settings.extra.supports_thinking",
        "Preserves thinking in chat history" => "settings.extra.preserves_thinking_in_chat_history",
        "Default reasoning effort" => "settings.extra.default_reasoning_effort",
        "Save Provider" => "settings.extra.save_provider",
        "Settings update was canceled" => "settings.extra.settings_update_was_canceled",
        "Provider was not registered" => "settings.extra.provider_was_not_registered",
        "Provider Name cannot be empty" => "settings.extra.provider_name_cannot_be_empty",
        "Provider Name is already taken by another provider" => {
            "settings.extra.provider_name_is_already_taken_by_another_provider"
        }
        "API URL cannot be empty" => "settings.extra.api_url_cannot_be_empty",
        "API Key cannot be empty" => "settings.extra.api_key_cannot_be_empty",
        "Model Names must be unique" => "settings.extra.model_names_must_be_unique",
        "Model Name cannot be empty" => "settings.extra.model_name_cannot_be_empty",
        "Configured Servers" => "settings.extra.configured_servers",
        "Manage servers connected directly or via extensions." => {
            "settings.extra.manage_servers_connected_directly_or_via_extensions"
        }
        "MCP Server Timeout" => "settings.extra.mcp_server_timeout",
        "Default timeout in seconds for MCP server tool calls." => {
            "settings.extra.default_timeout_in_seconds_for_mcp_server_tool_calls"
        }
        "No MCP servers added yet. Click \"Add Server\" to get started." => {
            "settings.extra.no_mcp_servers_added_yet_click_add_server_to_get_started"
        }
        "No active project found. Open a workspace to manage MCP servers." => {
            "settings.extra.no_active_project_found_open_a_workspace_to_manage_mcp_servers"
        }
        "Configure MCP Server" => "settings.extra.configure_mcp_server",
        "Uninstall MCP Server" => "settings.extra.uninstall_mcp_server",
        "Log Out" => "settings.extra.log_out",
        "Authenticate to connect this server" => {
            "settings.extra.authenticate_to_connect_this_server"
        }
        "Authenticate" => "settings.extra.authenticate",
        "A client secret is required to connect this server" => {
            "settings.extra.a_client_secret_is_required_to_connect_this_server"
        }
        "Authenticating…" => "settings.extra.authenticating",
        "Add Server" => "settings.extra.add_server",
        "Add Local Server" => "settings.extra.add_local_server",
        "Add Remote Server" => "settings.extra.add_remote_server",
        "Install from Extensions" => "settings.extra.install_from_extensions",
        "Optional OAuth client ID" => "settings.extra.optional_oauth_client_id",
        "Add Local MCP Server" => "settings.extra.add_local_mcp_server",
        "Add Remote MCP Server" => "settings.extra.add_remote_mcp_server",
        "Server Name" => "settings.extra.server_name",
        "Required. A unique name used to identify this MCP server." => {
            "settings.extra.required_a_unique_name_used_to_identify_this_mcp_server"
        }
        "Required. Path to the executable that launches the server." => {
            "settings.extra.required_path_to_the_executable_that_launches_the_server"
        }
        "Environment variables provided to the server process." => {
            "settings.extra.environment_variables_provided_to_the_server_process"
        }
        "Timeout (seconds)" => "settings.extra.timeout_seconds",
        "How long to wait for the server to respond before timing out." => {
            "settings.extra.how_long_to_wait_for_the_server_to_respond_before_timing_out"
        }
        "Required. The base URL of the remote MCP server." => {
            "settings.extra.required_the_base_url_of_the_remote_mcp_server"
        }
        "Headers" => "settings.extra.headers",
        "HTTP headers sent with each request to the server." => {
            "settings.extra.http_headers_sent_with_each_request_to_the_server"
        }
        "OAuth Client ID" => "settings.extra.oauth_client_id",
        "Optional OAuth client ID used to authenticate with the server." => {
            "settings.extra.optional_oauth_client_id_used_to_authenticate_with_the_server"
        }
        "Server name is required." => "settings.extra.server_name_is_required",
        "URL is required." => "settings.extra.url_is_required",
        "Invalid URL in settings." => "settings.extra.invalid_url_in_settings",
        "Timeout must be a positive whole number of seconds." => {
            "settings.extra.timeout_must_be_a_positive_whole_number_of_seconds"
        }
        "Each entry is an exact domain (github.com) or a leading-*. subdomain wildcard (*.npmjs.org). IP addresses and local domains are not allowed." => {
            "settings.extra.each_entry_is_an_exact_domain_github_com_or_a_leading_subdomain_wildcard_npmjs_org_ip"
        }
        "Each entry must be an absolute path and grants write access to the whole subtree, except protected Git metadata." => {
            "settings.extra.each_entry_must_be_an_absolute_path_and_grants_write_access_to_the_whole_subtree_except"
        }
        "Enable Sandbox" => "settings.extra.enable_sandbox",
        "Wrap agent-run terminal commands in an OS-level sandbox. When off, commands run with Zed's own permissions." => {
            "settings.extra.wrap_agent_run_terminal_commands_in_an_os_level_sandbox_when_off_commands_run_with_zed"
        }
        "Learn more about sandboxing" => "settings.extra.learn_more_about_sandboxing",
        "Dismiss" => "settings.extra.dismiss",
        "Allow All Domains" => "settings.extra.allow_all_domains",
        "Let sandboxed commands reach any domain over the network without prompting." => {
            "settings.extra.let_sandboxed_commands_reach_any_domain_over_the_network_without_prompting"
        }
        "Allowed Domains" => "settings.extra.allowed_domains",
        "File System" => "settings.extra.file_system",
        "Allow All File System Writes" => "settings.extra.allow_all_file_system_writes",
        "Let sandboxed commands write anywhere except protected Git metadata without prompting." => {
            "settings.extra.let_sandboxed_commands_write_anywhere_except_protected_git_metadata_without_prompting"
        }
        "Writable Paths" => "settings.extra.writable_paths",
        "Escalation Prompts" => "settings.extra.escalation_prompts",
        "Warn About Confusable Unicode" => "settings.extra.warn_about_confusable_unicode",
        "Warn when an approval prompt requests a domain or write path that contains potentially confusable Unicode characters, such as homoglyphs (i.e. two symbols that look similar, such as a Cyrillic `а`)" => {
            "settings.extra.warn_when_an_approval_prompt_requests_a_domain_or_write_path_that_contains_potentially_confusable_unicode"
        }
        "Warn About Windows-Drive Grants" => "settings.extra.warn_about_windows_drive_grants",
        "Windows only: warn when a sandbox grant targets a file on a Windows drive (accessed inside WSL via DrvFs). Such grants are enforced through a translated path and their sandbox-integrity guarantees are weaker than files on the Linux distro's own filesystem." => {
            "settings.extra.windows_only_warn_when_a_sandbox_grant_targets_a_file_on_a_windows_drive_accessed_inside"
        }
        "Nothing configured" => "settings.extra.nothing_configured",
        "Remove Domain" => "settings.extra.remove_domain",
        "Add domain (e.g. github.com or *.npmjs.org)…" => {
            "settings.extra.add_domain_e_g_github_com_or_npmjs_org"
        }
        "Remove Path" => "settings.extra.remove_path",
        "Add an absolute path (e.g. /path/to/directory)…" => {
            "settings.extra.add_an_absolute_path_e_g_path_to_directory"
        }
        "Domain cannot be empty." => "settings.extra.domain_cannot_be_empty",
        "IP addresses and local domains aren't allowed; enter a domain like github.com." => {
            "settings.extra.ip_addresses_and_local_domains_aren_t_allowed_enter_a_domain_like_github_com"
        }
        "Wildcards are only allowed as a leading label, e.g. *.github.com." => {
            "settings.extra.wildcards_are_only_allowed_as_a_leading_label_e_g_github_com"
        }
        "Not a valid domain. Use a domain like github.com or *.npmjs.org." => {
            "settings.extra.not_a_valid_domain_use_a_domain_like_github_com_or_npmjs_org"
        }
        "Allow always" => "settings.extra.allow_always",
        "No global skills installed." => "settings.extra.no_global_skills_installed",
        "No project skills found." => "settings.extra.no_project_skills_found",
        "No skills available for this context." => {
            "settings.extra.no_skills_available_for_this_context"
        }
        "Create a Skill" => "settings.extra.create_a_skill",
        "Copy Share Link" => "settings.extra.copy_share_link",
        "Delete Skill" => "settings.extra.delete_skill",
        "Description" => "settings.extra.description",
        "Add skill content…" => "settings.extra.add_skill_content",
        "Body is required." => "settings.extra.body_is_required",
        "Import from URL" => "settings.extra.import_from_url",
        "File exists" => "settings.extra.file_exists",
        "(optional)" => "settings.extra.optional",
        "Fetching and parsing…" => "settings.extra.fetching_and_parsing",
        "Front-matter" => "settings.extra.front_matter",
        "Skill Content" => "settings.extra.skill_content",
        "enabled for all" => "settings.extra.enabled_for_all",
        "Note: custom tool permissions only apply to the Zed native agent and don’t extend to external agents connected through the Agent Client Protocol (ACP)." => {
            "settings.extra.note_custom_tool_permissions_only_apply_to_the_zed_native_agent_and_don_t_extend_to"
        }
        "Commands executed in the terminal" => "settings.extra.commands_executed_in_the_terminal",
        "Patterns are matched against each command in the input. Commands chained with &&, ||, ;, or pipes are split and checked individually." => {
            "settings.extra.patterns_are_matched_against_each_command_in_the_input_commands_chained_with_or_pipes_are_split"
        }
        "Edit File" => "settings.extra.edit_file",
        "File editing operations" => "settings.extra.file_editing_operations",
        "Patterns are matched against the file path being edited." => {
            "settings.extra.patterns_are_matched_against_the_file_path_being_edited"
        }
        "Write File" => "settings.extra.write_file",
        "File creation and overwrite operations" => {
            "settings.extra.file_creation_and_overwrite_operations"
        }
        "Patterns are matched against the file path being written." => {
            "settings.extra.patterns_are_matched_against_the_file_path_being_written"
        }
        "Delete Path" => "settings.extra.delete_path",
        "File and directory deletion" => "settings.extra.file_and_directory_deletion",
        "Patterns are matched against the path being deleted." => {
            "settings.extra.patterns_are_matched_against_the_path_being_deleted"
        }
        "Copy Path" => "settings.extra.copy_path",
        "File and directory copying" => "settings.extra.file_and_directory_copying",
        "Patterns are matched independently against the source path and the destination path. Enter either path below to test." => {
            "settings.extra.patterns_are_matched_independently_against_the_source_path_and_the_destination_path_enter_either_path_below"
        }
        "Move Path" => "settings.extra.move_path",
        "File and directory moves/renames" => "settings.extra.file_and_directory_moves_renames",
        "Create Directory" => "settings.extra.create_directory",
        "Directory creation" => "settings.extra.directory_creation",
        "Patterns are matched against the directory path being created." => {
            "settings.extra.patterns_are_matched_against_the_directory_path_being_created"
        }
        "Fetch" => "settings.extra.fetch",
        "HTTP requests to URLs" => "settings.extra.http_requests_to_urls",
        "Patterns are matched against the URL being fetched." => {
            "settings.extra.patterns_are_matched_against_the_url_being_fetched"
        }
        "Web Search" => "settings.extra.web_search",
        "Web search queries" => "settings.extra.web_search_queries",
        "Patterns are matched against the search query." => {
            "settings.extra.patterns_are_matched_against_the_search_query"
        }
        "Skill" => "settings.extra.skill",
        "Loading agent skill instructions" => "settings.extra.loading_agent_skill_instructions",
        "Patterns are matched against the absolute path to the skill's SKILL.md file." => {
            "settings.extra.patterns_are_matched_against_the_absolute_path_to_the_skill_s_skill_md_file"
        }
        "Always Deny" => "settings.extra.always_deny",
        "If any of these regexes match, the tool action will be denied." => {
            "settings.extra.if_any_of_these_regexes_match_the_tool_action_will_be_denied"
        }
        "Always Allow" => "settings.extra.always_allow",
        "If any of these regexes match, the action will be approved—unless an Always Confirm or Always Deny matches." => {
            "settings.extra.if_any_of_these_regexes_match_the_action_will_be_approved_unless_an_always_confirm_or"
        }
        "Always Confirm" => "settings.extra.always_confirm",
        "If any of these regexes match, a confirmation will be shown unless an Always Deny regex matches." => {
            "settings.extra.if_any_of_these_regexes_match_a_confirmation_will_be_shown_unless_an_always_deny_regex"
        }
        "Enter a tool input to test your rules…" => {
            "settings.extra.enter_a_tool_input_to_test_your_rules"
        }
        "Test Your Rules" => "settings.extra.test_your_rules",
        "No regex matches, using the default action." => {
            "settings.extra.no_regex_matches_using_the_default_action"
        }
        "Result:" => "settings.extra.result",
        "Invalid Patterns" => "settings.extra.invalid_patterns",
        "Delete Invalid Pattern" => "settings.extra.delete_invalid_pattern",
        "No patterns configured" => "settings.extra.no_patterns_configured",
        "Delete Pattern" => "settings.extra.delete_pattern",
        "Default Permission" => "settings.extra.default_permission",
        "Allow" => "settings.extra.allow",
        "Deny" => "settings.extra.deny",
        "Default Action" => "settings.extra.default_action",
        "Action to take when no patterns match." => {
            "settings.extra.action_to_take_when_no_patterns_match"
        }
        "Controls the default behavior for all tool actions. Per-tool rules and patterns can override this." => {
            "settings.extra.controls_the_default_behavior_for_all_tool_actions_per_tool_rules_and_patterns_can_override_this"
        }
        "Pattern preview differs from engine — showing authoritative result." => {
            "settings.extra.pattern_preview_differs_from_engine_showing_authoritative_result"
        }
        "These patterns failed to compile as regular expressions. The tool will be blocked until they are fixed or removed." => {
            "settings.extra.these_patterns_failed_to_compile_as_regular_expressions_the_tool_will_be_blocked_until_they_are"
        }
        "`rm -rf` commands are always blocked when run on `$HOME`, `~`, `.`, `..`, or `/`" => {
            "settings.extra.rm_rf_commands_are_always_blocked_when_run_on_home_or"
        }
        "Saving…" => "settings.extra.saving",
        "Save Skill" => "settings.extra.save_skill",
        "e.g., Fill the PR description following this template." => {
            "settings.extra.e_g_fill_the_pr_description_following_this_template"
        }
        "Paste a GitHub .md URL to fetch it and fill out the form. For private files, Zed retries using GITHUB_TOKEN, if set." => {
            "settings.extra.paste_a_github_md_url_to_fetch_it_and_fill_out_the_form_for_private_files"
        }
        "Low" => "settings.extra.low",
        "Medium" => "settings.extra.medium",
        "High" => "settings.extra.high",
        "Minimal" => "settings.extra.minimal",
        "Extra High" => "settings.extra.extra_high",
        "Denied: {reason}" => "settings.extra.denied_reason",
        "Reason: {reason}" => "settings.extra.reason_reason",
        "Error: {error}" => "settings.extra.error_error",
        "{count} rules" => "settings.extra.count_rules",
        "1 rule" => "settings.extra.1_rule",
        "{count} invalid" => "settings.extra.count_invalid",
        "To reset your API key, unset the {variable} environment variable." => {
            "settings.extra.to_reset_your_api_key_unset_the_variable_environment_variable"
        }
        "Or set the {variable} env var and restart Zed." => {
            "settings.extra.or_set_the_variable_env_var_and_restart_zed"
        }
        "Or set the {variable} env var and restart Zed for it to take effect." => {
            "settings.extra.or_set_the_variable_env_var_and_restart_zed_for_it_to_take_effect"
        }
        "Failed to load your settings. Some values may be incorrect and changes may be lost." => {
            "settings.extra.failed_to_load_your_settings_some_values_may_be_incorrect_and_changes_may_be_lost"
        }
        "Your settings are out of date, and need to be updated." => {
            "settings.extra.your_settings_are_out_of_date_and_need_to_be_updated"
        }
        "They can be automatically migrated to the latest version." => {
            "settings.extra.they_can_be_automatically_migrated_to_the_latest_version"
        }
        "They must be manually migrated to the latest version." => {
            "settings.extra.they_must_be_manually_migrated_to_the_latest_version"
        }
        "Your settings file is out of date, automatic migration failed" => {
            "settings.extra.your_settings_file_is_out_of_date_automatic_migration_failed"
        }
        _ => return source.to_owned().into(),
    };
    text(key, cx).into()
}
