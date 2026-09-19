//! Rewrites Suzuri's markdown into the dialect Pandoc reads.
//!
//! The preview understands a set of Obsidian extensions Pandoc has never heard
//! of, and handed the source unchanged Pandoc does not merely ignore them: a
//! wikilink survives as literal bracket text, a callout flattens into an
//! unmarked quote, and a tag sitting at the start of a line can be read as a
//! heading. This module is the translation pass that runs before the converter
//! sees the document.
//!
//! Two syntaxes are deliberately left alone. Pandoc citations (`[@key]`) are
//! Pandoc's own, which is the reason Suzuri writes them in the first place, and
//! `==highlights==` are handled by enabling Pandoc's `mark` extension rather
//! than by rewriting them here.
//!
//! Transclusions are the one transform that reads other files: `![[Note]]` is
//! replaced by that note's text, recursively, so a paper assembled from section
//! notes exports as one document. Everything else is a local rewrite.

use std::path::{Path, PathBuf};

/// How deep `![[Note]]` embeds may nest before the pass stops descending.
/// Cycles are caught separately and exactly; this only bounds a legitimately
/// deep tree, where the remaining levels are far more likely to be a mistake
/// than an intent.
const MAX_EMBED_DEPTH: usize = 8;

/// The notes an export can resolve `[[...]]` and `![[...]]` against.
///
/// This is a trait rather than a concrete index so the pass can be tested
/// against notes held in memory: the real implementation walks the project's
/// worktrees, which needs an `App` the transform itself has no business
/// holding.
pub trait Vault {
    /// The note `target` names, resolved the way the preview resolves it.
    /// `source_directory` is the folder of the note doing the linking, which
    /// breaks ties in favor of a sibling.
    fn resolve(&self, target: &str, source_directory: Option<&Path>) -> Option<PathBuf>;

    /// The contents of a note previously returned by [`Vault::resolve`].
    fn read(&self, path: &Path) -> Option<String>;
}

/// The result of rewriting one document.
pub struct Preprocessed {
    /// Markdown in Pandoc's dialect.
    pub text: String,
    /// Embed targets that named a note the vault does not hold, in the order
    /// they appeared. The export still succeeds; these are reported so the
    /// user learns which sections are missing from the output rather than
    /// discovering the hole themselves.
    pub missing_embeds: Vec<String>,
}

/// Rewrites `text` for Pandoc. `source_directory` is the folder holding the
/// document, used to resolve relative embeds.
pub fn preprocess(text: &str, source_directory: Option<&Path>, vault: &dyn Vault) -> Preprocessed {
    let mut state = State {
        vault,
        missing_embeds: Vec::new(),
        open: Vec::new(),
    };
    let (frontmatter, body) = split_frontmatter(text);
    let mut output = String::with_capacity(text.len());
    if let Some(frontmatter) = frontmatter {
        output.push_str(frontmatter);
        if !frontmatter.ends_with('\n') {
            output.push('\n');
        }
    }
    output.push_str(&convert_body(body, source_directory, &mut state));
    Preprocessed {
        text: output,
        missing_embeds: state.missing_embeds,
    }
}

struct State<'a> {
    vault: &'a dyn Vault,
    missing_embeds: Vec<String>,
    /// Notes currently being expanded, innermost last. A target already in
    /// here would embed itself.
    open: Vec<PathBuf>,
}

/// Splits a leading YAML or TOML frontmatter block off the document.
///
/// Frontmatter is passed through untouched because Pandoc reads it natively
/// for the title, author, and date, and because rewriting inside it would
/// corrupt values that merely look like markdown. Only the document's own
/// block survives: an embedded note's frontmatter is metadata about that note
/// and has no meaning in the middle of another document.
fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let fence = if text.starts_with("---\n") || text.starts_with("---\r\n") {
        "---"
    } else if text.starts_with("+++\n") || text.starts_with("+++\r\n") {
        "+++"
    } else {
        return (None, text);
    };

    let mut offset = 0;
    let mut lines = text.split_inclusive('\n');
    let Some(opener) = lines.next() else {
        return (None, text);
    };
    offset += opener.len();
    for line in lines {
        let trimmed = line.trim_end();
        offset += line.len();
        if trimmed == fence || (fence == "---" && trimmed == "...") {
            return (Some(&text[..offset]), &text[offset..]);
        }
    }
    (None, text)
}

