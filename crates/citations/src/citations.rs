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
mod keys;
mod library;
mod zotero;

pub use bibliography::{
    BibEntry, Bibliography, CitationCompletionProvider, CitationSemanticsProvider, citation_key_at,
    citation_key_start,
};
pub use keys::{disambiguate, is_portable_key, mint_key};
pub use library::{DEFAULT_LIBRARY_PATH, Merge, append_entry, merge_entry};
pub use zotero::{DEFAULT_BASE_URL, ZoteroClient, ZoteroError, ZoteroItem, ZoteroStatus};

pub(crate) const MARKDOWN: &str = "Markdown";
