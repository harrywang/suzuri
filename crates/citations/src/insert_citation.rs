//! The Insert Citation picker: search the vault library and Zotero without
//! leaving the editor, pick one or more entries, and get a citation at the
//! cursor in the buffer's own syntax. Picking a Zotero item also records its
//! entry in `refs/refs.bib` under a key Suzuri mints and copies its PDF
//! beside it, so the vault stays self-contained and nothing goes stale.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use editor::Editor;
use fs::{CopyOptions, Fs};
use fuzzy::{StringMatchCandidate, match_strings};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    ParentElement, Render, SharedString, Subscription, Task, WeakEntity, Window, actions, rems,
};
use picker::{Picker, PickerDelegate};
use project::Project;
use settings::Settings as _;
use ui::{Color, HighlightedLabel, Label, LabelSize, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, Workspace, notifications::NotifyTaskExt as _};

use crate::{
    BibEntry, Bibliography, CitationsSettings, ZoteroClient, ZoteroItem, append_entry,
    disambiguate, is_portable_key, mint_key,
};

actions!(
    citations,
    [
        /// Searches the vault library and Zotero and inserts a citation at the cursor.
        InsertCitation
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &InsertCitation, window, cx| {
            let zotero_url = CitationsSettings::get_global(cx).zotero_url.clone();
            let zotero = match ZoteroClient::local(zotero_url) {
                Ok(client) => Arc::new(client),
                Err(error) => {
                    log::error!("citations: {error:#}");
                    return;
                }
            };
            CitationPicker::open(workspace, zotero, window, cx);
        });
    })
    .detach();
}

/// The citation syntax of the buffer being edited, chosen by file
/// extension the same way the typeset preview picks its compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CitationFormat {
    /// Pandoc: `[@key; @other]`.
    Markdown,
    /// `\cite{key,other}`.
    Latex,
    /// `@key`, or `#cite(label("key"))` when the key is not a legal label.
    Typst,
}

