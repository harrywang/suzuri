//! Formatted citations and reference-list entries, rendered by hayagriva,
//! the CSL engine Typst uses. Styles come from hayagriva's bundled archive
//! (APA, IEEE, Chicago and roughly a hundred more), selected per document
//! through a `csl:` key in the note's frontmatter.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use fs::Fs;
use gpui::{App, AppContext as _, Entity, Global};

use hayagriva::{
    BibliographyDriver, BibliographyRequest, BufWriteFormat, CitationItem, CitationRequest,
    CitePurpose, ElemChildren, Entry, LocatorPayload, SpecificLocator,
    archive::{ArchivedStyle, locales},
    citationberg::{IndependentStyle, Locale, Style},
};

pub use hayagriva::citationberg::taxonomy::Locator;

/// The style used when a document names none.
pub const DEFAULT_STYLE: &str = "apa";

/// A CSL style ready to render with. Decoding one from the archive costs a
/// few milliseconds, so they are cached by name for the life of the process.
pub struct CslStyle {
    pub name: String,
    style: IndependentStyle,
}

fn locale_files() -> &'static [Locale] {
    static LOCALES: OnceLock<Vec<Locale>> = OnceLock::new();
    LOCALES.get_or_init(locales)
}

/// The bundled style called `name` (hayagriva's names: `apa`,
/// `ieee`, `chicago-author-date`, `modern-language-association`, …), or
/// `None` when no bundled style goes by that name. Dependent styles, which
/// only point at a parent, are treated as unknown.
pub fn style_named(name: &str) -> Option<Arc<CslStyle>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<CslStyle>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let name = name.trim().trim_end_matches(".csl").to_ascii_lowercase();
    if let Ok(cache) = cache.lock()
        && let Some(style) = cache.get(&name)
    {
        return Some(style.clone());
    }
    let Style::Independent(style) = ArchivedStyle::by_name(&name)?.get() else {
        return None;
    };
    let style = Arc::new(CslStyle {
        name: name.clone(),
        style,
    });
    if let Ok(mut cache) = cache.lock() {
        cache.insert(name, style.clone());
    }
    Some(style)
}

/// The style a note asked for, or APA when the name is not bundled, in
/// which case the unknown name comes back too so the UI can say so instead
/// of silently dropping every rendering.
pub fn style_or_default(name: &str) -> Option<(Arc<CslStyle>, Option<String>)> {
    if let Some(style) = style_named(name) {
        return Some((style, None));
    }
    style_named(DEFAULT_STYLE).map(|style| (style, Some(name.trim().to_string())))
}

/// Every name a bundled style answers to (`apa` as well as
/// `american-psychological-association`), sorted, for messages that suggest
/// what to type.
pub fn style_names() -> Vec<&'static str> {
    let mut names = ArchivedStyle::all()
        .iter()
        .flat_map(|style| style.names().iter().copied())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    names
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedReference {
    /// The in-text form, e.g. `(Vaswani et al., 2017)`.
    pub citation: String,
    /// The reference-list entry.
    pub reference: String,
}

/// In-text citation and reference-list entry for one work. Note styles
/// have no reference list, in which case the citation stands in for it.
pub fn render_reference(entry: &Entry, style: &CslStyle) -> Option<RenderedReference> {
    let locales = locale_files();
    let mut driver = BibliographyDriver::new();
    driver.citation(CitationRequest::from_items(
        vec![CitationItem::with_entry(entry)],
        &style.style,
        locales,
    ));
    let rendered = driver.finish(BibliographyRequest::new(&style.style, None, locales));
    let citation = plain(&rendered.citations.first()?.citation);
    let reference = rendered
        .bibliography
        .and_then(|bibliography| bibliography.items.into_iter().next())
        .map(|item| plain(&item.content))
        .unwrap_or_else(|| citation.clone());
    Some(RenderedReference {
        citation,
        reference,
    })
}

