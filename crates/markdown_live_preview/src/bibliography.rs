//! Bibliography index backing Pandoc-style `[@key]` citations.
//!
//! Every `.bib` file in the project's worktrees is parsed (off the main
//! thread, via `biblatex`) into a process-wide [`Bibliography`] entity. Two
//! consumers hang off it: [`CitationCompletionProvider`] serves cite-key
//! completions when the cursor sits in an `@key` context, and
//! `apply_emphasis_highlights` styles keys that resolve to no entry as
//! unresolved — but only once at least one entry exists, so vaults that never
//! use a bibliography see no red ink.
//!
//! The index is global rather than per-editor because `.bib` files are shared
//! state: every open markdown editor should agree on whether a key resolves,
//! and a file should be parsed once, not once per editor.

use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use collections::{HashMap, HashSet};
use editor::{CompletionProvider, Editor, SemanticsProvider};
use gpui::{App, AppContext as _, Context, Entity, EntityId, SharedString, Task, Window};
use language::{Buffer, CodeLabel, LanguageName, ToOffset as _};
use project::{
    Completion, CompletionDisplayOptions, CompletionResponse, CompletionSource, Project,
    lsp_store::CompletionDocumentation,
};
use util::ResultExt as _;

/// One entry parsed out of a `.bib` file, reduced to the fields the citation
/// pipeline shows: enough to pick the right entry from a completion menu and
/// to recognize it on hover, not a full reference model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BibEntry {
    pub key: SharedString,
    pub title: Option<SharedString>,
    pub authors: Option<SharedString>,
    pub year: Option<SharedString>,
    pub entry_type: SharedString,
}

impl BibEntry {
    /// The reference as markdown — bolded title, authors and year, entry
    /// type — shared by the completion card and the hover card.
    fn reference_markdown(&self) -> Option<String> {
        let mut text = String::new();
        if let Some(title) = &self.title {
            text.push_str(&format!("**{title}**"));
        }
        let mut line = String::new();
        if let Some(authors) = &self.authors {
            line.push_str(authors);
        }
        if let Some(year) = &self.year {
            if !line.is_empty() {
                line.push_str(", ");
            }
            line.push_str(year);
        }
        if !line.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&line);
        }
        if text.is_empty() {
            return None;
        }
        text.push_str(&format!("\n\n*{}*", self.entry_type));
        Some(text)
    }

    fn documentation(&self) -> Option<CompletionDocumentation> {
        Some(CompletionDocumentation::MultiLineMarkdown(
            self.reference_markdown()?.into(),
        ))
    }
}

pub struct Bibliography {
    /// Parsed entries per absolute `.bib` path. A file that fails to parse
    /// holds an empty list, which also serves as its tombstone on deletion.
    files: HashMap<PathBuf, Vec<BibEntry>>,
    /// Every key across `files`. The highlight pass resolves each citation
    /// in a note on every keystroke, so membership has to be O(1) rather
    /// than a scan over what may be a Zotero-sized library.
    keys: HashSet<SharedString>,
    /// Projects whose worktrees have already been walked, so each is scanned
    /// once; later changes arrive through `WorktreeUpdatedEntries`.
    scanned_projects: HashSet<EntityId>,
}

struct GlobalBibliography(Entity<Bibliography>);

impl gpui::Global for GlobalBibliography {}

impl Bibliography {
    pub fn global(cx: &mut App) -> Entity<Bibliography> {
        if let Some(global) = cx.try_global::<GlobalBibliography>() {
            return global.0.clone();
        }
        let bibliography = cx.new(|_| Bibliography {
            files: HashMap::default(),
            keys: HashSet::default(),
            scanned_projects: HashSet::default(),
        });
        cx.set_global(GlobalBibliography(bibliography.clone()));
        bibliography
    }

