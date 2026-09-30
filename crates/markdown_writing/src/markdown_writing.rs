//! The small commands writers reach for first: bold, italic, strikethrough,
//! inline code and links as toggles, and a word count in the status bar.
//!
//! Live preview hides the asterisks a note is written in, which makes typing
//! them by hand feel broken rather than minimal. These commands edit the
//! markdown source, so the note stays plain text that every other tool reads.

use std::{any::TypeId, ops::Range};

use editor::Editor;
use gpui::{App, Context, DispatchPhase, Window, actions};
use language::LanguageName;
use multi_buffer::MultiBufferOffset;
use util::ResultExt as _;

mod word_count;

pub use word_count::{WordCount, WordCountSettings};

actions!(
    markdown,
    [
        /// Makes the selection or the word under the cursor bold, or removes
        /// the bold it already has.
        ToggleBold,
        /// Makes the selection or the word under the cursor italic, or removes
        /// the italic it already has.
        ToggleItalic,
        /// Strikes through the selection or the word under the cursor, or
        /// removes the strikethrough it already has.
        ToggleStrikethrough,
        /// Formats the selection or the word under the cursor as inline code,
        /// or removes the code span it is already in.
        ToggleInlineCode,
        /// Turns the selection into a link, or a link back into its text.
        InsertLink,
        /// Makes the lines in the selection a level 1 heading, or body text
        /// if they already are one.
        ToggleHeading1,
        /// Makes the lines in the selection a level 2 heading, or body text
        /// if they already are one.
        ToggleHeading2,
        /// Makes the lines in the selection a level 3 heading, or body text
        /// if they already are one.
        ToggleHeading3,
        /// Makes the lines in the selection a level 4 heading, or body text
        /// if they already are one.
        ToggleHeading4,
        /// Makes the lines in the selection a level 5 heading, or body text
        /// if they already are one.
        ToggleHeading5,
        /// Makes the lines in the selection a level 6 heading, or body text
        /// if they already are one.
        ToggleHeading6,
        /// Turns the headings in the selection back into body text.
        ClearHeading,
    ]
);

const MARKDOWN: &str = "Markdown";

pub fn init(cx: &mut App) {
    cx.observe_new(register_editor).detach();
}

fn register_editor(editor: &mut Editor, _window: Option<&mut Window>, _cx: &mut Context<Editor>) {
    if !editor.mode().is_full() {
        return;
    }
    // Registered on every full editor rather than only markdown ones: a
    // buffer's language is often detected after its editor is created, and a
    // note opened that way would otherwise never get the commands. The
    // listeners are attached per render instead, and only while the buffer is
    // markdown, so the Format menu greys out everywhere else.
    register::<ToggleBold>(editor, |text, selection| {
        toggle_emphasis(text, selection, Emphasis::Bold)
    });
    register::<ToggleItalic>(editor, |text, selection| {
        toggle_emphasis(text, selection, Emphasis::Italic)
    });
    register::<ToggleStrikethrough>(editor, |text, selection| {
        toggle_emphasis(text, selection, Emphasis::Strikethrough)
    });
    register::<ToggleInlineCode>(editor, |text, selection| {
        toggle_emphasis(text, selection, Emphasis::Code)
    });
    register::<InsertLink>(editor, toggle_link);
    register::<ToggleHeading1>(editor, |text, selection| set_heading(text, selection, 1));
    register::<ToggleHeading2>(editor, |text, selection| set_heading(text, selection, 2));
    register::<ToggleHeading3>(editor, |text, selection| set_heading(text, selection, 3));
    register::<ToggleHeading4>(editor, |text, selection| set_heading(text, selection, 4));
    register::<ToggleHeading5>(editor, |text, selection| set_heading(text, selection, 5));
    register::<ToggleHeading6>(editor, |text, selection| set_heading(text, selection, 6));
    register::<ClearHeading>(editor, |text, selection| set_heading(text, selection, 0));
}