/// The reference list for the cited works, as `(key, text)` pairs in the
/// order the style prescribes (alphabetical for APA, cited order for IEEE).
pub fn render_bibliography(entries: &[&Entry], style: &CslStyle) -> Vec<(String, String)> {
    let locales = locale_files();
    let mut driver = BibliographyDriver::new();
    for entry in entries {
        driver.citation(CitationRequest::from_items(
            vec![CitationItem::with_entry(*entry)],
            &style.style,
            locales,
        ));
    }
    let rendered = driver.finish(BibliographyRequest::new(&style.style, None, locales));
    let Some(bibliography) = rendered.bibliography else {
        return rendered
            .citations
            .iter()
            .zip(entries)
            .map(|(citation, entry)| (entry.key().to_string(), plain(&citation.citation)))
            .collect();
    };
    bibliography
        .items
        .into_iter()
        .map(|item| {
            let mut text = String::new();
            if let Some(first) = &item.first_field {
                first.write_buf(&mut text, BufWriteFormat::Plain).ok();
                text.push(' ');
            }
            item.content
                .write_buf(&mut text, BufWriteFormat::Plain)
                .ok();
            (item.key, text)
        })
        .collect()
}

/// One work inside a citation group, as the note cites it.
pub struct CiteItem<'a> {
    pub entry: &'a Entry,
    /// `[@key, p. 3]` → `(Page, "3")`.
    pub locator: Option<(Locator, String)>,
    /// How the item reads: pandoc's `[-@key]` suppresses the author,
    /// and a bare `@key` in running text names the author in prose.
    pub form: CiteForm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CiteForm {
    Normal,
    YearOnly,
    Prose,
}

/// Everything a note's citations render to, from one pass, so numbered
/// styles agree between the in-text marks and the reference list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedDocument {
    /// One in-text citation per group, in the order given.
    pub citations: Vec<String>,
    /// The reference list as `(key, text)`, in the style's order.
    pub bibliography: Vec<(String, String)>,
}

/// Renders every citation group of a note and its reference list in one
/// pass. Groups must come in document order: numbered styles assign
/// numbers by first citation.
pub fn render_document(groups: &[Vec<CiteItem<'_>>], style: &CslStyle) -> RenderedDocument {
    let locales = locale_files();
    let mut driver = BibliographyDriver::new();
    for group in groups {
        let items = group
            .iter()
            .map(|item| {
                let locator = item
                    .locator
                    .as_ref()
                    .map(|(kind, value)| SpecificLocator(*kind, LocatorPayload::Str(value)));
                let citation = CitationItem::with_locator(item.entry, locator);
                match item.form {
                    CiteForm::Normal => citation,
                    CiteForm::YearOnly => citation.kind(CitePurpose::Year),
                    CiteForm::Prose => citation.kind(CitePurpose::Prose),
                }
            })
            .collect();
        driver.citation(CitationRequest::from_items(items, &style.style, locales));
    }
    let rendered = driver.finish(BibliographyRequest::new(&style.style, None, locales));
    let citations = rendered
        .citations
        .iter()
        .zip(groups)
        .map(|(citation, group)| {
            let text = plain(&citation.citation);
            // hayagriva drops the affixes for a year-only cite; pandoc keeps
            // them, so `Vaswani et al. [-@key]` reads `Vaswani et al. (2017)`.
            let year_only = group.iter().all(|item| item.form == CiteForm::YearOnly);
            if year_only && !text.starts_with(['(', '[']) {
                format!("({text})")
            } else {
                text
            }
        })
        .collect();
    let bibliography = match rendered.bibliography {
        Some(bibliography) => bibliography
            .items
            .into_iter()
            .map(|item| {
                let mut text = String::new();
                if let Some(first) = &item.first_field {
                    first.write_buf(&mut text, BufWriteFormat::Plain).ok();
                    text.push(' ');
                }
                item.content
                    .write_buf(&mut text, BufWriteFormat::Plain)
                    .ok();
                (item.key, text)
            })
            .collect(),
        None => Vec::new(),
    };
    RenderedDocument {
        citations,
        bibliography,
    }
}

/// What follows a key inside a citation group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiteSuffix {
    /// Nothing, or only punctuation and whitespace.
    None,
    /// A locator pandoc would recognize: `, p. 3`, `, pp. 3-4`, `, chap. 2`.
    Locator(Locator, String),
    /// Free text the renderer cannot place, e.g. `, emphasis added`.
    Unsupported,
}