    pub fn has_entries(&self) -> bool {
        self.files.values().any(|entries| !entries.is_empty())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    pub fn resolve(&self, key: &str) -> Option<&BibEntry> {
        self.entries().find(|entry| entry.key.as_ref() == key)
    }

    fn rebuild_keys(&mut self) {
        self.keys = self
            .files
            .values()
            .flatten()
            .map(|entry| entry.key.clone())
            .collect();
    }

    pub fn entries(&self) -> impl Iterator<Item = &BibEntry> {
        self.files.values().flatten()
    }

    /// Walks the project's worktrees for `.bib` files the first time this
    /// project is seen and queues them for parsing.
    pub fn ensure_project(
        bibliography: &Entity<Bibliography>,
        project: &Entity<Project>,
        cx: &mut App,
    ) {
        let newly_seen = bibliography.update(cx, |bibliography, _| {
            bibliography.scanned_projects.insert(project.entity_id())
        });
        if !newly_seen {
            return;
        }
        let mut paths = Vec::new();
        for worktree in project.read(cx).worktrees(cx) {
            let worktree = worktree.read(cx);
            for entry in worktree.entries(false, 0) {
                if entry
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "bib")
                {
                    paths.push(worktree.absolutize(&entry.path));
                }
            }
        }
        Self::reload_paths(bibliography, project, paths, cx);
    }

    /// Loads and parses the given `.bib` paths off the main thread, then
    /// publishes the results so observers restyle. Paths that no longer load
    /// (deleted, unreadable) drop their entries instead.
    pub fn reload_paths(
        bibliography: &Entity<Bibliography>,
        project: &Entity<Project>,
        paths: Vec<PathBuf>,
        cx: &mut App,
    ) {
        if paths.is_empty() {
            return;
        }
        let fs = project.read(cx).fs().clone();
        let bibliography = bibliography.clone();
        cx.spawn(async move |cx| {
            let mut results = Vec::with_capacity(paths.len());
            for path in paths {
                let entries = match fs.load(&path).await {
                    Ok(source) => cx.background_spawn(async move { parse_bib(&source) }).await,
                    Err(_) => Vec::new(),
                };
                results.push((path, entries));
            }
            bibliography.update(cx, |bibliography, cx| {
                for (path, entries) in results {
                    bibliography.files.insert(path, entries);
                }
                bibliography.rebuild_keys();
                cx.notify();
            })
        })
        .detach();
    }

    pub fn remove_path(bibliography: &Entity<Bibliography>, path: &Path, cx: &mut App) {
        bibliography.update(cx, |bibliography, cx| {
            if bibliography.files.remove(path).is_some() {
                bibliography.rebuild_keys();
                cx.notify();
            }
        });
    }
}