fn register<A: gpui::Action>(editor: &mut Editor, transform: fn(&str, Range<usize>) -> Change) {
    editor
        .register_action_renderer(move |editor, window, cx| {
            if !is_markdown(editor, cx) {
                return;
            }
            let weak_editor = cx.weak_entity();
            window.on_action(TypeId::of::<A>(), move |_, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                weak_editor
                    .update(cx, |editor, cx| apply(editor, transform, window, cx))
                    .log_err();
            });
        })
        .detach();
}

pub(crate) fn is_markdown(editor: &Editor, cx: &App) -> bool {
    editor
        .buffer()
        .read(cx)
        .as_singleton()
        .and_then(|buffer| buffer.read(cx).language().map(|language| language.name()))
        .is_some_and(|name| name == LanguageName::new(MARKDOWN))
}

/// Applies `transform` to every selection as one undoable edit.
fn apply(
    editor: &mut Editor,
    transform: fn(&str, Range<usize>) -> Change,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let text = snapshot.text();
    let display = editor.display_snapshot(cx);
    let selections = editor.selections.all::<MultiBufferOffset>(&display);

    let mut changes: Vec<Change> = selections
        .iter()
        .map(|selection| transform(&text, selection.start.0..selection.end.0))
        .collect();
    changes.sort_by_key(|change| change.range.start);
    // Two cursors in the same word would produce overlapping edits; the
    // first one wins rather than the buffer receiving a contradiction.
    let mut kept: Vec<Change> = Vec::with_capacity(changes.len());
    for change in changes {
        if kept
            .last()
            .is_none_or(|previous| previous.range.end <= change.range.start)
        {
            kept.push(change);
        }
    }

    let mut new_selections = Vec::with_capacity(kept.len());
    let mut shift: isize = 0;
    for change in &kept {
        let start = (change.range.start as isize + shift) as usize;
        new_selections.push(
            MultiBufferOffset(start + change.selection.start)
                ..MultiBufferOffset(start + change.selection.end),
        );
        shift += change.text.len() as isize - change.range.len() as isize;
    }
    let edits: Vec<(Range<MultiBufferOffset>, String)> = kept
        .into_iter()
        .map(|change| {
            (
                MultiBufferOffset(change.range.start)..MultiBufferOffset(change.range.end),
                change.text,
            )
        })
        .collect();

    editor.transact(window, cx, |editor, window, cx| {
        editor.edit(edits, cx);
        editor.change_selections(Default::default(), window, cx, |selections| {
            selections.select_ranges(new_selections);
        });
    });
}

/// One replacement in the source: `range` becomes `text`, and `selection` is
/// where the selection lands, relative to the start of `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    range: Range<usize>,
    text: String,
    selection: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Emphasis {
    Bold,
    Italic,
    Strikethrough,
    Code,
}

impl Emphasis {
    /// What this command writes. Underscore forms are recognized when
    /// removing emphasis but never written, since `_` inside a word is not
    /// emphasis in CommonMark and `*` always is.
    fn marker(self) -> &'static str {
        match self {
            Emphasis::Bold => "**",
            Emphasis::Italic => "*",
            Emphasis::Strikethrough => "~~",
            Emphasis::Code => "`",
        }
    }

    /// Whether `count` copies of `character` on both sides of some text
    /// already apply this emphasis. Three stars mean bold and italic at once,
    /// so they count as both.
    fn applied_by(self, character: char, count: usize) -> bool {
        match self {
            Emphasis::Bold => matches!(character, '*' | '_') && count >= 2,
            Emphasis::Italic => matches!(character, '*' | '_') && (count == 1 || count == 3),
            Emphasis::Strikethrough => character == '~' && count >= 2,
            Emphasis::Code => character == '`' && count >= 1,
        }
    }

    /// How many delimiter characters to strip from each side when removing
    /// this emphasis from a run of `count`.
    fn removal(self, count: usize) -> usize {
        match self {
            Emphasis::Bold | Emphasis::Strikethrough => 2,
            Emphasis::Italic => 1,
            Emphasis::Code => count,
        }
    }
}

const DELIMITERS: [char; 4] = ['*', '_', '~', '`'];

/// The run of one delimiter character that `text` starts with, capped at
/// three, which is as deep as markdown emphasis nests.
fn leading_run(text: &str) -> Option<(char, usize)> {
    let first = text.chars().next().filter(|c| DELIMITERS.contains(c))?;
    let count = text.chars().take_while(|&c| c == first).take(3).count();
    Some((first, count))
}