/// Parses the text after a cite key: `, p. 3` → a page locator.
pub fn parse_cite_suffix(suffix: &str) -> CiteSuffix {
    let trimmed = suffix.trim().trim_start_matches(',').trim();
    if trimmed.is_empty() {
        return CiteSuffix::None;
    }
    let (label, value) = match trimmed.split_once(|character: char| character.is_whitespace()) {
        Some((label, value)) => (label, value.trim()),
        None => return CiteSuffix::Unsupported,
    };
    let kind = match label.trim_end_matches('.').to_ascii_lowercase().as_str() {
        "p" | "pp" | "page" | "pages" => Locator::Page,
        "ch" | "chap" | "chapter" | "chapters" => Locator::Chapter,
        "sec" | "section" | "sections" => Locator::Section,
        "fig" | "figure" | "figures" => Locator::Figure,
        "vol" | "volume" | "volumes" => Locator::Volume,
        "no" | "number" | "issue" => Locator::Issue,
        "para" | "paragraph" | "paragraphs" => Locator::Paragraph,
        "n" | "note" | "notes" => Locator::Note,
        "l" | "line" | "lines" => Locator::Line,
        "table" => Locator::Table,
        "pt" | "part" => Locator::Part,
        _ => return CiteSuffix::Unsupported,
    };
    if value.is_empty() {
        return CiteSuffix::Unsupported;
    }
    CiteSuffix::Locator(kind, value.to_string())
}

/// What a note's `csl:` value names: a bundled style by name, or a `.csl`
/// file by path (anything ending in `.csl` or containing a separator),
/// resolved against the note's folder and then the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleSource {
    Bundled(String),
    File(String),
}

/// The `csl:` (or `citation-style:`) value in a document's leading
/// frontmatter, as written, minus quotes.
pub fn document_style_source(text: &str) -> Option<StyleSource> {
    let mut lines = text.lines();
    let opener = lines.next()?.trim();
    if opener != "---" && opener != "+++" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "+++" || trimmed == "..." {
            break;
        }
        let Some((key, value)) = trimmed.split_once(':').or_else(|| trimmed.split_once('=')) else {
            continue;
        };
        if !matches!(key.trim(), "csl" | "citation-style" | "citation_style") {
            continue;
        }
        let value = value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .trim();
        if value.is_empty() {
            return None;
        }
        let is_file = value.ends_with(".csl") || value.contains('/') || value.contains('\\');
        return Some(if is_file {
            StyleSource::File(value.to_string())
        } else {
            StyleSource::Bundled(value.to_string())
        });
    }
    None
}

/// The bundled-style name a document's frontmatter implies: the name itself,
/// or a file's stem (`styles/ieee.csl` → `ieee`).
pub fn document_style(text: &str) -> Option<String> {
    match document_style_source(text)? {
        StyleSource::Bundled(name) => Some(name),
        StyleSource::File(path) => Path::new(&path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned()),
    }
}

/// Parses a CSL file's XML into a style ready to render with.
pub fn style_from_xml(name: impl Into<String>, xml: &str) -> Result<Arc<CslStyle>, String> {
    match Style::from_xml(xml) {
        Ok(Style::Independent(style)) => Ok(Arc::new(CslStyle {
            name: name.into(),
            style,
        })),
        Ok(Style::Dependent(_)) => {
            Err("is a dependent style, one that only points at a parent style".to_string())
        }
        Err(error) => Err(format!("could not be parsed: {error}")),
    }
}

/// What is known about one `.csl` file a note points at.
#[derive(Debug, Clone)]
pub enum StyleFileState {
    Loading,
    Ready(Arc<CslStyle>),
    Missing,
    Failed(String),
}

impl std::fmt::Debug for CslStyle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CslStyle")
            .field("name", &self.name)
            .finish()
    }
}