impl CitationFormat {
    pub fn for_path(path: &Path) -> Self {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("tex" | "latex" | "ltx") => Self::Latex,
            Some("typ") => Self::Typst,
            _ => Self::Markdown,
        }
    }

    pub fn citation_text(self, keys: &[String]) -> String {
        match self {
            Self::Markdown => {
                let inner = keys
                    .iter()
                    .map(|key| format!("@{key}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("[{inner}]")
            }
            Self::Latex => format!("\\cite{{{}}}", keys.join(",")),
            Self::Typst => keys
                .iter()
                .map(|key| {
                    if is_portable_key(key) {
                        format!("@{key}")
                    } else {
                        format!("#cite(label({key:?}))")
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// The key a Zotero item gets in the vault: Better BibTeX's, when that add-on
/// assigned one and it is portable, so a user who already cites that key
/// elsewhere keeps it; otherwise one Suzuri mints.
pub fn choose_key(item: &ZoteroItem, taken: impl Fn(&str) -> bool) -> String {
    let base = match &item.citation_key {
        Some(key) if is_portable_key(key) => key.clone(),
        _ => mint_key(
            item.first_surname.as_deref(),
            Some(&item.date),
            Some(&item.title),
        ),
    };
    disambiguate(&base, taken)
}

pub struct CitationPicker {
    picker: Entity<Picker<CitationPickerDelegate>>,
    _reload_on_index_change: Subscription,
}

impl CitationPicker {
    /// Opens the picker over the active editor. Returns `None` when there is
    /// no editor with a file to cite into.
    pub fn open(
        workspace: &mut Workspace,
        zotero: Arc<ZoteroClient>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Option<()> {
        let editor = workspace.active_item_as::<Editor>(cx)?;
        let project = workspace.project().clone();
        let (format, root) = {
            let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
            let buffer = buffer.read(cx);
            let file = buffer.file()?;
            let worktree = project.read(cx).worktree_for_id(file.worktree_id(cx), cx)?;
            let format = CitationFormat::for_path(file.path().as_std_path());
            (format, worktree.read(cx).abs_path().to_path_buf())
        };
        let library_path = root.join(&CitationsSettings::get_global(cx).library);
        let bibliography = Bibliography::global(cx);
        Bibliography::ensure_project(&bibliography, &project, cx);
        let workspace_handle = cx.entity().downgrade();
        let editor = editor.downgrade();
        workspace.toggle_modal(window, cx, move |window, cx| {
            CitationPicker::new(
                CitationPickerDelegate {
                    modal: cx.entity().downgrade(),
                    workspace: workspace_handle,
                    editor,
                    project,
                    bibliography: bibliography.clone(),
                    zotero,
                    library_path,
                    format,
                    vault_entries: Vec::new(),
                    vault_candidates: Vec::new(),
                    matches: Vec::new(),
                    selected_index: 0,
                    chosen: Vec::new(),
                    zotero_note: None,
                    query: String::new(),
                },
                bibliography,
                window,
                cx,
            )
        });
        Some(())
    }

    fn new(
        mut delegate: CitationPickerDelegate,
        bibliography: Entity<Bibliography>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        delegate.reload_vault(bibliography.read(cx));
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        // The index parses `.bib` files off the main thread, so a vault
        // opened a moment ago may finish loading while the picker is up.
        let reload = cx.observe_in(&bibliography, window, |this, bibliography, window, cx| {
            this.picker.update(cx, |picker, cx| {
                picker.delegate.reload_vault(bibliography.read(cx));
                picker.refresh(window, cx);
            });
        });
        Self {
            picker,
            _reload_on_index_change: reload,
        }
    }
}

impl Render for CitationPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("CitationPicker")
            .w(rems(40.))
            .child(self.picker.clone())
    }
}

impl Focusable for CitationPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for CitationPicker {}
impl ModalView for CitationPicker {}

#[derive(Clone, Debug, PartialEq)]
enum Candidate {
    Vault(BibEntry),
    Zotero(ZoteroItem),
}

impl Candidate {
    fn title(&self) -> SharedString {
        match self {
            Self::Vault(entry) => entry.title.clone().unwrap_or_else(|| entry.key.clone()),
            Self::Zotero(item) => {
                if item.title.is_empty() {
                    "(untitled)".into()
                } else {
                    item.title.clone().into()
                }
            }
        }
    }

    /// Authors, year and where the entry comes from.
    fn detail(&self) -> String {
        let mut parts = Vec::new();
        match self {
            Self::Vault(entry) => {
                parts.extend(entry.authors.as_ref().map(|authors| authors.to_string()));
                parts.extend(entry.year.as_ref().map(|year| year.to_string()));
                parts.push(format!("in vault as @{}", entry.key));
            }
            Self::Zotero(item) => {
                if !item.creators.is_empty() {
                    parts.push(item.creators.join(", "));
                }
                parts.extend(item.year());
                parts.push("Zotero".to_string());
            }
        }
        parts.join(" · ")
    }
}

struct Match {
    candidate: Candidate,
    /// Fuzzy-match positions within the title, for highlighting.
    positions: Vec<usize>,
}

pub struct CitationPickerDelegate {
    modal: WeakEntity<CitationPicker>,
    workspace: WeakEntity<Workspace>,
    editor: WeakEntity<Editor>,
    project: Entity<Project>,
    bibliography: Entity<Bibliography>,
    zotero: Arc<ZoteroClient>,
    library_path: PathBuf,
    format: CitationFormat,
    vault_entries: Vec<BibEntry>,
    /// One per vault entry, in the same order: title, authors, year and key
    /// joined, so a search by author or year finds the entry too.
    vault_candidates: Vec<StringMatchCandidate>,
    matches: Vec<Match>,
    selected_index: usize,
    /// The multi-selection, in the order the user picked.
    chosen: Vec<Candidate>,
    /// Why Zotero results are missing, when they are.
    zotero_note: Option<SharedString>,
    query: String,
}

/// How long typing pauses before the query goes to Zotero. The vault search
/// runs on every keystroke; only the network call is held back.
const ZOTERO_DEBOUNCE: Duration = Duration::from_millis(150);
const ZOTERO_RESULT_LIMIT: usize = 20;
const VAULT_RESULT_LIMIT: usize = 50;

impl CitationPickerDelegate {
    fn reload_vault(&mut self, bibliography: &Bibliography) {
        let mut entries = bibliography.entries().cloned().collect::<Vec<_>>();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        entries.dedup_by(|a, b| a.key == b.key);
        self.vault_candidates = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut text = String::new();
                for part in [&entry.title, &entry.authors, &entry.year]
                    .into_iter()
                    .flatten()
                {
                    text.push_str(part);
                    text.push(' ');
                }
                text.push_str(&entry.key);
                StringMatchCandidate::new(index, &text)
            })
            .collect();
        self.vault_entries = entries;
    }

    fn duplicates_vault_entry(&self, item: &ZoteroItem) -> bool {
        let title = normalized(&item.title);
        self.vault_entries.iter().any(|entry| {
            item.citation_key.as_deref() == Some(entry.key.as_ref())
                || (!title.is_empty()
                    && entry
                        .title
                        .as_ref()
                        .is_some_and(|vault_title| normalized(vault_title) == title))
        })
    }

    fn commit(
        &mut self,
        candidates: Vec<Candidate>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        if candidates.is_empty() {
            return;
        }
        let task = self.insert(candidates, window, cx);
        task.detach_and_notify_err(self.workspace.clone(), window, cx);
        self.dismissed(window, cx);
    }

    /// Records each Zotero pick in the library (and its PDF beside it), then
    /// inserts the citation. Vault picks only need their key.
    fn insert(
        &self,
        candidates: Vec<Candidate>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<Result<()>> {
        let fs: Arc<dyn Fs> = self.project.read(cx).fs().clone();
        let zotero = self.zotero.clone();
        let library_path = self.library_path.clone();
        let format = self.format;
        let editor = self.editor.clone();
        let bibliography = self.bibliography.clone();
        let project = self.project.clone();
        let mut taken: HashSet<String> = self
            .vault_entries
            .iter()
            .map(|entry| entry.key.to_string())
            .collect();
        cx.spawn_in(window, async move |_, cx| {
            let mut keys = Vec::with_capacity(candidates.len());
            let mut library_changed = false;
            for candidate in candidates {
                match candidate {
                    Candidate::Vault(entry) => keys.push(entry.key.to_string()),
                    Candidate::Zotero(item) => {
                        let source = zotero
                            .biblatex(&item.key)
                            .await
                            .map_err(anyhow::Error::new)?;
                        let key = choose_key(&item, |candidate| taken.contains(candidate));
                        taken.insert(key.clone());
                        if append_entry(&fs, &library_path, &source, &key).await? {
                            library_changed = true;
                        }
                        // A missing PDF is not a reason to lose the citation.
                        match zotero.pdf_path(&item.key).await {
                            Ok(Some(pdf)) => {
                                let target = library_path
                                    .parent()
                                    .unwrap_or(Path::new(""))
                                    .join(format!("{key}.pdf"));
                                if !fs.is_file(&target).await {
                                    fs.copy_file(
                                        &pdf,
                                        &target,
                                        CopyOptions {
                                            overwrite: false,
                                            ignore_if_exists: true,
                                        },
                                    )
                                    .await
                                    .log_err();
                                }
                            }
                            Ok(None) => {}
                            Err(error) => log::warn!("citations: no PDF for {}: {error}", item.key),
                        }
                        keys.push(key);
                    }
                }
            }
            if library_changed {
                cx.update(|_, cx| {
                    Bibliography::reload_paths(&bibliography, &project, vec![library_path], cx)
                })?;
            }
            let text = format.citation_text(&keys);
            editor.update_in(cx, |editor, window, cx| editor.insert(&text, window, cx))?;
            Ok(())
        })
    }
}

fn normalized(title: &str) -> String {
    title
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

impl PickerDelegate for CitationPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "citation picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search the vault and Zotero by title, author or year…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        if let Some(note) = &self.zotero_note {
            return Some(note.clone());
        }
        if self.query.chars().count() < 2 {
            return Some("Type to search Zotero".into());
        }
        Some("No matches".into())
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_string();
        self.query = query.clone();
        let executor = cx.background_executor().clone();
        let candidates = self.vault_candidates.clone();
        let zotero = self.zotero.clone();
        cx.spawn_in(window, async move |this, cx| {
            let vault_matches = if query.is_empty() {
                (0..candidates.len().min(VAULT_RESULT_LIMIT))
                    .map(|index| (index, Vec::new()))
                    .collect::<Vec<_>>()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    VAULT_RESULT_LIMIT,
                    &Default::default(),
                    executor.clone(),
                )
                .await
                .into_iter()
                .map(|found| (found.candidate_id, found.positions))
                .collect()
            };
            this.update_in(cx, |picker, window, cx| {
                let delegate = &mut picker.delegate;
                delegate.matches = vault_matches
                    .into_iter()
                    .filter_map(|(index, positions)| {
                        let entry = delegate.vault_entries.get(index)?;
                        let title_length = entry.title.as_ref().map_or(0, |title| title.len());
                        Some(Match {
                            candidate: Candidate::Vault(entry.clone()),
                            positions: positions
                                .into_iter()
                                .filter(|position| *position < title_length)
                                .collect(),
                        })
                    })
                    .collect();
                delegate.zotero_note = None;
                picker.set_selected_index(0, None, false, window, cx);
                cx.notify();
            })
            .log_err();

            if query.chars().count() < 2 {
                return;
            }
            executor.timer(ZOTERO_DEBOUNCE).await;
            let result = zotero.search(&query, ZOTERO_RESULT_LIMIT).await;
            this.update_in(cx, |picker, _window, cx| {
                let delegate = &mut picker.delegate;
                match result {
                    Ok(items) => {
                        for item in items {
                            if !delegate.duplicates_vault_entry(&item) {
                                delegate.matches.push(Match {
                                    candidate: Candidate::Zotero(item),
                                    positions: Vec::new(),
                                });
                            }
                        }
                    }
                    Err(error) => delegate.zotero_note = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(found) = self.matches.get(self.selected_index) else {
            return;
        };
        let candidate = found.candidate.clone();
        self.commit(vec![candidate], window, cx);
    }

    fn supports_multi_select(&self) -> bool {
        true
    }

    fn is_item_selected(&self, ix: usize) -> bool {
        self.matches
            .get(ix)
            .is_some_and(|found| self.chosen.contains(&found.candidate))
    }

    fn toggle_item_selected(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(found) = self.matches.get(ix) else {
            return;
        };
        if let Some(position) = self
            .chosen
            .iter()
            .position(|chosen| *chosen == found.candidate)
        {
            self.chosen.remove(position);
        } else {
            self.chosen.push(found.candidate.clone());
        }
        cx.notify();
    }

    fn selected_item_count(&self) -> usize {
        self.chosen.len()
    }

    fn clear_selection(&mut self, _cx: &mut Context<Picker<Self>>) {
        self.chosen.clear();
    }

    fn confirm_multi(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let chosen = std::mem::take(&mut self.chosen);
        self.commit(chosen, window, cx);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.modal.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let found = self.matches.get(ix)?;
        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .child(
                            HighlightedLabel::new(found.candidate.title(), found.positions.clone())
                                .truncate_middle(),
                        )
                        .child(
                            Label::new(found.candidate.detail())
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate_start(),
                        ),
                ),
        )
    }

    fn render_footer(
        &self,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        let note = self.zotero_note.clone()?;
        if self.matches.is_empty() {
            // `no_matches_text` already shows it.
            return None;
        }
        Some(
            h_flex()
                .p_2()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(Label::new(note).size(LabelSize::Small).color(Color::Muted))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_BASE_URL;
    use gpui::TestAppContext;
    use http_client::{AsyncBody, FakeHttpClient, Response};
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::SettingsStore;

    #[test]
    fn citation_text_follows_the_buffer_syntax() {
        let keys = vec!["vaswani2017attention".to_string(), "knuth1984".to_string()];
        assert_eq!(
            CitationFormat::Markdown.citation_text(&keys),
            "[@vaswani2017attention; @knuth1984]"
        );
        assert_eq!(
            CitationFormat::Latex.citation_text(&keys),
            "\\cite{vaswani2017attention,knuth1984}"
        );
        assert_eq!(
            CitationFormat::Typst.citation_text(&keys),
            "@vaswani2017attention @knuth1984"
        );
        assert_eq!(
            CitationFormat::Typst.citation_text(&["Segawa2019Inf.Process.".to_string()]),
            "#cite(label(\"Segawa2019Inf.Process.\"))"
        );
    }

    #[test]
    fn format_follows_the_extension() {
        assert_eq!(
            CitationFormat::for_path(Path::new("a/Note.md")),
            CitationFormat::Markdown
        );
        assert_eq!(
            CitationFormat::for_path(Path::new("paper.TEX")),
            CitationFormat::Latex
        );
        assert_eq!(
            CitationFormat::for_path(Path::new("paper.typ")),
            CitationFormat::Typst
        );
        assert_eq!(
            CitationFormat::for_path(Path::new("notes")),
            CitationFormat::Markdown
        );
    }

    #[test]
    fn keeps_a_portable_better_bibtex_key_and_mints_otherwise() {
        let mut item = ZoteroItem {
            key: "ABCD".into(),
            item_type: "preprint".into(),
            title: "SkillsBench: Benchmarking Agent Skills".into(),
            creators: vec!["Ada Lovelace".into()],
            first_surname: Some("Lovelace".into()),
            date: "2026-02-01".into(),
            citation_key: Some("lovelace_skills".into()),
        };
        assert_eq!(choose_key(&item, |_| false), "lovelace_skills");
        assert_eq!(
            choose_key(&item, |key| key == "lovelace_skills"),
            "lovelace_skillsa"
        );
        item.citation_key = Some("bad/key.".into());
        assert_eq!(choose_key(&item, |_| false), "lovelace2026skillsbench");
        item.citation_key = None;
        assert_eq!(choose_key(&item, |_| false), "lovelace2026skillsbench");
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    /// A Zotero answering the four requests the picker makes, with one
    /// preprint that has a PDF attached.
    fn fake_zotero() -> Arc<ZoteroClient> {
        let http = FakeHttpClient::create(|request| async move {
            let path = request.uri().path().to_string();
            let query = request.uri().query().unwrap_or("").to_string();
            let (status, body, location) = match path.as_str() {
                "/api/users/0/items/top" => (
                    200,
                    json!([{ "key": "Y5TDIU47", "data": {
                        "key": "Y5TDIU47", "itemType": "preprint",
                        "title": "SkillsBench: Benchmarking How Well Agent Skills Work",
                        "creators": [{"creatorType": "author", "firstName": "Ada", "lastName": "Lovelace"}],
                        "date": "2026-02-01"
                    }}])
                    .to_string(),
                    None,
                ),
                "/api/users/0/items/Y5TDIU47" if query.contains("format=biblatex") => (
                    200,
                    "@online{lovelace_skillsbench_2026,\n  title = {SkillsBench: Benchmarking How Well Agent Skills Work},\n  author = {Lovelace, Ada},\n  date = {2026-02-01},\n}\n".to_string(),
                    None,
                ),
                "/api/users/0/items/Y5TDIU47/children" => (
                    200,
                    json!([{ "data": { "key": "PDTR8GS2", "itemType": "attachment", "contentType": "application/pdf" }}]).to_string(),
                    None,
                ),
                "/api/users/0/items/PDTR8GS2/file" => (
                    302,
                    String::new(),
                    Some("file:///zotero/storage/PDTR8GS2/Skills%20Bench.pdf"),
                ),
                _ => (404, format!("unexpected request to {path}"), None),
            };
            let mut response = Response::builder().status(status);
            if let Some(location) = location {
                response = response.header("Location", location);
            }
            Ok(response.body(AsyncBody::from(body)).unwrap())
        });
        Arc::new(ZoteroClient::new(http, DEFAULT_BASE_URL))
    }

    #[gpui::test]
    async fn picking_a_zotero_item_records_it_and_cites_it(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                "Note.md": "A claim ",
                "refs": {
                    "refs.bib": "@book{knuth1984,\n  title = {The TeXbook},\n  author = {Knuth, Donald},\n  date = {1984},\n}\n"
                }
            }),
        )
        .await;
        fs.insert_tree(
            "/zotero/storage/PDTR8GS2",
            json!({ "Skills Bench.pdf": "%PDF-1.4 fake" }),
        )
        .await;
        let project = Project::test(fs.clone(), ["/vault".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let note_path = project
            .read_with(cx, |project, cx| {
                project.find_project_path("/vault/Note.md", cx)
            })
            .expect("the note is in the vault");
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(note_path, None, true, window, cx)
            })
            .await
            .expect("failed to open the note")
            .downcast::<Editor>()
            .expect("a markdown file opens in an editor");
        editor.update_in(cx, |editor, window, cx| {
            editor.move_to_end(&editor::actions::MoveToEnd, window, cx);
        });

        let zotero = fake_zotero();
        workspace.update_in(cx, |workspace, window, cx| {
            CitationPicker::open(workspace, zotero, window, cx)
                .expect("the picker opens over an editor with a file");
        });
        cx.run_until_parked();
        let picker = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_modal::<CitationPicker>(cx)
                .expect("the picker is the active modal")
                .read(cx)
                .picker
                .clone()
        });

        // With no query, the vault's own entries are listed.
        let titles = |cx: &mut gpui::VisualTestContext| {
            picker.read_with(cx, |picker, _| {
                picker
                    .delegate
                    .matches
                    .iter()
                    .map(|found| found.candidate.title().to_string())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(titles(cx), ["The TeXbook"]);

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("skills".into(), window, cx);
        });
        cx.executor().advance_clock(ZOTERO_DEBOUNCE * 2);
        cx.run_until_parked();
        let listed = titles(cx);
        assert_eq!(
            listed,
            ["SkillsBench: Benchmarking How Well Agent Skills Work"],
            "the Zotero hit is listed and the non-matching vault entry is not"
        );

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.selected_index = 0;
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();

        let text = editor.read_with(cx, |editor, cx| editor.text(cx));
        assert_eq!(text, "A claim [@lovelace2026skillsbench]");
        let library = fs
            .load("/vault/refs/refs.bib".as_ref())
            .await
            .expect("the library exists");
        assert!(
            library.starts_with("@book{knuth1984,"),
            "existing entry kept:\n{library}"
        );
        assert!(
            library.contains("@online{lovelace2026skillsbench,"),
            "entry recorded under the minted key:\n{library}"
        );
        assert!(
            fs.is_file("/vault/refs/lovelace2026skillsbench.pdf".as_ref())
                .await,
            "the PDF was copied beside the library"
        );
        assert!(
            workspace.read_with(cx, |workspace, cx| workspace
                .active_modal::<CitationPicker>(cx)
                .is_none()),
            "the picker closes on confirm"
        );
    }

    #[gpui::test]
    async fn a_disabled_local_api_is_explained_not_hidden(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", json!({ "Note.md": "" })).await;
        let project = Project::test(fs.clone(), ["/vault".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let note_path = project
            .read_with(cx, |project, cx| {
                project.find_project_path("/vault/Note.md", cx)
            })
            .expect("the note is in the vault");
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(note_path, None, true, window, cx)
            })
            .await
            .expect("failed to open the note");
        let http = FakeHttpClient::create(|_request| async move {
            Ok(Response::builder()
                .status(403)
                .body(AsyncBody::from("Local API is not enabled"))
                .unwrap())
        });
        let zotero = Arc::new(ZoteroClient::new(http, DEFAULT_BASE_URL));
        workspace.update_in(cx, |workspace, window, cx| {
            CitationPicker::open(workspace, zotero, window, cx)
                .expect("the picker opens over an editor with a file");
        });
        cx.run_until_parked();
        let picker = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_modal::<CitationPicker>(cx)
                .expect("the picker is the active modal")
                .read(cx)
                .picker
                .clone()
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("anything".into(), window, cx);
        });
        cx.executor().advance_clock(ZOTERO_DEBOUNCE * 2);
        cx.run_until_parked();
        let note = picker.read_with(cx, |picker, _| picker.delegate.zotero_note.clone());
        let note = note.expect("the disabled API is reported");
        assert!(note.contains("Allow other applications"), "{note}");
    }
}