fn trailing_run(text: &str) -> Option<(char, usize)> {
    let last = text
        .chars()
        .next_back()
        .filter(|c| DELIMITERS.contains(c))?;
    let count = text
        .chars()
        .rev()
        .take_while(|&c| c == last)
        .take(3)
        .count();
    Some((last, count))
}

/// A matched pair of delimiter runs around some text: the character and how
/// many of it are on both sides.
fn enclosing(before: &str, after: &str) -> Option<(char, usize)> {
    let (left, left_count) = trailing_run(before)?;
    let (right, right_count) = leading_run(after)?;
    (left == right).then_some((left, left_count.min(right_count)))
}

fn toggle_emphasis(text: &str, selection: Range<usize>, emphasis: Emphasis) -> Change {
    if selection.is_empty() {
        return toggle_at_cursor(text, selection.start, emphasis);
    }
    toggle_range(text, selection, emphasis)
}

fn toggle_range(text: &str, selection: Range<usize>, emphasis: Emphasis) -> Change {
    // Whitespace at the edges is never part of the formatting: select-all and
    // triple-click both take a trailing newline, which would otherwise hide
    // the markers the selection is wrapped in.
    let untrimmed = &text[selection.clone()];
    let leading = untrimmed.len() - untrimmed.trim_start().len();
    if leading == untrimmed.len() {
        return insert_pair(selection.end, emphasis.marker(), emphasis.marker());
    }
    let trailing = untrimmed.len() - untrimmed.trim_end().len();
    let selection = selection.start + leading..selection.end - trailing;
    let selected = &text[selection.clone()];

    // The markers are part of the selection: `**word**` selected whole.
    if let Some((character, count)) = enclosing(selected, selected)
        && emphasis.applied_by(character, count)
    {
        let strip = emphasis.removal(count);
        if selected.len() >= strip * 2 {
            let inner = selected[strip..selected.len() - strip].to_string();
            let length = inner.len();
            return Change {
                range: selection,
                text: inner,
                selection: 0..length,
            };
        }
    }

    // The markers sit just outside it: `word` selected inside `**word**`.
    if let Some((character, count)) = enclosing(&text[..selection.start], &text[selection.end..])
        && emphasis.applied_by(character, count)
    {
        let strip = emphasis.removal(count);
        return Change {
            range: selection.start - strip..selection.end + strip,
            text: selected.to_string(),
            selection: 0..selected.len(),
        };
    }

    wrap(text, selection, emphasis.marker(), emphasis.marker())
}

/// Wraps a selection in `open`/`close`. Whitespace at its edges stays outside
/// the markers, because `**word **` is not emphasis in markdown and a
/// double-click often takes the trailing space along. A selection spanning a
/// blank line is wrapped paragraph by paragraph, since emphasis cannot cross
/// one.
fn wrap(text: &str, selection: Range<usize>, open: &str, close: &str) -> Change {
    let selected = &text[selection.clone()];
    let trimmed_start = selected.len() - selected.trim_start().len();
    let trimmed_end = selected.len() - selected.trim_end().len();
    if trimmed_start == selected.len() {
        return insert_pair(selection.end, open, close);
    }
    let range = selection.start + trimmed_start..selection.end - trimmed_end;
    let inner = &text[range.clone()];
    let wrapped = wrap_paragraphs(inner, open, close);
    let length = wrapped.len();
    let selection_in_text = if inner.contains("\n\n") || inner.contains("\n\r\n") {
        0..length
    } else {
        open.len()..length - close.len()
    };
    Change {
        range,
        text: wrapped,
        selection: selection_in_text,
    }
}