fn convert_body(body: &str, source_directory: Option<&Path>, state: &mut State) -> String {
    let mut output = String::with_capacity(body.len());
    let mut fence: Option<String> = None;

    for line in body.lines() {
        if let Some(open_fence) = &fence {
            output.push_str(line);
            output.push('\n');
            if closes_fence(line, open_fence) {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = opens_fence(line) {
            fence = Some(opened);
            output.push_str(line);
            output.push('\n');
            continue;
        }

        match note_embed_alone(line) {
            Some((indent, target, section)) => {
                expand_embed(
                    &mut output,
                    indent,
                    target,
                    section,
                    source_directory,
                    state,
                );
            }
            None => {
                output.push_str(&convert_line(line, state));
                output.push('\n');
            }
        }
    }
    output
}

/// The fence a line opens, if it opens one. Returned rather than a bool
/// because a fence closes only on a run of its own character at least as long
/// as the opener, so the opener has to be remembered.
fn opens_fence(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for character in ['`', '~'] {
        let run: String = trimmed
            .chars()
            .take_while(|&found| found == character)
            .collect();
        if run.len() >= 3 {
            return Some(run);
        }
    }
    None
}

fn closes_fence(line: &str, open_fence: &str) -> bool {
    let Some(character) = open_fence.chars().next() else {
        return false;
    };
    let trimmed = line.trim();
    trimmed.chars().all(|found| found == character) && trimmed.len() >= open_fence.len()
}

/// A line that is nothing but a note transclusion, as `(indent, target,
/// section)`. Only a whole-line embed is spliced: one sitting inside a
/// sentence is a reference to a note rather than a request to inline a
/// document, and is rewritten as text instead.
fn note_embed_alone(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let indent_length = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_length);
    let rest = rest.trim_end();
    let inner = rest.strip_prefix("![[")?.strip_suffix("]]")?;
    if inner.contains("[[") || inner.contains("]]") {
        return None;
    }
    // `![[photo.png]]` is an image embed and stays an image; only a note is
    // spliced.
    let (target, _) = split_embed_alias(inner);
    if is_media(target) {
        return None;
    }
    let (target, section) = split_section(target);
    (!target.is_empty()).then_some((indent, target, section))
}

fn expand_embed(
    output: &mut String,
    indent: &str,
    target: &str,
    section: Option<&str>,
    source_directory: Option<&Path>,
    state: &mut State,
) {
    let label = match section {
        Some(section) => format!("{target}#{section}"),
        None => target.to_string(),
    };

    let Some(path) = state.vault.resolve(target, source_directory) else {
        state.missing_embeds.push(label.clone());
        output.push_str(indent);
        output.push_str(&escape_text(&label));
        output.push('\n');
        return;
    };

    if state.open.contains(&path) || state.open.len() >= MAX_EMBED_DEPTH {
        output.push_str(indent);
        output.push_str(&escape_text(&label));
        output.push('\n');
        return;
    }

    let Some(contents) = state.vault.read(&path) else {
        state.missing_embeds.push(label.clone());
        output.push_str(indent);
        output.push_str(&escape_text(&label));
        output.push('\n');
        return;
    };

    let (_, body) = split_frontmatter(&contents);
    let body = match section {
        Some(section) => match section_of(body, section) {
            Some(slice) => slice,
            None => {
                state.missing_embeds.push(label.clone());
                output.push_str(indent);
                output.push_str(&escape_text(&label));
                output.push('\n');
                return;
            }
        },
        None => body.to_string(),
    };

    let nested_directory = path.parent().map(Path::to_path_buf);
    state.open.push(path);
    let converted = convert_body(&body, nested_directory.as_deref(), state);
    state.open.pop();

    // A spliced note is a block, and without blank lines around it its first
    // line would be absorbed into the paragraph above.
    if !output.ends_with("\n\n") && !output.is_empty() {
        output.push('\n');
    }
    for converted_line in converted.lines() {
        output.push_str(indent);
        output.push_str(converted_line);
        output.push('\n');
    }
    output.push('\n');
}

/// The slice of `text` under the heading called `section`, heading included,
/// ending where a heading of the same or higher rank begins.
fn section_of(text: &str, section: &str) -> Option<String> {
    let wanted = section.trim().to_lowercase();
    let (start, level) = text.lines().enumerate().find_map(|(index, line)| {
        let (level, title) = heading_title(line)?;
        (title.to_lowercase() == wanted).then_some((index, level))
    })?;
    let end = text
        .lines()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| heading_title(line).is_some_and(|(found, _)| found <= level))
        .map_or(text.lines().count(), |(index, _)| index);
    Some(
        text.lines()
            .take(end)
            .skip(start)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// An ATX heading's level and title.
fn heading_title(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|&character| character == '#')
        .count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    Some((level, rest.trim()))
}

fn convert_line(line: &str, state: &mut State) -> String {
    let line = convert_callout_header(line);
    convert_inline(&line, state)
}

/// Turns a callout header into a quote whose first line is its title.
///
/// Pandoc has no callout syntax, and the alternatives that survive every
/// output format are worse: a fenced div needs a template to mean anything,
/// and dropping the marker loses the fact that a block was set apart at all.
/// A bold title inside the quote Pandoc already understands carries the intent
/// into Word, PDF, and HTML alike.
fn convert_callout_header(line: &str) -> String {
    let Some((prefix, rest)) = split_quote_prefix(line) else {
        return line.to_string();
    };
    let rest = rest.trim();
    let Some(inner) = rest.strip_prefix("[!") else {
        return line.to_string();
    };
    let Some((kind, after)) = inner.split_once(']') else {
        return line.to_string();
    };
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return line.to_string();
    }
    // The `+`/`-` suffix asks the preview to start expanded or collapsed. A
    // printed document has no such state, so it is simply dropped.
    let title = after.trim_start_matches(['+', '-']).trim();
    let heading = if title.is_empty() {
        title_case(kind)
    } else {
        title.to_string()
    };
    format!("{prefix}**{}**", escape_text(&heading))
}

