//! Formatted citations and reference-list entries, rendered by hayagriva,
//! the CSL engine Typst uses. Styles come from hayagriva's bundled archive
//! (APA, IEEE, Chicago and roughly a hundred more), selected per document
//! through a `csl:` key in the note's frontmatter.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

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

/// The style a document asks for in its frontmatter: `csl: ieee`, or
/// pandoc's `csl: ieee.csl`, or `citation-style: ieee`. Only the leading
/// frontmatter block is read.
pub fn document_style(text: &str) -> Option<String> {
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
            .trim_matches(|character| character == '"' || character == '\'');
        let name = value
            .rsplit('/')
            .next()
            .unwrap_or(value)
            .trim_end_matches(".csl")
            .trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
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
    fn unknown_styles_are_none_and_names_are_normalized() {
        assert!(style_named("no-such-style").is_none());
        assert!(style_named("APA.csl").is_some());
        assert_eq!(style_named("ieee").unwrap().name, "ieee");
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