fn wrap_paragraphs(inner: &str, open: &str, close: &str) -> String {
    let lines: Vec<&str> = inner.split('\n').collect();
    let mut output = String::with_capacity(inner.len() + open.len() + close.len());
    let mut index = 0;
    while index < lines.len() {
        if index > 0 {
            output.push('\n');
        }
        if lines[index].trim().is_empty() {
            output.push_str(lines[index]);
            index += 1;
            continue;
        }
        let first = index;
        while index < lines.len() && !lines[index].trim().is_empty() {
            index += 1;
        }
        let paragraph = lines[first..index].join("\n");
        let leading = paragraph.len() - paragraph.trim_start().len();
        let trailing = paragraph.len() - paragraph.trim_end().len();
        output.push_str(&paragraph[..leading]);
        output.push_str(open);
        output.push_str(&paragraph[leading..paragraph.len() - trailing]);
        output.push_str(close);
        output.push_str(&paragraph[paragraph.len() - trailing..]);
    }
    output
}

fn insert_pair(offset: usize, open: &str, close: &str) -> Change {
    Change {
        range: offset..offset,
        text: format!("{open}{close}"),
        selection: open.len()..open.len(),
    }
}

/// With no selection a command applies to the word under the cursor, the
/// way word processors behave, and keeps the cursor where it was in that
/// word. Between two empty markers it removes them, so pressing the key twice
/// undoes itself. Anywhere else it inserts an empty pair to type into.
fn toggle_at_cursor(text: &str, offset: usize, emphasis: Emphasis) -> Change {
    if let Some((character, count)) = enclosing(&text[..offset], &text[offset..])
        && emphasis.applied_by(character, count)
    {
        let strip = emphasis.removal(count);
        let outside_before = text[..offset - strip].chars().next_back();
        let outside_after = text[offset + strip..].chars().next();
        // `**a**|**b**` has markers on both sides too, but they close one span
        // and open another; only a pair with nothing inside is removed.
        if !outside_before.is_some_and(is_word_character)
            && !outside_after.is_some_and(is_word_character)
        {
            return Change {
                range: offset - strip..offset + strip,
                text: String::new(),
                selection: 0..0,
            };
        }
    }

    if let Some(word) = word_at(text, offset) {
        let cursor = offset - word.start;
        let change = toggle_range(text, word, emphasis);
        // Place the cursor at the same spot in the word, wherever the
        // markers moved it.
        let word_start_in_text = change.selection.start;
        let position = word_start_in_text + cursor;
        return Change {
            selection: position..position,
            ..change
        };
    }

    insert_pair(offset, emphasis.marker(), emphasis.marker())
}

pub(crate) fn is_cjk(character: char) -> bool {
    matches!(character as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FA1F)
}

/// Letters and digits outside CJK scripts. CJK text has no spaces between
/// words, so treating its characters as word characters would make "the word
/// under the cursor" the whole sentence.
fn is_word_character(character: char) -> bool {
    (character.is_alphanumeric() || character == '_') && !is_cjk(character)
}

fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let before_word = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_word_character(*character))
        .last()
        .map_or(offset, |(index, _)| index);
    let after_word = text[offset..]
        .char_indices()
        .take_while(|(_, character)| is_word_character(*character))
        .last()
        .map_or(offset, |(index, character)| {
            offset + index + character.len_utf8()
        });
    (before_word < after_word).then_some(before_word..after_word)
}

fn is_url(text: &str) -> bool {
    let text = text.trim();
    !text.contains(char::is_whitespace)
        && (text.starts_with("http://")
            || text.starts_with("https://")
            || text.starts_with("www.")
            || text.starts_with("mailto:"))
}

/// The label of a markdown link that makes up all of `text`.
fn link_label(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('[')?.strip_suffix(')')?;
    let (label, destination) = inner.split_once("](")?;
    (!label.contains(']') && !destination.contains(')')).then_some(label)
}