/// Splits a line's `>` quote markers from its content.
fn split_quote_prefix(line: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    let mut seen_marker = false;
    for (index, character) in line.char_indices() {
        match character {
            '>' => {
                seen_marker = true;
                end = index + character.len_utf8();
            }
            ' ' | '\t' => end = index + character.len_utf8(),
            _ => break,
        }
    }
    seen_marker.then(|| line.split_at(end))
}

fn title_case(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// Rewrites the inline constructs Pandoc would misread, skipping code spans so
/// a note about `[[wikilinks]]` survives being written about.
fn convert_inline(line: &str, state: &mut State) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        match rest.find('`') {
            Some(index) => {
                let (before, from_tick) = rest.split_at(index);
                output.push_str(&convert_inline_segment(before, state));
                match code_span_length(from_tick) {
                    Some(length) => {
                        output.push_str(&from_tick[..length]);
                        rest = &from_tick[length..];
                    }
                    None => {
                        // An unmatched backtick is literal text, so step past
                        // it rather than looping on the same position.
                        output.push('`');
                        rest = &from_tick[1..];
                    }
                }
            }
            None => {
                output.push_str(&convert_inline_segment(rest, state));
                break;
            }
        }
    }
    output
}

/// The byte length of the code span starting at `text`, including both
/// delimiters.
fn code_span_length(text: &str) -> Option<usize> {
    let ticks = text
        .chars()
        .take_while(|&character| character == '`')
        .count();
    let delimiter = &text[..ticks];
    let remainder = &text[ticks..];
    let mut search = 0;
    while let Some(found) = remainder[search..].find(delimiter) {
        let absolute = search + found;
        let after = absolute + delimiter.len();
        let longer = remainder[after..].starts_with('`');
        if !longer {
            return Some(ticks + after);
        }
        search = after;
    }
    None
}