/// The `.csl` files notes point at, loaded through the project's filesystem
/// so they work in every kind of worktree. Loading is asynchronous: a lookup
/// that starts a load answers `Loading`, and the entity notifies when the
/// file arrives so live preview can recompute.
pub struct StyleFiles {
    states: HashMap<PathBuf, StyleFileState>,
}

struct GlobalStyleFiles(Entity<StyleFiles>);

impl Global for GlobalStyleFiles {}

impl StyleFiles {
    pub fn global(cx: &mut App) -> Entity<StyleFiles> {
        if let Some(global) = cx.try_global::<GlobalStyleFiles>() {
            return global.0.clone();
        }
        let files = cx.new(|_| StyleFiles {
            states: HashMap::new(),
        });
        cx.set_global(GlobalStyleFiles(files.clone()));
        files
    }

    pub fn try_global(cx: &App) -> Option<Entity<StyleFiles>> {
        cx.try_global::<GlobalStyleFiles>()
            .map(|global| global.0.clone())
    }

    pub fn state(&self, path: &Path) -> Option<&StyleFileState> {
        self.states.get(path)
    }

    /// The state of `path`, starting its load the first time it is asked for.
    pub fn lookup(
        this: &Entity<StyleFiles>,
        path: PathBuf,
        fs: Arc<dyn Fs>,
        cx: &mut App,
    ) -> StyleFileState {
        if let Some(state) = this.read(cx).states.get(&path) {
            return state.clone();
        }
        this.update(cx, |files, _| {
            files.states.insert(path.clone(), StyleFileState::Loading);
        });
        let files = this.clone();
        cx.spawn(async move |cx| {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let state = match fs.load(&path).await {
                Ok(xml) => {
                    cx.background_spawn(async move {
                        match style_from_xml(name, &xml) {
                            Ok(style) => StyleFileState::Ready(style),
                            Err(problem) => StyleFileState::Failed(problem),
                        }
                    })
                    .await
                }
                Err(_) => StyleFileState::Missing,
            };
            files.update(cx, |files, cx| {
                files.states.insert(path, state);
                cx.notify();
            });
        })
        .detach();
        StyleFileState::Loading
    }

    /// Drops what is known about `path`, so the next lookup reads it again.
    pub fn forget(this: &Entity<StyleFiles>, path: &Path, cx: &mut App) {
        this.update(cx, |files, cx| {
            if files.states.remove(path).is_some() {
                cx.notify();
            }
        });
    }
}

/// The style a note renders with, plus a problem to show when it is not the
/// one the note asked for.
#[derive(Debug, Clone)]
pub struct ResolvedStyle {
    pub style: Arc<CslStyle>,
    pub problem: Option<String>,
}

fn bundled_suggestions() -> String {
    [
        "apa",
        "ieee",
        "chicago-author-date",
        "mla",
        "harvard-cite-them-right",
    ]
    .into_iter()
    .filter(|known| style_named(known).is_some())
    .collect::<Vec<_>>()
    .join(", ")
}

/// A bundled style by name, or APA with a note when the name is unknown.
pub fn resolve_bundled(name: &str) -> Option<ResolvedStyle> {
    let (style, unknown) = style_or_default(name)?;
    let problem = unknown.map(|name| {
        format!(
            "csl: \"{name}\" is not a bundled style, so this is APA. Try {}, …",
            bundled_suggestions()
        )
    });
    Some(ResolvedStyle { style, problem })
}

/// What to render with when a `.csl` file cannot be used: the bundled style
/// of the same name when there is one (`styles/ieee.csl` → `ieee`), else APA
/// with a note saying why.
fn fallback_for_file(relative: &str, reason: &str) -> Option<ResolvedStyle> {
    let stem = Path::new(relative)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(style) = style_named(&stem) {
        return Some(ResolvedStyle {
            style,
            problem: None,
        });
    }
    style_named(DEFAULT_STYLE).map(|style| ResolvedStyle {
        style,
        problem: Some(format!("csl: \"{relative}\" {reason}, so this is APA.")),
    })
}