fn toggle_link(text: &str, selection: Range<usize>) -> Change {
    let target = if selection.is_empty() {
        word_at(text, selection.start)
    } else {
        Some(selection.clone())
    };
    let Some(target) = target else {
        return Change {
            range: selection.start..selection.start,
            text: "[]()".to_string(),
            selection: 1..1,
        };
    };

    let selected = &text[target.clone()];
    let trimmed_start = selected.len() - selected.trim_start().len();
    let trimmed_end = selected.len() - selected.trim_end().len();
    if trimmed_start == selected.len() {
        return Change {
            range: target.end..target.end,
            text: "[]()".to_string(),
            selection: 1..1,
        };
    }
    let range = target.start + trimmed_start..target.end - trimmed_end;
    let content = &text[range.clone()];

    if let Some(label) = link_label(content) {
        return Change {
            range,
            text: label.to_string(),
            selection: 0..label.len(),
        };
    }
    if is_url(content) {
        return Change {
            range,
            text: format!("[]({content})"),
            selection: 1..1,
        };
    }
    // The cursor goes where the destination is typed next.
    let destination = content.len() + 3;
    Change {
        range,
        text: format!("[{content}]()"),
        selection: destination..destination,
    }
}

/// The length of an ATX heading's prefix (up to three spaces of indent, the
/// `#` run and the whitespace after it) and its level, or `(0, 0)` for a line
/// that is not a heading. A heading needs the space after its `#` run, so a
/// `#tag` at the start of a line is left alone.
fn heading_prefix(line: &str) -> (usize, usize) {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return (0, 0);
    }
    let after_indent = &line[indent..];
    let level = after_indent.len() - after_indent.trim_start_matches('#').len();
    let after_hashes = &after_indent[level..];
    let spacing = after_hashes.len() - after_hashes.trim_start_matches([' ', '\t']).len();
    if !(1..=6).contains(&level) || (spacing == 0 && !after_hashes.is_empty()) {
        return (0, 0);
    }
    (indent + level + spacing, level)
}