fn convert_inline_segment(segment: &str, state: &mut State) -> String {
    let with_embeds = convert_media_embeds(segment, state);
    let with_links = convert_wikilinks(&with_embeds);
    let with_images = convert_image_widths(&with_links);
    escape_tags(&with_images)
}

/// `![[photo.png]]` and `![[photo.png|320]]` become ordinary image syntax.
/// Non-image attachments become links, because Pandoc would otherwise emit an
/// image element pointing at a video.
fn convert_media_embeds(segment: &str, state: &mut State) -> String {
    rewrite_brackets(segment, "![[", |inner| {
        let (target, alias) = split_embed_alias(inner);
        if target.is_empty() {
            return None;
        }
        if !is_media(target) {
            // A note embedded mid-sentence is a reference, not a splice.
            state.missing_embeds.push(target.to_string());
            return Some(escape_text(target));
        }
        let encoded = encode_target(target);
        if is_image(target) {
            Some(match alias.and_then(parse_width) {
                Some(width) => format!("![]({encoded}){{width={width}}}"),
                None => format!("![]({encoded})"),
            })
        } else {
            let label = alias.unwrap_or(target);
            Some(format!("[{}]({encoded})", escape_text(label)))
        }
    })
}

/// `[[Note]]`, `[[Note|alias]]`, and `[[Note#Heading]]` become plain text.
///
/// A wikilink points at a note in the vault, and the vault does not travel
/// with the exported file. Emitting a link to a `.md` that will not be beside
/// the output produces a reference that is dead everywhere it is opened, so
/// the display text alone is the honest rendering.
fn convert_wikilinks(segment: &str) -> String {
    rewrite_brackets(segment, "[[", |inner| {
        let (target, alias) = split_embed_alias(inner);
        let text = match alias {
            Some(alias) if !alias.trim().is_empty() => alias.trim(),
            _ => split_section(target).0,
        };
        let text = if text.is_empty() { target } else { text };
        (!text.is_empty()).then(|| escape_text(text))
    })
}

