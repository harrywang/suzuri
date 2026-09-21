---
title: Suzuri Display Language
description: "Choose English or Simplified Chinese for Suzuri's menus, welcome screens, and settings."
---

# Suzuri Display Language {#suzuri-display-language}

Open **Suzuri > Open Settings** on Windows/Linux, or **Suzuri > Settings > Open Settings** on macOS, then **General > General Settings**.
Find **Language / 界面语言** and choose **English** or **简体中文**. Restart Suzuri
when prompted to apply the selection. English is the default.

中文操作：Windows/Linux 打开 **Suzuri > Open Settings**（macOS 位于 **Settings** 子菜单），在 **General** 页面的
**Language / 界面语言** 中选择 **简体中文**，然后点击重启提示。切换后，相同入口显示为
**Suzuri > 打开设置**（macOS 为 **Suzuri > 设置 > 打开设置**），设置页显示为 **通用**。要恢复英文，选择 **English** 并重启。

You can also open the Settings Editor with {#action zed::OpenSettings} and search
for `ui_language` or `界面语言`.

Or add this to your user settings.json:

```json [settings]
{
  "ui_language": "zh-CN"
}
```

Use `"en"` to select English. This is an application preference; changing a
project's settings does not change the interface language.

## Coverage {#localization-coverage}

This first version translates the main application menus, their commands and
submenus, welcome and initial setup screens, settings navigation and groups,
fixed titles and descriptions throughout the Settings Editor, dropdown choices,
and built-in configuration forms for providers, agents, MCP servers and tool permissions,
the language-change restart notification, and title-bar update controls. Missing or empty translations fall back to English.

Provider-supplied embedded views, dynamic error details, command palette search
results, editor context menus, and some dialogs may remain in English. Operating-system file dialogs use
the system's language. On macOS, the native **Window** menu keeps its English
title so that AppKit can recognize it; its application-provided commands are
translated.

Changing the preference takes effect after a restart. Opening another window
or changing key bindings before restarting does not change the session's
language. Existing shortcuts continue to work in either language.

Your selection is saved in your user settings. Replacing or rebuilding the
executable keeps that preference; English remains the default for new profiles.

Settings search accepts both translated labels and the original English labels
and JSON setting names. Configuration values and action identifiers are unchanged.

## Language packs {#language-packs}

English and Simplified Chinese are bundled with Suzuri. You do not need to
install an extension. Community language-pack installation is not available in
this version. See the [localization development plan](./development/suzuri-localization.md)
for the proposed path toward separately distributed language packs.
