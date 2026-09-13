//! Citations for the research vault: the `refs/refs.bib` library, the keys
//! Suzuri mints for it, the Zotero client that feeds it, and the editor
//! index that resolves `[@key]` against it.
//!
//! The vault owns its bibliography. Every other Zotero integration keeps an
//! exported `.bib` in sync with the whole library and lets an add-on choose
//! the keys; both halves are where their users' complaints come from (stale
//! exports, drifting keys). Here an entry is pulled from Zotero once, at
//! insert time, under a key this crate chose and will never change.

mod bibliography;
mod insert_citation;
mod keys;
mod library;
mod zotero;

pub use bibliography::{
    BibEntry, Bibliography, CitationCompletionProvider, CitationSemanticsProvider, citation_key_at,
    citation_key_start,
};
pub use insert_citation::{CitationFormat, CitationPicker, InsertCitation, choose_key};
pub use keys::{disambiguate, is_portable_key, mint_key};
pub use library::{DEFAULT_LIBRARY_PATH, Merge, append_entry, merge_entry};
pub use zotero::{DEFAULT_BASE_URL, ZoteroClient, ZoteroError, ZoteroItem, ZoteroStatus};

pub(crate) const MARKDOWN: &str = "Markdown";

/// User-facing settings, read from the `citations` section of settings.json.
#[derive(Clone, Debug, settings::RegisterSetting)]
pub struct CitationsSettings {
    /// The BibLaTeX file inserted citations are recorded in, relative to the
    /// project root.
    pub library: String,
    /// Where Zotero's local API listens.
    pub zotero_url: String,
}

impl settings::Settings for CitationsSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.citations.clone().unwrap_or_default();
        Self {
            library: content
                .library
                .filter(|library| !library.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_LIBRARY_PATH.to_string()),
            zotero_url: content
                .zotero_url
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        }
    }
}

pub fn init(cx: &mut gpui::App) {
    insert_citation::init(cx);
}