/// The absolute paths a `csl:` file value may mean, in the order to try:
/// as written when absolute, else under each of `search_dirs` (the note's
/// folder first, then the project root).
pub fn style_file_candidates(relative: &str, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return vec![path.to_path_buf()];
    }
    let mut candidates = Vec::new();
    for directory in search_dirs {
        let candidate = directory.join(path);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// Resolves a note's style, loading a `.csl` file it points at through
/// `fs` when it is not yet known. While a file loads the note renders in APA
/// without a note; the cache notifies when the file arrives.
pub fn resolve_style(
    source: Option<&StyleSource>,
    search_dirs: &[PathBuf],
    fs: Arc<dyn Fs>,
    cx: &mut App,
) -> Option<ResolvedStyle> {
    match source {
        None => style_named(DEFAULT_STYLE).map(|style| ResolvedStyle {
            style,
            problem: None,
        }),
        Some(StyleSource::Bundled(name)) => resolve_bundled(name),
        Some(StyleSource::File(relative)) => {
            let files = StyleFiles::global(cx);
            let mut failure: Option<String> = None;
            let mut loading = false;
            for candidate in style_file_candidates(relative, search_dirs) {
                match StyleFiles::lookup(&files, candidate, fs.clone(), cx) {
                    StyleFileState::Ready(style) => {
                        return Some(ResolvedStyle {
                            style,
                            problem: None,
                        });
                    }
                    StyleFileState::Loading => loading = true,
                    StyleFileState::Failed(problem) => {
                        failure.get_or_insert(problem);
                    }
                    StyleFileState::Missing => {}
                }
            }
            if loading {
                return style_named(DEFAULT_STYLE).map(|style| ResolvedStyle {
                    style,
                    problem: None,
                });
            }
            match failure {
                Some(problem) => fallback_for_file(relative, &problem),
                None => fallback_for_file(relative, "was not found"),
            }
        }
    }
}

/// [`resolve_style`] without the ability to start a load, for callers that
/// only hold `&App` (the hover card). A file no one has loaded yet resolves
/// as still loading.
pub fn resolve_style_readonly(
    source: Option<&StyleSource>,
    search_dirs: &[PathBuf],
    cx: &App,
) -> Option<ResolvedStyle> {
    match source {
        None => style_named(DEFAULT_STYLE).map(|style| ResolvedStyle {
            style,
            problem: None,
        }),
        Some(StyleSource::Bundled(name)) => resolve_bundled(name),
        Some(StyleSource::File(relative)) => {
            let files = StyleFiles::try_global(cx);
            let mut failure: Option<String> = None;
            let mut missing = 0;
            let candidates = style_file_candidates(relative, search_dirs);
            for candidate in &candidates {
                let state = files
                    .as_ref()
                    .and_then(|files| files.read(cx).state(candidate).cloned());
                match state {
                    Some(StyleFileState::Ready(style)) => {
                        return Some(ResolvedStyle {
                            style,
                            problem: None,
                        });
                    }
                    Some(StyleFileState::Failed(problem)) => {
                        failure.get_or_insert(problem);
                    }
                    Some(StyleFileState::Missing) => missing += 1,
                    Some(StyleFileState::Loading) | None => {}
                }
            }
            if let Some(problem) = failure {
                return fallback_for_file(relative, &problem);
            }
            if missing == candidates.len() && !candidates.is_empty() {
                return fallback_for_file(relative, "was not found");
            }
            style_named(DEFAULT_STYLE).map(|style| ResolvedStyle {
                style,
                problem: None,
            })
        }
    }
}

fn plain(children: &ElemChildren) -> String {
    let mut text = String::new();
    children.write_buf(&mut text, BufWriteFormat::Plain).ok();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"
@article{vaswani2017attention,
  title = {Attention Is All You Need},
  author = {Vaswani, Ashish and Shazeer, Noam and Parmar, Niki},
  date = {2017},
  journaltitle = {Advances in Neural Information Processing Systems},
}
@book{knuth1984texbook,
  title = {The {TeX}book},
  author = {Knuth, Donald E.},
  date = {1984},
  publisher = {Addison-Wesley},
}
"#;

    fn library() -> hayagriva::Library {
        hayagriva::io::from_biblatex_str(LIBRARY).expect("the fixture parses")
    }

    #[test]
    fn renders_apa_citation_and_reference() {
        let library = library();
        let entry = library.get("vaswani2017attention").unwrap();
        let style = style_named("apa").expect("apa is bundled");
        let rendered = render_reference(entry, &style).expect("renders");
        assert_eq!(rendered.citation, "(Vaswani et al., 2017)");
        assert!(
            rendered
                .reference
                .starts_with("Vaswani, A., Shazeer, N., & Parmar, N. (2017)."),
            "{}",
            rendered.reference
        );
        assert!(
            rendered.reference.contains("Attention Is All You Need"),
            "{}",
            rendered.reference
        );
    }

    #[test]
    fn reference_list_follows_the_style_order() {
        let library = library();
        let entries = [
            library.get("vaswani2017attention").unwrap(),
            library.get("knuth1984texbook").unwrap(),
        ];
        let apa = style_named("apa").unwrap();
        let keys = render_bibliography(&entries, &apa)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            ["knuth1984texbook", "vaswani2017attention"],
            "APA sorts by author"
        );
        let ieee = style_named("ieee").unwrap();
        let ieee_list = render_bibliography(&entries, &ieee);
        assert_eq!(
            ieee_list[0].0, "vaswani2017attention",
            "IEEE keeps citation order"
        );
        assert!(ieee_list[0].1.starts_with("[1]"), "{}", ieee_list[0].1);
    }

    #[test]
    fn renders_a_document_in_one_pass() {
        let library = library();
        let vaswani = library.get("vaswani2017attention").unwrap();
        let knuth = library.get("knuth1984texbook").unwrap();
        let groups = vec![
            vec![CiteItem {
                entry: knuth,
                locator: Some((Locator::Page, "12".into())),
                form: CiteForm::Normal,
            }],
            vec![
                CiteItem {
                    entry: vaswani,
                    locator: None,
                    form: CiteForm::Normal,
                },
                CiteItem {
                    entry: knuth,
                    locator: None,
                    form: CiteForm::Normal,
                },
            ],
            vec![CiteItem {
                entry: vaswani,
                locator: None,
                form: CiteForm::YearOnly,
            }],
            vec![CiteItem {
                entry: vaswani,
                locator: None,
                form: CiteForm::Prose,
            }],
        ];
        let apa = render_document(&groups, &style_named("apa").unwrap());
        assert_eq!(
            apa.citations,
            [
                "(Knuth, 1984, p. 12)",
                "(Knuth, 1984; Vaswani et al., 2017)",
                "(2017)",
                "Vaswani et al. (2017)"
            ]
        );
        assert_eq!(apa.bibliography.len(), 2);
        let ieee = render_document(&groups, &style_named("ieee").unwrap());
        assert_eq!(ieee.citations[0], "[1, p. 12]");
        assert_eq!(
            ieee.citations[1], "[1], [2]",
            "numbers follow first citation, sorted within a group"
        );
        assert_eq!(ieee.bibliography[0].0, "knuth1984texbook");
    }

    #[test]
    fn parses_pandoc_locators() {
        assert_eq!(parse_cite_suffix(""), CiteSuffix::None);
        assert_eq!(parse_cite_suffix(" , "), CiteSuffix::None);
        assert_eq!(
            parse_cite_suffix(", p. 3"),
            CiteSuffix::Locator(Locator::Page, "3".into())
        );
        assert_eq!(
            parse_cite_suffix(", pp. 3-4"),
            CiteSuffix::Locator(Locator::Page, "3-4".into())
        );
        assert_eq!(
            parse_cite_suffix(", chap. 2"),
            CiteSuffix::Locator(Locator::Chapter, "2".into())
        );
        assert_eq!(
            parse_cite_suffix(", emphasis added"),
            CiteSuffix::Unsupported
        );
        assert_eq!(parse_cite_suffix(", p."), CiteSuffix::Unsupported);
    }

    #[test]
    fn unknown_styles_fall_back_to_apa_and_say_so() {
        let (style, unknown) = style_or_default("apaa").unwrap();
        assert_eq!(style.name, "apa");
        assert_eq!(unknown.as_deref(), Some("apaa"));
        let (style, unknown) = style_or_default("ieee").unwrap();
        assert_eq!(style.name, "ieee");
        assert_eq!(unknown, None);
        let names = style_names();
        assert!(
            names.contains(&"apa") && names.contains(&"ieee"),
            "{names:?}"
        );
        assert!(names.len() > 50);
    }

    #[test]
    fn unknown_styles_are_none_and_names_are_normalized() {
        assert!(style_named("no-such-style").is_none());
        assert!(style_named("APA.csl").is_some());
        assert_eq!(style_named("ieee").unwrap().name, "ieee");
    }

    pub(crate) const TEST_NUMERIC_CSL: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<style xmlns="http://purl.org/net/xbiblio/csl" class="in-text" version="1.0">
  <info>
    <title>Test Numeric</title>
    <id>http://example.com/styles/test-numeric</id>
    <updated>2024-01-01T00:00:00+00:00</updated>
  </info>
  <citation>
    <layout prefix="⟨" suffix="⟩" delimiter=", ">
      <text variable="citation-number"/>
    </layout>
  </citation>
  <bibliography>
    <layout>
      <text variable="citation-number" prefix="⟨" suffix="⟩ "/>
      <names variable="author"><name/></names>
      <text variable="title" prefix=". "/>
    </layout>
  </bibliography>
</style>"#;

    #[test]
    fn parses_a_csl_file_and_renders_with_it() {
        let style = style_from_xml("test-numeric", TEST_NUMERIC_CSL).expect("a valid style");
        assert_eq!(style.name, "test-numeric");
        let library = library();
        let entry = library.get("knuth1984texbook").unwrap();
        let rendered = render_reference(entry, &style).unwrap();
        assert_eq!(rendered.citation, "⟨1⟩");
        assert!(
            rendered.reference.starts_with("⟨1⟩") && rendered.reference.contains("Knuth"),
            "{}",
            rendered.reference
        );
        assert!(style_from_xml("x", "<style>").is_err());
    }

    #[test]
    fn tells_bundled_names_from_files() {
        assert_eq!(
            document_style_source("---\ncsl: ieee\n---\n"),
            Some(StyleSource::Bundled("ieee".into()))
        );
        assert_eq!(
            document_style_source("---\ncsl: styles/journal.csl\n---\n"),
            Some(StyleSource::File("styles/journal.csl".into()))
        );
        assert_eq!(
            document_style_source("---\ncsl: \"journal.csl\"\n---\n"),
            Some(StyleSource::File("journal.csl".into()))
        );
        assert_eq!(
            style_file_candidates(
                "styles/j.csl",
                &[PathBuf::from("/v/notes"), PathBuf::from("/v")]
            ),
            [
                PathBuf::from("/v/notes/styles/j.csl"),
                PathBuf::from("/v/styles/j.csl")
            ]
        );
        assert_eq!(
            style_file_candidates("/abs/j.csl", &[PathBuf::from("/v")]),
            [PathBuf::from("/abs/j.csl")]
        );
    }

    #[test]
    fn reads_the_style_from_frontmatter() {
        assert_eq!(
            document_style("---\ntitle: x\ncsl: ieee\n---\nbody"),
            Some("ieee".into())
        );
        assert_eq!(
            document_style("---\ncsl: \"styles/chicago-author-date.csl\"\n---\n"),
            Some("chicago-author-date".into())
        );
        assert_eq!(
            document_style("---\ncitation-style: apa\n---\n"),
            Some("apa".into())
        );
        assert_eq!(document_style("no frontmatter\ncsl: ieee\n"), None);
        assert_eq!(document_style("---\ntitle: x\n---\ncsl: ieee\n"), None);
    }
}
