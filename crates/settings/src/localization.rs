use std::{collections::BTreeMap, sync::LazyLock};

use gpui::{App, Global, SharedString};

use crate::{self as settings, RegisterSetting, Settings, SettingsContent, UiLanguage};

#[derive(Clone, Copy, Debug, RegisterSetting)]
pub struct UiLanguageSetting(pub UiLanguage);

impl Settings for UiLanguageSetting {
    fn from_settings(content: &SettingsContent) -> Self {
        Self(content.ui_language.unwrap_or_default())
    }
}

#[derive(Clone, Copy)]
struct ActiveUiLanguage(UiLanguage);

impl Global for ActiveUiLanguage {}

/// Call after user settings have loaded, before constructing any menus or windows.
pub fn init(cx: &mut App) {
    cx.set_global(ActiveUiLanguage(UiLanguageSetting::get_global(cx).0));
}

pub fn active_language(cx: &App) -> UiLanguage {
    // Keep a single language for the session, including menus rebuilt by keymap changes.
    cx.try_global::<ActiveUiLanguage>()
        .map(|language| language.0)
        .unwrap_or_default()
}

type Catalog = BTreeMap<String, String>;

const ENGLISH_JSON: &str = include_str!("../../../assets/locales/en.json");
const CHINESE_JSON: &str = include_str!("../../../assets/locales/zh-CN.json");

static ENGLISH: LazyLock<Catalog> = LazyLock::new(|| load_catalog(ENGLISH_JSON));
static CHINESE: LazyLock<Catalog> = LazyLock::new(|| load_catalog(CHINESE_JSON));

fn load_catalog(source: &str) -> Catalog {
    match serde_json::from_str(source) {
        Ok(catalog) => catalog,
        Err(error) => {
            log::error!("Failed to load bundled UI translations: {error}");
            Catalog::default()
        }
    }
}

fn lookup<'a>(key: &'a str, translated: &'a Catalog, english: &'a Catalog) -> &'a str {
    translated
        .get(key)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| english.get(key))
        .map(String::as_str)
        .unwrap_or(key)
}

pub fn text_for(language: UiLanguage, key: &'static str) -> &'static str {
    let translated = match language {
        UiLanguage::English => &*ENGLISH,
        UiLanguage::SimplifiedChinese => &*CHINESE,
    };
    lookup(key, translated, &ENGLISH)
}

pub fn text(key: &'static str, cx: &App) -> &'static str {
    text_for(active_language(cx), key)
}

/// Resolve canonical menu names used by existing key bindings to their visible labels.
pub fn menu_name(name: &str, cx: &App) -> SharedString {
    let key = match name {
        // Default keymaps and existing user keymaps still use the upstream app name.
        "Zed" => return "Suzuri".into(),
        "File" => "menu.file",
        "Edit" => "menu.edit",
        "Selection" => "menu.selection",
        "View" => "menu.view",
        "Go" => "menu.go",
        "Run" => "menu.run",
        "Window" => "menu.window",
        "Help" => "menu.help",
        _ => return name.to_owned().into(),
    };
    text(key, cx).into()
}

#[cfg(test)]
mod tests {
    use gpui::UpdateGlobal;

    use super::*;

    #[test]
    fn missing_and_blank_translations_fall_back_to_english() {
        let english = Catalog::from([("save".into(), "Save".into())]);
        assert_eq!(lookup("save", &Catalog::new(), &english), "Save");
        let blank = Catalog::from([("save".into(), "  ".into())]);
        assert_eq!(lookup("save", &blank, &english), "Save");
        assert_eq!(lookup("unknown", &blank, &english), "unknown");
    }

    #[test]
    fn bundled_catalogs_have_matching_nonempty_keys() {
        let english: Catalog = serde_json::from_str(ENGLISH_JSON).expect("valid English catalog");
        let chinese: Catalog = serde_json::from_str(CHINESE_JSON).expect("valid Chinese catalog");
        assert!(!english.is_empty());
        assert_eq!(
            english.keys().collect::<Vec<_>>(),
            chinese.keys().collect::<Vec<_>>()
        );
        for (key, value) in &chinese {
            assert!(!value.trim().is_empty(), "empty translation: {key}");
        }
        assert_eq!(text_for(UiLanguage::English, "menu.file"), "File");
        assert_eq!(text_for(UiLanguage::SimplifiedChinese, "menu.file"), "文件");
    }

    #[test]
    fn language_setting_round_trips_and_rejects_unsupported_locales() {
        for (tag, language) in [
            ("en", UiLanguage::English),
            ("zh-CN", UiLanguage::SimplifiedChinese),
        ] {
            let source = format!(r#"{{"ui_language":"{tag}"}}"#);
            let content: SettingsContent = serde_json::from_str(&source).expect("valid settings");
            assert_eq!(UiLanguageSetting::from_settings(&content).0, language);
            assert_eq!(
                serde_json::to_value(&content).expect("serializable settings")["ui_language"],
                tag
            );
        }
        assert!(serde_json::from_str::<UiLanguage>(r#""unsupported""#).is_err());
        assert_eq!(
            UiLanguageSetting::from_settings(&SettingsContent::default()).0,
            UiLanguage::English
        );
    }

    #[gpui::test]
    fn language_changes_require_restart_and_preserve_menu_shortcuts(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::init(cx);
            crate::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"ui_language":"zh-CN"}"#, cx)
                    .expect("valid language setting");
            });
            init(cx);
            assert_eq!(menu_name("File", cx), "文件");
            assert_eq!(menu_name("文件", cx), "文件");
            assert_eq!(menu_name("Suzuri", cx), "Suzuri");
            assert_eq!(menu_name("Zed", cx), "Suzuri");
            crate::SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{"ui_language":"en"}"#, cx)
                    .expect("valid language setting");
            });
            assert_eq!(text("menu.file", cx), "文件");
            init(cx);
            assert_eq!(text("menu.file", cx), "File");
            assert_eq!(menu_name("Zed", cx), "Suzuri");
        });
    }
}