/// Reduces a BibTeX/BibLaTeX source to the entries' displayable fields. A
/// source that fails to parse yields nothing: while the user is mid-edit on
/// their `.bib`, flagging every citation in every note as unresolved would be
/// noise, so the previous parse (if any) simply goes stale until the file
/// parses again.
fn parse_bib(source: &str) -> Vec<BibEntry> {
    use biblatex::ChunksExt as _;

    let Some(parsed) = biblatex::Bibliography::parse(source).log_err() else {
        return Vec::new();
    };
    parsed
        .iter()
        .map(|entry| {
            let title = entry
                .title()
                .ok()
                .map(|chunks| collapse_whitespace(&chunks.format_verbatim()))
                .filter(|title| !title.is_empty());
            let authors = entry.author().ok().map(|people| {
                people
                    .iter()
                    .map(|person| {
                        let mut name = String::new();
                        if !person.given_name.is_empty() {
                            name.push_str(&person.given_name);
                            name.push(' ');
                        }
                        name.push_str(&person.name);
                        name
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            let year = entry
                .get("year")
                .or_else(|| entry.get("date"))
                .map(|chunks| chunks.format_verbatim())
                .and_then(|value| first_year(&value));
            BibEntry {
                key: SharedString::from(entry.key.clone()),
                title: title.map(SharedString::from),
                authors: authors
                    .filter(|authors| !authors.is_empty())
                    .map(SharedString::from),
                year: year.map(SharedString::from),
                entry_type: SharedString::from(entry.entry_type.to_string()),
            }
        })
        .collect()
}

/// `.bib` titles often carry the file's own line wrapping; a completion
/// menu wants them on one line.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First run of four consecutive digits, so both `2024` and `2024-03-01`
/// (BibLaTeX `date`) yield a year.
fn first_year(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut run_start = None;
    for (index, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_digit() {
            let start = *run_start.get_or_insert(index);
            if index - start + 1 == 4 {
                return Some(value[start..=index].to_string());
            }
        } else {
            run_start = None;
        }
    }
    None
}

/// Wraps the editor's stock (project-backed) completion provider, serving
/// cite keys when the cursor sits in a Pandoc `@key` context in a markdown
/// buffer and delegating everything else untouched — LSP completions from
/// markdown-oxide and friends keep working.
pub struct CitationCompletionProvider {
    inner: Rc<dyn CompletionProvider>,
    bibliography: Entity<Bibliography>,
}

impl CitationCompletionProvider {
    pub fn new(project: Entity<Project>, cx: &mut App) -> Self {
        Self {
            inner: Rc::new(project),
            bibliography: Bibliography::global(cx),
        }
    }
}

/// Where the key being typed starts (just after the `@`), if `offset` sits in
/// a citation context: an `@` preceded by a Pandoc citation boundary, with
/// nothing but key characters between it and the cursor. Mirrors the
/// tolerances of `citation_keys`, which decides what ultimately renders as a
/// citation chip.
fn is_citation_key_char(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '_' | ':' | '.' | '#' | '$' | '%' | '&' | '-' | '+' | '?' | '<' | '>' | '~' | '/'
        )
}

/// Whether a citation boundary precedes the `@` the reversed iterator has
/// stopped on: start of text, whitespace, a group separator, or `[`.
fn citation_boundary(before_at: Option<char>) -> bool {
    match before_at {
        None => true,
        Some(character) => character.is_whitespace() || matches!(character, ';' | '-' | '['),
    }
}

pub(crate) fn citation_key_start(buffer: &Buffer, offset: usize) -> Option<usize> {
    let mut walked = 0;
    let mut characters = buffer.reversed_chars_at(offset);
    loop {
        let character = characters.next()?;
        if character == '@' {
            break;
        }
        if !is_citation_key_char(character) || walked > 128 {
            return None;
        }
        walked += character.len_utf8();
    }
    citation_boundary(characters.next()).then_some(offset - walked)
}

/// The citation key containing `offset`, if any: its full range including
/// the `@`, plus the key text without it. Bracketed and bare citations look
/// identical from here — an `@` after a boundary followed by key characters.
pub(crate) fn citation_key_at(
    buffer: &Buffer,
    offset: usize,
) -> Option<(std::ops::Range<usize>, SharedString)> {
    let mut start = offset;
    let mut characters = buffer.reversed_chars_at(offset);
    let mut at_found = false;
    while let Some(character) = characters.next() {
        if character == '@' {
            at_found = true;
            start -= character.len_utf8();
            break;
        }
        if !is_citation_key_char(character) || offset - start > 128 {
            return None;
        }
        start -= character.len_utf8();
    }
    if !at_found || !citation_boundary(characters.next()) {
        return None;
    }
    let mut end = offset;
    for character in buffer.chars_at(offset) {
        if !is_citation_key_char(character) || end - start > 256 {
            break;
        }
        end += character.len_utf8();
    }
    let text: String = buffer.text_for_range(start..end).collect();
    let key = text.strip_prefix('@')?;
    // Punctuation is only valid inside a key, not at its edges, matching
    // `citation_keys`.
    let key = key
        .trim_end_matches(|character: char| !(character.is_ascii_alphanumeric() || character == '_'));
    if key.is_empty()
        || !key
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let end = start + 1 + key.len();
    if offset > end {
        return None;
    }
    Some((start..end, SharedString::from(key.to_string())))
}

fn buffer_is_markdown(buffer: &Buffer) -> bool {
    buffer
        .language()
        .is_some_and(|language| language.name() == LanguageName::new(crate::MARKDOWN))
}

impl CompletionProvider for CitationCompletionProvider {
    fn completions(
        &self,
        buffer: &Entity<Buffer>,
        buffer_position: text::Anchor,
        trigger: editor::CompletionContext,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<Vec<CompletionResponse>>> {
        let citation = {
            let buffer = buffer.read(cx);
            if buffer_is_markdown(buffer) {
                let offset = buffer_position.to_offset(buffer);
                citation_key_start(buffer, offset)
                    .map(|key_start| buffer.anchor_before(key_start)..buffer_position)
            } else {
                None
            }
        };
        if let Some(replace_range) = citation {
            let bibliography = self.bibliography.read(cx);
            if bibliography.has_entries() {
                // The same key routinely appears in several `.bib` files
                // (per-paper copies of one master bibliography); the menu
                // wants one row per key, not one per file.
                let mut seen = HashSet::default();
                let completions = bibliography
                    .entries()
                    .filter(|entry| seen.insert(entry.key.clone()))
                    .map(|entry| Completion {
                        replace_range: replace_range.clone(),
                        new_text: entry.key.to_string(),
                        label: CodeLabel::plain(entry.key.to_string(), None),
                        documentation: entry.documentation(),
                        source: CompletionSource::Custom,
                        icon_path: None,
                        icon_color: None,
                        match_start: Some(replace_range.start),
                        snippet_deduplication_key: None,
                        insert_text_mode: None,
                        confirm: None,
                        group: None,
                    })
                    .collect();
                return Task::ready(Ok(vec![CompletionResponse {
                    completions,
                    // Cite keys are short; without dynamic width the menu
                    // pads out to the stock LSP width and dwarfs its rows.
                    display_options: CompletionDisplayOptions {
                        dynamic_width: true,
                    },
                    is_incomplete: false,
                }]));
            }
        }
        self.inner
            .completions(buffer, buffer_position, trigger, window, cx)
    }

    fn resolve_completions(
        &self,
        buffer: Entity<Buffer>,
        completion_indices: Vec<usize>,
        completions: Rc<std::cell::RefCell<Box<[Completion]>>>,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<bool>> {
        self.inner
            .resolve_completions(buffer, completion_indices, completions, cx)
    }

    fn apply_additional_edits_for_completion(
        &self,
        buffer: Entity<Buffer>,
        completions: Rc<std::cell::RefCell<Box<[Completion]>>>,
        completion_index: usize,
        push_to_history: bool,
        all_commit_ranges: Vec<std::ops::Range<language::Anchor>>,
        cx: &mut Context<Editor>,
    ) -> Task<anyhow::Result<Option<language::Transaction>>> {
        self.inner.apply_additional_edits_for_completion(
            buffer,
            completions,
            completion_index,
            push_to_history,
            all_commit_ranges,
            cx,
        )
    }

    fn is_completion_trigger(
        &self,
        buffer: &Entity<Buffer>,
        position: language::Anchor,
        text: &str,
        trigger_in_words: bool,
        cx: &mut Context<Editor>,
    ) -> bool {
        if text.ends_with('@')
            && buffer_is_markdown(buffer.read(cx))
            && self.bibliography.read(cx).has_entries()
        {
            return true;
        }
        self.inner
            .is_completion_trigger(buffer, position, text, trigger_in_words, cx)
    }

    fn selection_changed(
        &self,
        mat: Option<&fuzzy::StringMatch>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.selection_changed(mat, window, cx);
    }

    fn sort_completions(&self) -> bool {
        self.inner.sort_completions()
    }

    fn filter_completions(&self) -> bool {
        self.inner.filter_completions()
    }

    fn show_snippets(&self) -> bool {
        self.inner.show_snippets()
    }
}

/// Wraps the editor's stock (project-backed) semantics provider so hovering
/// a resolved cite key shows the reference card; every other request — LSP
/// hovers, definitions, rename, inlay hints — delegates untouched.
pub struct CitationSemanticsProvider {
    inner: Rc<dyn SemanticsProvider>,
    bibliography: Entity<Bibliography>,
}

impl CitationSemanticsProvider {
    pub fn new(inner: Rc<dyn SemanticsProvider>, cx: &mut App) -> Self {
        Self {
            inner,
            bibliography: Bibliography::global(cx),
        }
    }
}

impl SemanticsProvider for CitationSemanticsProvider {
    fn hover(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Option<Vec<project::Hover>>>> {
        {
            let buffer_ref = buffer.read(cx);
            if buffer_is_markdown(buffer_ref) {
                let offset = position.to_offset(buffer_ref);
                if let Some((range, key)) = citation_key_at(buffer_ref, offset)
                    && let Some(entry) = self.bibliography.read(cx).resolve(&key)
                    && let Some(markdown) = entry.reference_markdown()
                {
                    let range =
                        buffer_ref.anchor_before(range.start)..buffer_ref.anchor_after(range.end);
                    return Some(Task::ready(Some(vec![project::Hover {
                        contents: vec![project::HoverBlock {
                            text: markdown,
                            kind: project::HoverBlockKind::Markdown,
                        }],
                        range: Some(range),
                        language: None,
                    }])));
                }
            }
        }
        self.inner.hover(buffer, position, cx)
    }

    fn inline_values(
        &self,
        buffer_handle: Entity<Buffer>,
        range: std::ops::Range<text::Anchor>,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Vec<project::InlayHint>>>> {
        self.inner.inline_values(buffer_handle, range, cx)
    }

    fn applicable_inlay_chunks(
        &self,
        buffer: &Entity<Buffer>,
        ranges: &[std::ops::Range<text::Anchor>],
        cx: &mut App,
    ) -> Vec<std::ops::Range<language::BufferRow>> {
        self.inner.applicable_inlay_chunks(buffer, ranges, cx)
    }

    fn invalidate_inlay_hints(
        &self,
        for_buffers: &HashSet<language::BufferId>,
        cx: &mut App,
    ) {
        self.inner.invalidate_inlay_hints(for_buffers, cx)
    }

    fn inlay_hints(
        &self,
        invalidate: project::InvalidationStrategy,
        buffer: Entity<Buffer>,
        ranges: Vec<std::ops::Range<text::Anchor>>,
        known_chunks: Option<(
            clock::Global,
            HashSet<std::ops::Range<language::BufferRow>>,
        )>,
        cx: &mut App,
    ) -> Option<
        HashMap<
            std::ops::Range<language::BufferRow>,
            Task<anyhow::Result<project::lsp_store::CacheInlayHints>>,
        >,
    > {
        self.inner
            .inlay_hints(invalidate, buffer, ranges, known_chunks, cx)
    }

    fn semantic_tokens(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut App,
    ) -> Option<
        futures::future::Shared<
            Task<
                std::result::Result<
                    project::lsp_store::BufferSemanticTokens,
                    std::sync::Arc<anyhow::Error>,
                >,
            >,
        >,
    > {
        self.inner.semantic_tokens(buffer, cx)
    }

    fn supports_inlay_hints(&self, buffer: &Entity<Buffer>, cx: &mut App) -> bool {
        self.inner.supports_inlay_hints(buffer, cx)
    }

    fn supports_semantic_tokens(&self, buffer: &Entity<Buffer>, cx: &mut App) -> bool {
        self.inner.supports_semantic_tokens(buffer, cx)
    }

    fn document_highlights(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Vec<project::DocumentHighlight>>>> {
        self.inner.document_highlights(buffer, position, cx)
    }

    fn definitions(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        kind: editor::GotoDefinitionKind,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Option<Vec<project::LocationLink>>>>> {
        self.inner.definitions(buffer, position, kind, cx)
    }

    fn range_for_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<std::ops::Range<text::Anchor>>>> {
        self.inner.range_for_rename(buffer, position, cx)
    }

    fn perform_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        new_name: String,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<project::ProjectTransaction>>> {
        self.inner
            .perform_rename(buffer, position, new_name, cx)
    }
}

#[cfg(test)]
mod bibliography_tests {
    use super::*;

    #[test]
    fn parses_entries_with_display_fields() {
        let entries = parse_bib(
            r#"
@article{smith2020,
  title = {A Study of
           Wrapped Titles},
  author = {Smith, Jane and Doe, John},
  year = {2020},
  journal = {Journal of Tests},
}

@book{knuth1984,
  title = {The {\TeX}book},
  author = {Knuth, Donald E.},
  date = {1984-01-01},
}
"#,
        );
        assert_eq!(entries.len(), 2);
        let smith = entries
            .iter()
            .find(|entry| entry.key.as_ref() == "smith2020")
            .expect("smith2020 parsed");
        assert_eq!(
            smith.title.as_deref(),
            Some("A Study of Wrapped Titles"),
            "wrapped title collapses to one line"
        );
        assert_eq!(smith.authors.as_deref(), Some("Jane Smith, John Doe"));
        assert_eq!(smith.year.as_deref(), Some("2020"));
        assert_eq!(smith.entry_type.as_ref(), "article");
        let knuth = entries
            .iter()
            .find(|entry| entry.key.as_ref() == "knuth1984")
            .expect("knuth1984 parsed");
        assert_eq!(
            knuth.year.as_deref(),
            Some("1984"),
            "date field yields a year"
        );
    }

    #[test]
    fn malformed_bib_parses_to_nothing() {
        assert_eq!(parse_bib("@article{unclosed,"), Vec::new());
        assert_eq!(parse_bib(""), Vec::new());
    }

    #[test]
    fn first_year_finds_four_digit_runs() {
        assert_eq!(first_year("2024"), Some("2024".to_string()));
        assert_eq!(first_year("2024-03-01"), Some("2024".to_string()));
        assert_eq!(first_year("about 1984, maybe"), Some("1984".to_string()));
        assert_eq!(first_year("no digits"), None);
        assert_eq!(first_year("123"), None);
    }
}
