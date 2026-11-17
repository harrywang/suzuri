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

use std::collections::HashSet;
use std::ops::Range;
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
/// document, used to resolve relative embeds, and `citation_keys` the keys
/// Pandoc can cite.
pub fn preprocess(
    text: &str,
    source_directory: Option<&Path>,
    vault: &dyn Vault,
    citation_keys: &HashSet<String>,
) -> Preprocessed {
    let mut state = State {
        vault,
        citation_keys,
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
    /// The keys of the library Pandoc formats citations from; empty when
    /// there is none.
    citation_keys: &'a HashSet<String>,
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
    let body = &convert_bracket_math(body);
    let mut output = String::with_capacity(body.len());
    let mut fence: Option<String> = None;
    let mut after_heading = false;
    let mut after_blank = true;
    // The quote prefix of the paragraph the previous line belongs to, if it
    // was paragraph text.
    let mut paragraph_quote: Option<&str> = None;
    let lines: Vec<&str> = body.lines().collect();

    for (index, &line) in lines.iter().enumerate() {
        // Pandoc tries a setext heading before an ATX one, so `# Title`
        // directly above `---` becomes a heading that reads "# Title". The
        // preview reads it as a heading and a rule; a blank line makes
        // Pandoc agree.
        if after_heading && is_setext_underline(line) {
            output.push('\n');
            after_blank = true;
        }
        // The preview lets a list, heading, quote, table, or rule start on
        // the line right after a paragraph, also inside a quote, where Pandoc
        // reads it as more of the paragraph: "Strengths: - one - two".
        let (quote, content) = split_quote_prefix(line).unwrap_or(("", line));
        if fence.is_none()
            && let Some(paragraph_quote) = paragraph_quote
        {
            let depth = quote_depth(quote);
            let paragraph_depth = quote_depth(paragraph_quote);
            let next_content = lines
                .get(index + 1)
                .map(|next| split_quote_prefix(next).map_or(*next, |(_, content)| content));
            let interrupts = depth > paragraph_depth
                || (depth == paragraph_depth
                    && (interrupts_paragraph(content)
                        || next_content.is_some_and(|next| starts_table(content, next))));
            if interrupts {
                output.push_str(paragraph_quote.trim_end());
                output.push('\n');
                after_blank = true;
            }
        }
        paragraph_quote = (fence.is_none() && is_paragraph_text(content)).then_some(quote);
        // Pandoc also reads `---` after a blank line as the start of a YAML
        // metadata block, and fails the export on the prose that follows,
        // unless a blank line comes next. The preview reads a rule.
        let opens_metadata = fence.is_none() && after_blank && line.trim_end() == "---";
        after_heading = fence.is_none() && heading_title(line).is_some();
        after_blank = line.trim().is_empty();

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
        if opens_metadata {
            output.push('\n');
        }
    }
    output
}

/// Rewrites the `\(...\)` and `\[...\]` formulas the preview renders as the
/// `$...$` and `$$...$$` Pandoc reads by default.
///
/// Pandoc's `tex_math_single_backslash` would read them directly, but it
/// takes every `\[` and `\(` for math: the escaped brackets of `see \[1\]`,
/// which the preview leaves as text, and the escapes this pass writes itself
/// (`\(1)` for a list lookalike, `\[` in a link label). Rewriting only what
/// the preview claims leaves every other escape meaning what it says. The
/// rules are the preview's: delimiters pair within a paragraph, outside code,
/// and `\[...\]` counts only alone on its lines.
fn convert_bracket_math(body: &str) -> String {
    if !body.contains(r"\(") && !body.contains(r"\[") {
        return body.to_string();
    }
    let mut output = String::with_capacity(body.len());
    let mut fence: Option<String> = None;
    let mut indented_code = false;
    let mut paragraph = String::new();
    for line in body.split_inclusive('\n') {
        if let Some(open_fence) = &fence {
            if closes_fence(line, open_fence) {
                fence = None;
            }
            output.push_str(line);
            continue;
        }
        if let Some(opened) = opens_fence(line) {
            output.push_str(&convert_paragraph_bracket_math(&std::mem::take(
                &mut paragraph,
            )));
            fence = Some(opened);
            output.push_str(line);
            continue;
        }
        if line.trim().is_empty() {
            output.push_str(&convert_paragraph_bracket_math(&std::mem::take(
                &mut paragraph,
            )));
            indented_code = false;
            output.push_str(line);
            continue;
        }
        // An indented code block, like `    sed 's/\(a\)/\1/'`, is code.
        if paragraph.is_empty() && (line.starts_with("    ") || line.starts_with('\t')) {
            indented_code = true;
        }
        if indented_code {
            output.push_str(line);
            continue;
        }
        paragraph.push_str(line);
    }
    output.push_str(&convert_paragraph_bracket_math(&paragraph));
    output
}

fn convert_paragraph_bracket_math(paragraph: &str) -> String {
    let mut delimiters = Vec::new();
    let mut index = 0;
    while let Some(offset) = paragraph[index..].find(['`', '\\', ']']) {
        let at = index + offset;
        let rest = &paragraph[at..];
        if rest.starts_with('`') {
            index = at + code_span_length(rest).unwrap_or(1);
            continue;
        }
        if rest.starts_with(']') {
            // `Foo_\(bar\)` in a link's destination escapes a URL's
            // parentheses.
            index = at + rest.strip_prefix("](").map_or(1, destination_length);
            continue;
        }
        // A backslash escapes the next character, so `\\(` is a backslash
        // followed by a parenthesis.
        match rest.get(..2) {
            Some(delimiter @ (r"\(" | r"\)" | r"\[" | r"\]")) => {
                delimiters.push((at, delimiter));
            }
            _ => {}
        }
        index = at + rest.chars().take(2).map(char::len_utf8).sum::<usize>();
    }

    let mut replacements = Vec::new();
    let mut open: Option<(usize, &str)> = None;
    for (at, delimiter) in delimiters {
        match (open, delimiter) {
            (Some((start, r"\(")), r"\)") | (Some((start, r"\[")), r"\]") => {
                open = None;
                let source = &paragraph[start + 2..at];
                if source.trim().is_empty() {
                    continue;
                }
                let end = at + 2;
                if delimiter == r"\)" {
                    let mut math = format!("${}$", source.trim());
                    // Pandoc does not close `$` math right before a digit.
                    if paragraph[end..].starts_with(|character: char| character.is_ascii_digit()) {
                        math.push_str("<!-- -->");
                    }
                    replacements.push((start..end, math));
                } else {
                    let line_start = paragraph[..start].rfind('\n').map_or(0, |line| line + 1);
                    let line_end = paragraph[end..]
                        .find('\n')
                        .map_or(paragraph.len(), |line| end + line);
                    if paragraph[line_start..start].trim().is_empty()
                        && paragraph[end..line_end].trim().is_empty()
                    {
                        replacements.push((start..end, format!("$${source}$$")));
                    }
                }
            }
            // A mid-line `\[` is an escaped bracket, never an opener.
            (_, r"\[")
                if !paragraph[..at]
                    .rsplit('\n')
                    .next()
                    .is_none_or(|line| line.trim().is_empty()) => {}
            // A second opener means the first was a stray escape.
            (_, r"\(" | r"\[") => open = Some((at, delimiter)),
            _ => {}
        }
    }

    let mut output = String::with_capacity(paragraph.len());
    let mut copied = 0;
    for (range, replacement) in replacements {
        output.push_str(&paragraph[copied..range.start]);
        output.push_str(&replacement);
        copied = range.end;
    }
    output.push_str(&paragraph[copied..]);
    output
}

/// The byte length of `](` plus the link destination after it, through its
/// closing parenthesis, which may enclose balanced or escaped parentheses.
/// The whole rest when the destination never closes.
fn destination_length(destination: &str) -> usize {
    let mut depth = 0;
    let mut characters = destination.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                characters.next();
            }
            '(' => depth += 1,
            ')' if depth == 0 => return 2 + index + 1,
            ')' => depth -= 1,
            '\n' => return 2 + index,
            _ => {}
        }
    }
    2 + destination.len()
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
fn is_setext_underline(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && (trimmed.chars().all(|character| character == '-')
            || trimmed.chars().all(|character| character == '='))
}