/// Rewrites every `opener … ]]` run through `replace`, leaving the text alone
/// where `replace` declines.
fn rewrite_brackets(
    segment: &str,
    opener: &str,
    mut replace: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut output = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(start) = rest.find(opener) {
        // `![[x]]` also contains `[[x]]`; when scanning for the plain form,
        // leave the image form to its own pass.
        if opener == "[[" && rest[..start].ends_with('!') {
            let consumed = start + opener.len();
            output.push_str(&rest[..consumed]);
            rest = &rest[consumed..];
            continue;
        }
        let after = &rest[start + opener.len()..];
        let Some(end) = after.find("]]") else {
            break;
        };
        let inner = &after[..end];
        if inner.contains("[[") {
            let consumed = start + opener.len();
            output.push_str(&rest[..consumed]);
            rest = &rest[consumed..];
            continue;
        }
        match replace(inner) {
            Some(replacement) => {
                output.push_str(&rest[..start]);
                output.push_str(&replacement);
            }
            None => {
                output.push_str(&rest[..start + opener.len() + end + 2]);
            }
        }
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    output
}

/// Obsidian's `![alt|640](path)` width syntax, which Pandoc reads as part of
/// the alt text, becomes Pandoc's own `{width=640}` attribute.
fn convert_image_widths(segment: &str) -> String {
    let mut output = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(start) = rest.find("![") {
        let after = &rest[start + 2..];
        let Some(close) = after.find(']') else {
            break;
        };
        let alt = &after[..close];
        let remainder = &after[close + 1..];
        if !remainder.starts_with('(') {
            output.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        }
        let Some(paren) = remainder.find(')') else {
            break;
        };
        let destination = &remainder[1..paren];
        output.push_str(&rest[..start]);
        match alt
            .rsplit_once('|')
            .and_then(|(text, width)| parse_width(width).map(|width| (text.to_string(), width)))
        {
            Some((text, width)) => {
                output.push_str(&format!("![{text}]({destination}){{width={width}}}"));
            }
            None => output.push_str(&format!("![{alt}]({destination})")),
        }
        rest = &remainder[paren + 1..];
    }
    output.push_str(rest);
    output
}

/// Escapes `#tag` so Pandoc reads it as text.
///
/// Without this a tag opening a line is a heading in every markdown dialect
/// that does not require a space after the hash, which silently promotes a
/// filing label into a section title.
fn escape_tags(segment: &str) -> String {
    let mut output = String::with_capacity(segment.len());
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < segment.len() {
        let character = segment[index..].chars().next().unwrap_or('#');
        if character != '#' {
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        let preceded_by_text = index > 0
            && !matches!(
                bytes.get(index - 1),
                Some(b' ') | Some(b'\t') | Some(b'(') | Some(b'[')
            );
        let tail = &segment[index + 1..];
        if preceded_by_text || !is_tag_body(tail) {
            output.push('#');
            index += 1;
            continue;
        }
        output.push_str("\\#");
        index += 1;
    }
    output
}

/// Whether what follows a `#` reads as a tag: at least one letter, and only
/// characters a tag may contain. A bare `#1` is an issue number, not a tag.
fn is_tag_body(tail: &str) -> bool {
    let name: String = tail
        .chars()
        .take_while(|&character| {
            character.is_alphanumeric() || matches!(character, '_' | '-' | '/')
        })
        .collect();
    !name.is_empty() && name.chars().any(char::is_alphabetic)
}

fn split_embed_alias(inner: &str) -> (&str, Option<&str>) {
    match inner.split_once('|') {
        Some((target, alias)) => (target.trim(), Some(alias)),
        None => (inner.trim(), None),
    }
}

fn split_section(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((note, section)) => (note.trim(), Some(section.trim())),
        None => (target.trim(), None),
    }
}

fn parse_width(value: &str) -> Option<u32> {
    let value = value.trim();
    (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

const IMAGE_EXTENSIONS: &[&str] = &["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"];

const OTHER_MEDIA_EXTENSIONS: &[&str] = &[
    "3gp", "flac", "m4a", "mkv", "mov", "mp3", "mp4", "ogg", "ogv", "pdf", "wav", "webm",
];

fn extension_of(target: &str) -> Option<String> {
    Path::new(target.split('#').next().unwrap_or(target))
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

fn is_image(target: &str) -> bool {
    extension_of(target).is_some_and(|extension| IMAGE_EXTENSIONS.contains(&extension.as_str()))
}

fn is_media(target: &str) -> bool {
    extension_of(target).is_some_and(|extension| {
        IMAGE_EXTENSIONS.contains(&extension.as_str())
            || OTHER_MEDIA_EXTENSIONS.contains(&extension.as_str())
    })
}

/// Percent-encodes the characters that would end a markdown destination early.
fn encode_target(target: &str) -> String {
    target
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

/// Escapes the markdown punctuation in text that came from a link label or a
/// note name, so a note called `A *starred* idea` does not turn into emphasis.
fn escape_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '*' | '_' | '[' | ']' | '`' | '#' | '<' | '>' | '\\' | '~'
        ) {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A vault with nothing in it, for a document that stands alone.
    struct EmptyVault;

    impl Vault for EmptyVault {
        fn resolve(&self, _target: &str, _source_directory: Option<&Path>) -> Option<PathBuf> {
            None
        }

        fn read(&self, _path: &Path) -> Option<String> {
            None
        }
    }

    #[derive(Default)]
    struct MapVault {
        notes: HashMap<String, String>,
    }

    impl MapVault {
        fn with(notes: &[(&str, &str)]) -> Self {
            Self {
                notes: notes
                    .iter()
                    .map(|(name, body)| ((*name).to_string(), (*body).to_string()))
                    .collect(),
            }
        }
    }

    impl Vault for MapVault {
        fn resolve(&self, target: &str, _source_directory: Option<&Path>) -> Option<PathBuf> {
            let wanted = target.trim().trim_end_matches(".md").to_lowercase();
            self.notes
                .keys()
                .find(|name| name.to_lowercase() == wanted)
                .map(|name| PathBuf::from(format!("/vault/{name}.md")))
        }

        fn read(&self, path: &Path) -> Option<String> {
            let stem = path.file_stem()?.to_str()?;
            self.notes.get(stem).cloned()
        }
    }

    fn run(text: &str) -> String {
        preprocess(text, None, &EmptyVault).text
    }

    fn run_in(text: &str, vault: &dyn Vault) -> Preprocessed {
        preprocess(text, None, vault)
    }

    #[test]
    fn wikilinks_become_their_display_text() {
        assert_eq!(run("See [[Attention]].\n"), "See Attention.\n");
        assert_eq!(run("See [[Attention|the paper]].\n"), "See the paper.\n");
        assert_eq!(
            run("See [[Attention#Method]].\n"),
            "See Attention.\n",
            "the section is navigation, not content"
        );
    }

    #[test]
    fn citations_are_left_for_pandoc() {
        let source = "Shown before [@doe2020, p. 3; also @roe2021].\n";
        assert_eq!(run(source), source);
    }

    #[test]
    fn highlights_are_left_for_the_mark_extension() {
        assert_eq!(run("A ==claim== here.\n"), "A ==claim== here.\n");
    }

    #[test]
    fn tags_are_escaped_so_they_do_not_become_headings() {
        assert_eq!(run("#method/qualitative\n"), "\\#method/qualitative\n");
        assert_eq!(
            run("Filed under #method today.\n"),
            "Filed under \\#method today.\n"
        );
    }

    #[test]
    fn headings_and_issue_numbers_are_not_tags() {
        assert_eq!(run("# A Heading\n"), "# A Heading\n");
        assert_eq!(run("### Deeper\n"), "### Deeper\n");
        assert_eq!(run("Closes #42 today.\n"), "Closes #42 today.\n");
    }

    #[test]
    fn callout_headers_become_quoted_titles() {
        assert_eq!(
            run("> [!warning] Mind the gap\n> Body text\n"),
            "> **Mind the gap**\n> Body text\n"
        );
    }

    #[test]
    fn callout_without_a_title_uses_its_kind() {
        assert_eq!(run("> [!note]\n> Body\n"), "> **Note**\n> Body\n");
    }

    #[test]
    fn collapse_suffix_is_dropped() {
        assert_eq!(run("> [!tip]- Later\n"), "> **Later**\n");
        assert_eq!(run("> [!tip]+ Now\n"), "> **Now**\n");
    }

    #[test]
    fn ordinary_quotes_are_untouched() {
        assert_eq!(run("> Just a quote\n"), "> Just a quote\n");
        assert_eq!(run("> [not a callout] text\n"), "> [not a callout] text\n");
    }

    #[test]
    fn image_embeds_become_image_syntax() {
        assert_eq!(run("![[figure.png]]\n"), "![](figure.png)\n");
        assert_eq!(run("![[figure.png|320]]\n"), "![](figure.png){width=320}\n");
    }

    #[test]
    fn non_image_attachments_become_links() {
        assert_eq!(run("![[paper.pdf]]\n"), "[paper.pdf](paper.pdf)\n");
    }

    #[test]
    fn obsidian_image_widths_become_pandoc_attributes() {
        assert_eq!(
            run("![A figure|640](figure.png)\n"),
            "![A figure](figure.png){width=640}\n"
        );
        assert_eq!(
            run("![Plain alt](figure.png)\n"),
            "![Plain alt](figure.png)\n"
        );
    }

    #[test]
    fn spaces_in_embed_targets_are_encoded() {
        assert_eq!(run("![[my figure.png]]\n"), "![](my%20figure.png)\n");
    }

    #[test]
    fn code_spans_are_left_alone() {
        assert_eq!(
            run("Write `[[Note]]` and `#tag` literally.\n"),
            "Write `[[Note]]` and `#tag` literally.\n"
        );
    }

    #[test]
    fn fenced_code_is_left_alone() {
        let source = "```md\n[[Note]]\n#tag\n> [!note] Title\n```\n";
        assert_eq!(run(source), source);
    }

    #[test]
    fn tilde_fences_are_left_alone() {
        let source = "~~~\n[[Note]]\n~~~\n";
        assert_eq!(run(source), source);
    }

    #[test]
    fn frontmatter_passes_through_untouched() {
        let source = "---\ntitle: A #hash and [[brackets]]\ncsl: ieee\n---\n\nBody [[Note]].\n";
        let output = run(source);
        assert!(output.starts_with("---\ntitle: A #hash and [[brackets]]\ncsl: ieee\n---\n"));
        assert!(output.ends_with("Body Note.\n"));
    }

    #[test]
    fn transclusion_splices_the_target_note() {
        let vault = MapVault::with(&[("Method", "## Method\n\nWe did the thing.\n")]);
        let output = run_in("# Paper\n\n![[Method]]\n", &vault);
        assert!(output.text.contains("## Method"));
        assert!(output.text.contains("We did the thing."));
        assert!(!output.text.contains("![["));
        assert!(output.missing_embeds.is_empty());
    }

    #[test]
    fn transclusion_can_take_one_section() {
        let vault = MapVault::with(&[(
            "Notes",
            "## Keep\n\nKept text.\n\n## Drop\n\nDropped text.\n",
        )]);
        let output = run_in("![[Notes#Keep]]\n", &vault);
        assert!(output.text.contains("Kept text."));
        assert!(!output.text.contains("Dropped text."));
    }

    #[test]
    fn transclusion_drops_the_embedded_notes_frontmatter() {
        let vault = MapVault::with(&[("Method", "---\ntitle: Method\n---\n\nBody.\n")]);
        let output = run_in("![[Method]]\n", &vault);
        assert!(!output.text.contains("title: Method"));
        assert!(output.text.contains("Body."));
    }

    #[test]
    fn transclusion_is_recursive() {
        let vault = MapVault::with(&[("Outer", "![[Inner]]\n"), ("Inner", "Deepest text.\n")]);
        let output = run_in("![[Outer]]\n", &vault);
        assert!(output.text.contains("Deepest text."));
    }

    #[test]
    fn a_cycle_does_not_recurse_forever() {
        let vault = MapVault::with(&[("A", "![[B]]\n"), ("B", "![[A]]\n")]);
        let output = run_in("![[A]]\n", &vault);
        assert!(output.text.contains('A'));
    }

    #[test]
    fn a_note_embedding_itself_does_not_recurse_forever() {
        let vault = MapVault::with(&[("Self", "Before\n\n![[Self]]\n\nAfter\n")]);
        let output = run_in("![[Self]]\n", &vault);
        assert!(output.text.contains("Before"));
        assert!(output.text.contains("After"));
    }

    #[test]
    fn a_missing_embed_is_reported_rather_than_silently_dropped() {
        let output = run_in("![[Nowhere]]\n", &EmptyVault);
        assert_eq!(output.missing_embeds, vec!["Nowhere".to_string()]);
        assert!(output.text.contains("Nowhere"));
    }

    #[test]
    fn a_missing_section_is_reported() {
        let vault = MapVault::with(&[("Notes", "## Present\n\nText.\n")]);
        let output = run_in("![[Notes#Absent]]\n", &vault);
        assert_eq!(output.missing_embeds, vec!["Notes#Absent".to_string()]);
    }

    #[test]
    fn note_names_with_markdown_punctuation_are_escaped() {
        assert_eq!(run("[[A *starred* note]]\n"), "A \\*starred\\* note\n");
    }

    #[test]
    fn an_unmatched_backtick_does_not_hang() {
        assert_eq!(
            run("A ` stray tick and [[Note]].\n"),
            "A ` stray tick and Note.\n"
        );
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(run(""), "");
    }
}
