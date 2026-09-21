# Suzuri Localization and Language-Pack Plan {#suzuri-localization-plan}

## First version {#localization-first-version}

The first version bundles English and Simplified Chinese and exposes a
user-level `ui_language` preference. It covers application menus, welcome and initial setup screens, settings
navigation, fixed row titles and descriptions, dropdown choices, built-in configuration forms,
and title-bar update controls. The preference is captured after startup settings are loaded;
changes require a restart. The restart notification uses the newly selected
language, while existing windows keep their session language.

Translation resources live in `assets/locales/en.json` and
`assets/locales/zh-CN.json`. Semantic keys such as `menu.check_updates` identify
messages independently of their wording. `settings::localization` provides
lookup and falls back to English for missing or blank translations. Keep keys
stable when editing a translation. New UI text requires an English entry and
an explicit call site; adding a language file alone does not translate more UI.

`UiLanguage` is currently a closed enum with two variants. This intentionally
keeps the first version small. Supporting arbitrary installed locales will
require replacing that enum and the fixed catalog loader with a locale registry;
the current implementation does not claim to provide a plugin API.

Canonical menu names used by existing `app_menu::OpenApplicationMenu` key
bindings are resolved to translated labels. Action identifiers and shortcut
definitions do not change. AppKit currently identifies its native Window menu
by the literal English name; keep that title until GPUI supports a separate
semantic menu identifier.

Settings navigation retains its original identifiers and deep-link targets.
Translate labels only when rendering, and index Chinese aliases alongside
English labels and JSON paths. User data and stored option values are not translated.

## Proposed next stages {#localization-next-stages}

The stages below are proposals for maintainer discussion, not implemented
features or a release-date commitment.

### 1. Broader built-in coverage {#localization-coverage-expansion}

Expand translation to provider-supplied views, remaining dialogs, command palette labels and search,
file operations, and common notifications in small reviewed changes. Preserve
English command search aliases and stable action identifiers. Introduce full
message templates with validated placeholders and plural rules before expanding
to messages whose grammar differs by language. Review layout and accessibility
on Windows, macOS, and Linux.

### 2. Data-only language-pack format {#localization-pack-format}

Define a versioned manifest with a pack identifier, locale tag, native display
name, author, license, supported catalog/API versions, and translation files.
Use the bundled English catalog as the canonical key set. Validate resource
types, duplicate keys, placeholders, file sizes, and compatibility before
loading a pack. Language packs should contain data only and require no code
execution or network permissions of their own.

Decide fallback order explicitly: a selected compatible community pack, then
any bundled catalog for that locale, then English. Keep a built-in English
selection available when a pack fails validation or is removed. Existing
`en` and `zh-CN` preferences must continue to work after migrating to a registry.

### 3. Community plugin distribution {#localization-plugin-distribution}

After agreeing on the format, extend Suzuri's extension manifest and host with
a language-pack contribution type. Reuse extension installation, updates, and
removal where suitable, and derive the language picker from the installed
locale registry. Package the existing Chinese resources as a reference pack
while retaining bundled English and Chinese for the initial target audience.

Agree with maintainers on whether packs use an upstream-compatible registry or
a Suzuri-specific distribution channel. Existing Zed extension infrastructure
does not currently expose this contribution type, so publishing an ordinary
extension is not sufficient. Resolve ownership, review, version compatibility,
conflicting packs for the same locale, and rollback before enabling distribution.

### 4. Community translation maintenance {#localization-community-maintenance}

Publish a terminology guide, contributor instructions, and coverage reports.
Automate missing-key, placeholder, and compatibility checks. Add a translation
platform only if contribution volume justifies it. Review upstream merges for
new or changed UI text; plugin packaging alone does not eliminate that work.

## Validation {#localization-validation}

With the repository's Rust toolchain and native build prerequisites installed:

```sh
node script/check-settings-localization.mjs
cargo test -p settings localization::tests
cargo test -p title_bar update_tooltips_follow_the_session_language
cargo test -p zed translated_menus_preserve_actions
cargo check -p zed
```

Manual acceptance checks:

1. Start with no `ui_language` preference and confirm English menus.
2. Select 简体中文 in the Settings Editor. Confirm a Chinese restart prompt.
3. Open a second window before restarting: both windows still use English.
4. Restart and check translated File, Edit, Settings, and update controls.
5. On Windows, exercise the existing menu shortcuts and arrow navigation.
6. Verify that open, save, close, and update commands still invoke the same actions.
7. Switch back to English, restart, and confirm the selection persists.
8. Change the preference and then revert it before restarting; the restart
   notification should disappear.
9. Check narrow windows, high display scaling, and screen-reader labels.
10. Confirm project settings cannot override the application language and that
    an invalid language value is reported through normal settings validation.

11. Check welcome and initial setup screens in both languages.
12. Search settings using Chinese labels, English labels, and JSON paths; verify
    navigation links and saved values still target the same setting.