/// Whether `line` starts a block that CommonMark, and so the preview, lets cut
/// a paragraph short without a blank line in between.
fn interrupts_paragraph(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    if indent > 3 {
        return false;
    }
    let trimmed = line.trim();
    // `---` under a paragraph underlines it as a heading, in the preview and
    // Pandoc alike.
    if heading_title(line).is_some()
        || trimmed.starts_with('>')
        || (is_thematic_break(trimmed) && !is_setext_underline(line))
    {
        return true;
    }
    // A nested item already sits inside a list, where Pandoc needs no blank
    // line. CommonMark lets only an ordered list that starts at 1 interrupt a
    // paragraph, so a wrapped line beginning "1984." stays prose.
    indent == 0
        && list_item_content(trimmed)
            .is_some_and(|(ordinal, content)| ordinal.is_none_or(|n| n == 1) && !content.is_empty())
}

fn quote_depth(prefix: &str) -> usize {
    prefix.matches('>').count()
}

/// Whether `line` is a table's header row, which it is when the next line
/// is the delimiter row under it.
fn starts_table(line: &str, next: &str) -> bool {
    let next = next.trim();
    line.contains('|')
        && next.contains('-')
        && next.contains('|')
        && next
            .chars()
            .all(|character| matches!(character, '|' | ':' | '-' | ' ' | '\t'))
}

