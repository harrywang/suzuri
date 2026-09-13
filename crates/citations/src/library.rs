//! The vault's own bibliography, `refs/refs.bib`, and how an entry gets in.
//!
//! The file is append-only from Suzuri's side. Entries already there are
//! never reserialized, so a hand-edited entry keeps its formatting and a
//! key that is cited somewhere is never rewritten.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use fs::Fs;

/// Where the library lives, relative to the worktree root.
pub const DEFAULT_LIBRARY_PATH: &str = "refs/refs.bib";

#[derive(Debug, PartialEq, Eq)]
pub enum Merge {
    /// The library text with the entry appended.
    Written(String),
    /// `key` was already in the library; nothing to write.
    AlreadyPresent,
}

/// Merges one BibLaTeX entry, renamed to `key`, into `existing`. Refuses
/// rather than clobbers when the existing text does not parse: a library
/// mid-edit is not a reason to lose it.
pub fn merge_entry(existing: &str, entry_source: &str, key: &str) -> Result<Merge> {
    let library = biblatex::Bibliography::parse(existing)
        .map_err(|error| anyhow!("{error}"))
        .context("parsing the vault library")?;
    if library.get(key).is_some() {
        return Ok(Merge::AlreadyPresent);
    }
    let incoming = biblatex::Bibliography::parse(entry_source)
        .map_err(|error| anyhow!("{error}"))
        .context("parsing the fetched entry")?;
    let mut entry = incoming
        .into_iter()
        .next()
        .context("the fetched BibLaTeX contains no entry")?;
    entry.key = key.to_string();
    // Zotero's export names the attachment by its absolute path on this
    // machine; the vault keeps its own copy as `refs/<key>.pdf` instead.
    entry.fields.remove("file");

    let mut text = existing.trim_end().to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&entry.to_biblatex_string());
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(Merge::Written(text))
}

/// Appends the entry to the library at `path`, creating the file and its
/// directory on first use. Returns whether anything was written.
pub async fn append_entry(
    fs: &Arc<dyn Fs>,
    path: &Path,
    entry_source: &str,
    key: &str,
) -> Result<bool> {
    let existing = if fs.is_file(path).await {
        fs.load(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    match merge_entry(&existing, entry_source, key)? {
        Merge::AlreadyPresent => Ok(false),
        Merge::Written(text) => {
            if let Some(parent) = path.parent()
                && !fs.is_dir(parent).await
            {
                fs.create_dir(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            // Not `atomic_write`: that goes through a private temp file and
            // leaves the library at mode 0600, which surprises anything else
            // that shares the vault.
            fs.write(path, text.as_bytes())
                .await
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FETCHED: &str = "@article{zoteroKey_2017,\n  title = {Attention Is All You Need},\n  author = {Vaswani, Ashish},\n  date = {2017-06-12},\n  file = {PDF:/Users/someone/Zotero/storage/AB12/paper.pdf:application/pdf},\n}\n";

    #[test]
    fn appends_under_the_minted_key_and_leaves_existing_text_alone() {
        let existing = "@book{knuth1984,\n  title = {The TeXbook},   % keep my odd spacing\n}\n";
        let Merge::Written(text) = merge_entry(existing, FETCHED, "vaswani2017attention").unwrap()
        else {
            panic!("expected a write");
        };
        assert!(
            text.starts_with(existing.trim_end()),
            "existing text was rewritten:\n{text}"
        );
        assert!(text.contains("@article{vaswani2017attention,"));
        assert!(!text.contains("zoteroKey_2017"));
        assert!(
            !text.contains("file ="),
            "the machine-local attachment path is dropped:\n{text}"
        );
        assert!(text.ends_with('\n'));
        let parsed = biblatex::Bibliography::parse(&text).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn starts_an_empty_library_without_leading_blank_lines() {
        let Merge::Written(text) = merge_entry("", FETCHED, "vaswani2017attention").unwrap() else {
            panic!("expected a write");
        };
        assert!(text.starts_with("@article{vaswani2017attention,"));
    }

    #[test]
    fn a_present_key_is_not_written_twice() {
        let existing = "@article{vaswani2017attention,\n  title = {x},\n}\n";
        assert_eq!(
            merge_entry(existing, FETCHED, "vaswani2017attention").unwrap(),
            Merge::AlreadyPresent
        );
    }

    #[test]
    fn a_library_that_does_not_parse_is_not_clobbered() {
        let error = merge_entry("@article{unclosed,", FETCHED, "k").unwrap_err();
        assert!(
            error.to_string().contains("parsing the vault library"),
            "{error:#}"
        );
    }
}
