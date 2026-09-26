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
mod open_source;
mod render;
mod zotero;

pub use bibliography::{
    BibEntry, Bibliography, CitationCompletionProvider, CitationSemanticsProvider, citation_key_at,
    citation_key_start,
};
pub use insert_citation::{CitationFormat, CitationPicker, InsertCitation, choose_key};
pub use keys::{disambiguate, is_portable_key, mint_key};
pub use library::{DEFAULT_LIBRARY_PATH, Merge, append_entry, merge_entry};
pub use open_source::{OpenSource, source_for_cursor};
pub use render::{
    CiteForm, CiteItem, CiteSuffix, CslStyle, DEFAULT_STYLE, Locator, ParsedStyle,
    RenderedDocument, RenderedReference, ResolvedStyle, STYLE_LIST_URL, StyleFileState, StyleFiles,
    StyleProblem, StyleSource, document_style, document_style_source, parse_cite_suffix,
    render_bibliography, render_document, render_reference, resolve_bundled, resolve_style,
    resolve_style_readonly, style_file_candidates, style_from_xml, style_named, style_names,
    style_or_default,
};

/// The folders a note's relative paths (a `csl:` file) resolve against: the
/// note's own folder, then its worktree root.
pub fn style_search_dirs(buffer: &language::Buffer, cx: &gpui::App) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    let Some(file) = buffer.file() else {
        return dirs;
    };
    let Some(local) = file.as_local() else {
        return dirs;
    };
    let absolute = local.abs_path(cx);
    if let Some(parent) = absolute.parent() {
        dirs.push(parent.to_path_buf());
    }
    // The worktree root is the absolute path with the worktree-relative one
    // peeled off.
    let depth = file.path().components().count();
    let mut root = absolute.as_path();
    for _ in 0..depth {
        match root.parent() {
            Some(parent) => root = parent,
            None => break,
        }
    }
    if !dirs.iter().any(|dir| dir == root) {
        dirs.push(root.to_path_buf());
    }
    dirs
}
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
    open_source::init(cx);
}