/// A line that continues a paragraph, as opposed to one that starts or
/// belongs to some other block.
fn is_paragraph_text(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && heading_title(line).is_none()
        && list_item_content(trimmed).is_none()
        && !is_thematic_break(trimmed)
        && !is_setext_underline(line)
        && !trimmed.starts_with(['>', '|', '<'])
        && opens_fence(line).is_none()
}

/// A list item's ordinal (`None` for a bullet) and content.
fn list_item_content(trimmed: &str) -> Option<(Option<u64>, &str)> {
    if let Some(rest) = trimmed.strip_prefix(['-', '*', '+']) {
        return (rest.is_empty() || rest.starts_with([' ', '\t'])).then(|| (None, rest.trim()));
    }
    let digits = trimmed
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = trimmed[digits..].strip_prefix(['.', ')'])?;
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some((trimmed[..digits].parse().ok(), rest.trim()))
}

fn is_thematic_break(trimmed: &str) -> bool {
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    matches!(first, '*' | '_' | '-')
        && trimmed
            .chars()
            .filter(|character| !character.is_whitespace())
            .all(|character| character == first)
        && trimmed
            .chars()
            .filter(|&character| character == first)
            .count()
            >= 3
}

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
    let line = escape_list_lookalike(&line);
    convert_inline(&line, state)
}

/// Pandoc starts a list at `(1)`, `a.`, `iv.`, `B)`, and `#.`, where the
/// preview, like CommonMark, only knows `-`, `*`, `+`, `1.`, and `1)`.
/// Escaping the delimiter keeps the line as the text the preview shows.
fn escape_list_lookalike(line: &str) -> String {
    let (quote, content) = split_quote_prefix(line).unwrap_or(("", line));
    let indent = content.len() - content.trim_start().len();
    // Four spaces in is an indented code block, where a backslash would show.
    if indent > 3 {
        return line.to_string();
    }
    let (leading, marked) = content.split_at(indent);
    let followed_by_space = |rest: &str| rest.is_empty() || rest.starts_with([' ', '\t']);
    let is_ordinal = |label: &str| {
        !label.is_empty()
            && (label.chars().all(|character| character.is_ascii_digit())
                || label.chars().count() == 1
                    && label
                        .chars()
                        .all(|character| character.is_ascii_alphabetic())
                || label
                    .chars()
                    .all(|character| "ivxlcdmIVXLCDM".contains(character)))
    };
    if let Some(inner) = marked.strip_prefix('(')
        && let Some((label, rest)) = inner.split_once(')')
        && is_ordinal(label)
        && followed_by_space(rest)
    {
        return format!("{quote}{leading}\\{marked}");
    }
    let Some(delimiter) = marked.find(['.', ')']) else {
        return line.to_string();
    };
    let label = &marked[..delimiter];
    let numeric = label.chars().all(|character| character.is_ascii_digit());
    if (label == "#" || (is_ordinal(label) && !numeric))
        && followed_by_space(&marked[delimiter + 1..])
    {
        return format!("{quote}{leading}{label}\\{}", &marked[delimiter..]);
    }
    line.to_string()
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
    let with_strikes = double_single_tildes(&with_images);
    let with_citations = escape_unknown_citations(&with_strikes, state.citation_keys);
    escape_tags(&with_citations)
}

