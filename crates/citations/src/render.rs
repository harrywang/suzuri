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
    ElemChildren, Entry,
    archive::{ArchivedStyle, locales},
    citationberg::{IndependentStyle, Locale, Style},
};

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

/// The bundled style called `name` as CSL XML.
///
/// Hayagriva renders the preview's own citations from its decoded form, but an
/// external converter speaks CSL and nothing else. Re-serializing the archived
/// style is what lets an exported document carry the same style the preview
/// shows, rather than falling back to whatever default the converter ships.
pub fn style_xml(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches(".csl").to_ascii_lowercase();
    ArchivedStyle::by_name(&name)?.get().to_xml().ok()
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