/// Gives every line the selection touches a level `level` heading, or body
/// text when `level` is 0. Lines that all have that level already become body
/// text instead, the way the emphasis commands toggle.
fn set_heading(text: &str, selection: Range<usize>, level: usize) -> Change {
    let start = text[..selection.start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    // A selection that ends at the start of a line, as a triple-click leaves
    // it, does not take that line along.
    let last = if selection.end > start && text[..selection.end].ends_with('\n') {
        selection.end - 1
    } else {
        selection.end
    };
    let end = text[last..]
        .find('\n')
        .map_or(text.len(), |index| last + index);
    let lines: Vec<&str> = text[start..end].split('\n').collect();
    // Across several lines, blank ones stay blank rather than becoming empty
    // headings; a blank line on its own gets the prefix to type after.
    let single_line = lines.len() == 1;
    let is_target = |line: &str| single_line || !line.trim().is_empty();
    let already = level > 0
        && lines
            .iter()
            .filter(|line| is_target(line))
            .all(|line| heading_prefix(line).1 == level);
    let prefix = if level == 0 || already {
        String::new()
    } else {
        format!("{} ", "#".repeat(level))
    };

    let mut output = String::with_capacity(end - start + lines.len() * prefix.len());
    // Each line's start before and after the edit, with its prefix length
    // before and after, so the selection can be carried across.
    let mut line_maps = Vec::with_capacity(lines.len());
    let mut old_line_start = start;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let (old_prefix, new_prefix) = if is_target(line) {
            (heading_prefix(line).0, prefix.as_str())
        } else {
            (0, "")
        };
        line_maps.push((old_line_start, old_prefix, output.len(), new_prefix.len()));
        output.push_str(new_prefix);
        output.push_str(&line[old_prefix..]);
        old_line_start += line.len() + 1;
    }

    let map = |offset: usize| {
        if offset > end {
            return output.len() + offset - end;
        }
        let (old_line_start, old_prefix, new_line_start, new_prefix) = line_maps
            .iter()
            .rev()
            .find(|(old_line_start, ..)| *old_line_start <= offset)
            .copied()
            .unwrap_or((start, 0, 0, 0));
        // A cursor inside the old prefix lands just after the new one.
        new_line_start + new_prefix + offset.saturating_sub(old_line_start + old_prefix)
    };
    Change {
        range: start..end,
        selection: map(selection.start)..map(selection.end),
        text: output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Format menu greys an item out when `is_action_available` finds no
    /// listener, and a buffer's language can arrive after its editor exists.
    #[gpui::test]
    async fn format_commands_are_available_only_in_markdown(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            init(cx);
        });
        let mut cx = editor::test::editor_test_context::EditorTestContext::new(cx).await;
        cx.set_state("ˇplain text");
        let bold_available = |cx: &mut editor::test::editor_test_context::EditorTestContext| {
            cx.executor().run_until_parked();
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                window.is_action_available(&ToggleBold, cx)
            })
        };
        assert!(!bold_available(&mut cx), "a buffer with no language");

        cx.update_buffer(|buffer, cx| buffer.set_language(Some(language::markdown_lang()), cx));
        assert!(bold_available(&mut cx), "the buffer became markdown");
    }

    /// Applies a change to `text` and renders the selection as `«…»`, or `ˇ`
    /// for a cursor, so a test reads as before and after.
    fn run(marked: &str, transform: impl Fn(&str, Range<usize>) -> Change) -> String {
        let (text, selection) = parse(marked);
        let change = transform(&text, selection);
        let mut result = text;
        result.replace_range(change.range.clone(), &change.text);
        let start = change.range.start + change.selection.start;
        let end = change.range.start + change.selection.end;
        if start == end {
            result.insert(start, 'ˇ');
        } else {
            result.insert(end, '»');
            result.insert(start, '«');
        }
        result
    }

    fn parse(marked: &str) -> (String, Range<usize>) {
        if let Some(cursor) = marked.find('ˇ') {
            return (marked.replacen('ˇ', "", 1), cursor..cursor);
        }
        let start = marked.find('«').expect("a selection or cursor marker");
        let without_open = marked.replacen('«', "", 1);
        let end = without_open.find('»').expect("a closing selection marker");
        (without_open.replacen('»', "", 1), start..end)
    }

    fn bold(marked: &str) -> String {
        run(marked, |text, selection| {
            toggle_emphasis(text, selection, Emphasis::Bold)
        })
    }

    fn italic(marked: &str) -> String {
        run(marked, |text, selection| {
            toggle_emphasis(text, selection, Emphasis::Italic)
        })
    }

    fn link(marked: &str) -> String {
        run(marked, toggle_link)
    }

    fn heading(level: usize, marked: &str) -> String {
        run(marked, |text, selection| {
            set_heading(text, selection, level)
        })
    }

    #[test]
    fn heading_prefixes_the_line_and_keeps_the_cursor_in_its_text() {
        assert_eq!(heading(2, "Heˇllo"), "## Heˇllo");
        assert_eq!(heading(2, "a\nHeˇllo\nb"), "a\n## Heˇllo\nb");
        assert_eq!(heading(3, "ˇ"), "### ˇ");
    }

    #[test]
    fn heading_changes_an_existing_level_and_the_same_level_toggles_it_off() {
        assert_eq!(heading(1, "### Heˇllo"), "# Heˇllo");
        assert_eq!(heading(2, "## Heˇllo"), "Heˇllo");
        assert_eq!(heading(0, "  ##   Heˇllo"), "Heˇllo");
        assert_eq!(heading(0, "plain ˇtext"), "plain ˇtext");
    }

    #[test]
    fn a_cursor_inside_the_old_prefix_lands_after_the_new_one() {
        assert_eq!(heading(1, "##ˇ Title"), "# ˇTitle");
        assert_eq!(heading(0, "#ˇ# Title"), "ˇTitle");
    }

    #[test]
    fn tags_and_over_long_runs_are_not_headings() {
        assert_eq!(heading(1, "#taˇg"), "# #taˇg");
        assert_eq!(heading(0, "####### sevˇen"), "####### sevˇen");
    }

    #[test]
    fn a_selection_sets_every_line_it_touches_and_skips_blank_ones() {
        assert_eq!(heading(2, "«one\n\ntwo»"), "## «one\n\n## two»");
        assert_eq!(heading(2, "«# one\n## two»"), "## «one\n## two»");
        assert_eq!(heading(2, "«## one\n## two»"), "«one\ntwo»");
        assert_eq!(heading(1, "«one\n»two"), "# «one\n»two");
    }

    #[test]
    fn bold_wraps_a_selection_and_keeps_it_selected() {
        assert_eq!(bold("a «word» here"), "a **«word»** here");
    }

    #[test]
    fn bold_removes_markers_just_outside_the_selection() {
        assert_eq!(bold("a **«word»** here"), "a «word» here");
    }

    #[test]
    fn bold_removes_markers_inside_the_selection() {
        assert_eq!(bold("a «**word**» here"), "a «word» here");
        assert_eq!(bold("a «__word__» here"), "a «word» here");
    }

    #[test]
    fn whitespace_at_the_edges_stays_outside_the_markers() {
        assert_eq!(bold("a «word »here"), "a **«word»** here");
    }

    #[test]
    fn a_selection_taking_the_trailing_newline_still_finds_the_markers() {
        assert_eq!(bold("«**hello world**\n»"), "«hello world»\n");
        assert_eq!(bold("«hello world\n»"), "**«hello world»**\n");
    }

    #[test]
    fn bold_with_no_selection_applies_to_the_word_under_the_cursor() {
        assert_eq!(bold("a woˇrd here"), "a **woˇrd** here");
        assert_eq!(bold("a **woˇrd** here"), "a woˇrd here");
    }

    #[test]
    fn bold_between_words_inserts_a_pair_and_a_second_press_removes_it() {
        assert_eq!(bold("a ˇ here"), "a **ˇ** here");
        assert_eq!(bold("a **ˇ** here"), "a ˇ here");
    }

    #[test]
    fn adjacent_spans_are_not_mistaken_for_an_empty_pair() {
        assert_eq!(bold("**a**ˇ**b**"), "**a****ˇ****b**");
    }

    #[test]
    fn italic_and_bold_nest_instead_of_undoing_each_other() {
        assert_eq!(italic("**«word»**"), "***«word»***");
        assert_eq!(bold("*«word»*"), "***«word»***");
        assert_eq!(italic("***«word»***"), "**«word»**");
        assert_eq!(bold("***«word»***"), "*«word»*");
    }

    #[test]
    fn italic_does_not_strip_bold() {
        assert_eq!(italic("«**word**»"), "*«**word**»*");
    }

    #[test]
    fn strikethrough_and_code_toggle() {
        let strike = |marked: &str| {
            run(marked, |text, selection| {
                toggle_emphasis(text, selection, Emphasis::Strikethrough)
            })
        };
        let code = |marked: &str| {
            run(marked, |text, selection| {
                toggle_emphasis(text, selection, Emphasis::Code)
            })
        };
        assert_eq!(strike("«gone»"), "~~«gone»~~");
        assert_eq!(strike("~~«gone»~~"), "«gone»");
        assert_eq!(code("run «ls»"), "run `«ls»`");
        assert_eq!(code("run ``«ls»``"), "run «ls»");
    }

    #[test]
    fn a_selection_across_paragraphs_is_wrapped_per_paragraph() {
        assert_eq!(bold("«one\n\ntwo»"), "«**one**\n\n**two**»");
    }

    #[test]
    fn a_selection_across_lines_of_one_paragraph_is_wrapped_once() {
        assert_eq!(bold("«one\ntwo»"), "**«one\ntwo»**");
    }

    #[test]
    fn cjk_text_is_not_treated_as_one_word() {
        assert_eq!(bold("中文ˇ文本"), "中文**ˇ**文本");
    }

    #[test]
    fn multibyte_words_keep_the_cursor_on_a_character_boundary() {
        assert_eq!(bold("naïˇve"), "**naïˇve**");
    }

    #[test]
    fn link_wraps_text_and_puts_the_cursor_where_the_url_goes() {
        assert_eq!(link("see «the paper» now"), "see [the paper](ˇ) now");
        assert_eq!(link("see paˇper now"), "see [paper](ˇ) now");
    }

    #[test]
    fn link_on_a_url_puts_the_cursor_where_the_label_goes() {
        assert_eq!(link("«https://example.com»"), "[ˇ](https://example.com)");
    }

    #[test]
    fn link_on_a_link_turns_it_back_into_text() {
        assert_eq!(link("«[paper](https://x.org)»"), "«paper»");
    }

    #[test]
    fn link_with_nothing_to_wrap_inserts_an_empty_link() {
        assert_eq!(link("a ˇ b"), "a [ˇ]() b");
    }
}