/// Escapes the `@` of every citation key the library does not hold. Pandoc
/// would render one as "(key?)", or with no library at all hand Typst a
/// reference to a missing label, which fails the PDF. The preview leaves an
/// unresolved key as the text it is. The boundaries are the preview's: a key
/// starts at the line's start or after whitespace, `;`, `-`, or `[`, which
/// leaves an email's `@` alone.
fn escape_unknown_citations(segment: &str, citation_keys: &HashSet<String>) -> String {
    if !segment.contains('@') {
        return segment.to_string();
    }
    let protected = protected_spans(segment);
    let mut output = String::with_capacity(segment.len());
    let mut copied = 0;
    for (at, _) in segment.match_indices('@') {
        let boundary = segment[..at].chars().last().is_none_or(|character| {
            character.is_whitespace() || matches!(character, ';' | '-' | '[')
        });
        if !boundary || protected.iter().any(|span| span.contains(&at)) {
            continue;
        }
        let key = citation_key(&segment[at + 1..]);
        if key.is_empty() || citation_keys.contains(key) {
            continue;
        }
        output.push_str(&segment[copied..at]);
        output.push('\\');
        copied = at;
    }
    output.push_str(&segment[copied..]);
    output
}

/// The citation key at the start of `text`, by the preview's rules: internal
/// punctuation is allowed, but a key starts and ends with a letter, digit,
/// or underscore.
fn citation_key(text: &str) -> &str {
    let is_word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    if !text.chars().next().is_some_and(is_word) {
        return "";
    }
    let length = text
        .find(|character: char| !(is_word(character) || ":.#$%&-+?<>~/".contains(character)))
        .unwrap_or(text.len());
    text[..length].trim_end_matches(|character: char| !is_word(character))
}

/// The keys of a BibTeX library, from each `@type{key,` entry head.
pub fn bibliography_keys(library: &str) -> HashSet<String> {
    library
        .split('@')
        .skip(1)
        .filter_map(|entry| {
            let (entry_type, rest) = entry.split_once('{')?;
            let entry_type = entry_type.trim().to_ascii_lowercase();
            if matches!(entry_type.as_str(), "comment" | "string" | "preamble")
                || !entry_type
                    .chars()
                    .all(|character| character.is_ascii_alphabetic())
            {
                return None;
            }
            let (key, _) = rest.split_once(',')?;
            let key = key.trim();
            (!key.is_empty() && !key.contains(char::is_whitespace)).then(|| key.to_string())
        })
        .collect()
}

/// The preview strikes through `~text~` as well as `~~text~~`, but Pandoc
/// strikes only the double form and reads the single one as a subscript.
/// Doubling a single tilde only where it pairs up leaves a lone `~5` and a
/// URL's `/~user` alone, and math is skipped because a tilde there is a
/// LaTeX space.
fn double_single_tildes(segment: &str) -> String {
    if !segment.contains('~') {
        return segment.to_string();
    }
    let characters: Vec<(usize, char)> = segment.char_indices().collect();
    let character_at = |index: usize| characters.get(index).map(|&(_, character)| character);
    let protected = protected_spans(segment);
    let is_single_tilde = |index: usize| {
        character_at(index) == Some('~')
            && index
                .checked_sub(1)
                .is_none_or(|previous| character_at(previous) != Some('~'))
            && character_at(index + 1) != Some('~')
            && !protected
                .iter()
                .any(|span| span.contains(&characters[index].0))
    };

    let mut doubled = Vec::new();
    let mut opener: Option<usize> = None;
    for index in 0..characters.len() {
        if !is_single_tilde(index) {
            continue;
        }
        let before = index
            .checked_sub(1)
            .and_then(|previous| character_at(previous));
        let after = character_at(index + 1);
        match opener {
            Some(open) if before.is_some_and(|character| !character.is_whitespace()) => {
                doubled.push(open);
                doubled.push(index);
                opener = None;
            }
            _ => {
                if after.is_some_and(|character| !character.is_whitespace()) && before != Some('/')
                {
                    opener = Some(index);
                }
            }
        }
    }

    let mut output = String::with_capacity(segment.len() + doubled.len());
    for (index, &(_, character)) in characters.iter().enumerate() {
        output.push(character);
        if doubled.contains(&index) {
            output.push('~');
        }
    }
    output
}

/// Byte ranges a tilde rewrite must not touch: `$...$` math and bare URLs.
fn protected_spans(segment: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut search_from = 0;
    while let Some(open_offset) = segment[search_from..].find('$') {
        let open = search_from + open_offset;
        let Some(close_offset) = segment[open + 1..].find('$') else {
            break;
        };
        let close = open + 1 + close_offset;
        spans.push(open..close + 1);
        search_from = close + 1;
    }
    let mut word_start = 0;
    for (index, character) in segment.char_indices().chain([(segment.len(), ' ')]) {
        if character.is_whitespace() {
            let word = &segment[word_start..index];
            if word.contains("://") || word.starts_with("www.") {
                spans.push(word_start..index);
            }
            word_start = index + character.len_utf8();
        }
    }
    spans
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
        preprocess(text, None, &EmptyVault, &HashSet::new()).text
    }

    #[test]
    fn bracket_math_becomes_dollar_math() {
        assert_eq!(
            run("energy \\(E = mc^2\\) here\n"),
            "energy $E = mc^2$ here\n"
        );
        // Pandoc's `$` must hug its formula.
        assert_eq!(run("padded \\( a + b \\) too\n"), "padded $a + b$ too\n");
        // And must not close right before a digit.
        assert_eq!(run("pay \\(x\\)5\n"), "pay $x$<!-- -->5\n");
        assert_eq!(run("\\[\na = b\n\\]\n"), "$$\na = b\n$$\n");
        assert_eq!(run("text\n\n  \\[ a = b \\]\n"), "text\n\n  $$ a = b $$\n");
        // A `\\[2pt]` line break is an escaped backslash, not a delimiter.
        assert_eq!(run("\\[\na \\\\[2pt] b\n\\]\n"), "$$\na \\\\[2pt] b\n$$\n");
    }

    #[test]
    fn escapes_the_preview_shows_as_text_stay_escapes() {
        for text in [
            "see \\[1\\] and \\[citation needed\\]\n",
            "a \\\\(x\\\\) b\n",
            "write `\\(x\\)` for math\n",
            "```\n\\(x\\)\n```\n",
            "open \\( here\n\nclosed \\) there\n",
            "empty \\(  \\) formula\n",
            "[Foo](https://en.wikipedia.org/wiki/Foo_\\(bar\\)) and ![x](a_\\(b\\).png)\n",
            "text\n\n    sed 's/\\(a\\)/\\1/'\n",
        ] {
            assert_eq!(run(text), text);
        }
    }

    #[test]
    fn stray_bracket_escapes_do_not_swallow_later_math() {
        assert_eq!(run("a \\[x] then \\(y\\)\n"), "a \\[x] then $y$\n");
        assert_eq!(run("\\(1) see \\(x\\)\n"), "\\(1) see $x$\n");
        assert_eq!(run("## Solving \\(ax^2\\)\n"), "## Solving $ax^2$\n");
    }

    fn run_in(text: &str, vault: &dyn Vault) -> Preprocessed {
        preprocess(text, None, vault, &HashSet::new())
    }

    fn run_with_keys(text: &str, keys: &[&str]) -> String {
        let keys = keys.iter().map(|key| key.to_string()).collect();
        preprocess(text, None, &EmptyVault, &keys).text
    }

    #[test]
    fn blocks_right_under_a_paragraph_inside_a_quote_and_tables() {
        assert_eq!(run("> Strengths:\n> - one\n"), "> Strengths:\n>\n> - one\n");
        assert_eq!(
            run("Data:\n| a | b |\n|---|---|\n| 1 | 2 |\n"),
            "Data:\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"
        );
        // A pipe in prose with no delimiter row under it is not a table.
        assert_eq!(run("Text\na | b\n"), "Text\na | b\n");
    }

    #[test]
    fn list_markers_only_pandoc_knows_are_escaped() {
        assert_eq!(run("(1) The first claim\n"), "\\(1) The first claim\n");
        assert_eq!(run("a. apple\n"), "a\\. apple\n");
        assert_eq!(run("iv. four\n"), "iv\\. four\n");
        assert_eq!(run("B) b\n"), "B\\) b\n");
        assert_eq!(run("#. item\n"), "#\\. item\n");
        assert_eq!(run("> (a) quoted\n"), "> \\(a) quoted\n");
        // Lists the preview shows as lists, and ordinary sentences, stay.
        assert_eq!(run("1. one\n2) two\n"), "1. one\n2) two\n");
        assert_eq!(run("Hello. World\n"), "Hello. World\n");
        assert_eq!(run("a.b.c is a path\n"), "a.b.c is a path\n");
        assert_eq!(run("Code:\n\n    a. x\n"), "Code:\n\n    a. x\n");
    }

    #[test]
    fn citations_the_library_lacks_are_escaped() {
        assert_eq!(
            run_with_keys(
                "Ping @harry, per @knuth1984 and [@knuth1984; @nope].\n",
                &["knuth1984"]
            ),
            "Ping \\@harry, per @knuth1984 and [@knuth1984; \\@nope].\n"
        );
        assert_eq!(run("See [@knuth1984].\n"), "See [\\@knuth1984].\n");
        assert_eq!(
            run("Mail me@example.com or https://x.org/@user.\n"),
            "Mail me@example.com or https://x.org/@user.\n"
        );
        assert_eq!(run("Code `@harry` here.\n"), "Code `@harry` here.\n");
    }

    #[test]
    fn library_keys_are_read_from_entry_heads() {
        let keys = bibliography_keys(
            "@comment{x}\n@string{s = \"y\"}\n@book{knuth1984,\n  title={T}}\n\
             @Article{ hayashi-2003 ,\n author={me@example.com}}\n",
        );
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort();
        assert_eq!(keys, ["hayashi-2003", "knuth1984"]);
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
        assert_eq!(run_with_keys(source, &["doe2020", "roe2021"]), source);
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
    fn a_rule_under_a_heading_does_not_make_it_a_setext_heading() {
        assert_eq!(
            run("# Title\n---\n## Next\n"),
            "# Title\n\n---\n\n## Next\n"
        );
        assert_eq!(run("Title\n---\n"), "Title\n---\n");
        assert_eq!(
            run("```\n# comment\n---\n```\n"),
            "```\n# comment\n---\n```\n"
        );
    }

    #[test]
    fn a_rule_after_a_blank_line_cannot_open_a_metadata_block() {
        assert_eq!(run("Para\n\n---\n* item\n"), "Para\n\n---\n\n* item\n");
        assert_eq!(run("Para\n---\n"), "Para\n---\n");
        assert_eq!(run("Para\n\n----\nText\n"), "Para\n\n----\nText\n");
    }

    #[test]
    fn a_block_right_under_a_paragraph_gets_its_own_blank_line() {
        assert_eq!(
            run("Strengths:\n- one\n- two\n"),
            "Strengths:\n\n- one\n- two\n"
        );
        assert_eq!(run("Steps:\n1. first\n"), "Steps:\n\n1. first\n");
        assert_eq!(run("Text\n## Heading\n"), "Text\n\n## Heading\n");
        assert_eq!(run("He said:\n> quoted\n"), "He said:\n\n> quoted\n");
        assert_eq!(run("Text\n***\nMore\n"), "Text\n\n***\nMore\n");
    }

    #[test]
    fn lines_the_preview_keeps_in_the_paragraph_are_left_alone() {
        // Only an ordered list starting at 1 may interrupt a paragraph.
        assert_eq!(
            run("The year was\n1984. A good year.\n"),
            "The year was\n1984. A good year.\n"
        );
        // A setext underline, an empty bullet, a nested item, a tag.
        assert_eq!(run("Title\n---\n"), "Title\n---\n");
        assert_eq!(run("Text\n-\n"), "Text\n-\n");
        assert_eq!(
            run("- parent\n  text\n  - child\n"),
            "- parent\n  text\n  - child\n"
        );
        assert_eq!(run("Text\n#tag here\n"), "Text\n\\#tag here\n");
        // Consecutive list items and code are not paragraphs.
        assert_eq!(run("- a\n- b\n"), "- a\n- b\n");
        assert_eq!(
            run("```\ntext\n- not a list\n```\n"),
            "```\ntext\n- not a list\n```\n"
        );
    }

    #[test]
    fn single_tilde_strikethrough_is_doubled_for_pandoc() {
        assert_eq!(run("~gone~ and ~~gone~~\n"), "~~gone~~ and ~~gone~~\n");
        assert_eq!(run("Costs ~5 to ~10.\n"), "Costs ~5 to ~10.\n");
        assert_eq!(
            run("See https://example.com/~a~b now.\n"),
            "See https://example.com/~a~b now.\n"
        );
        assert_eq!(run("Math $a~b~c$ here.\n"), "Math $a~b~c$ here.\n");
        assert_eq!(run("Code `~x~` here.\n"), "Code `~x~` here.\n");
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
