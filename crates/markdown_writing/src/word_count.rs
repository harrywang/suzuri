//! A word count in the status bar for markdown notes.
//!
//! Academic writing is writing to a limit, and journals do not count the
//! markdown a note is written in: not the frontmatter, the cite keys, the
//! URLs behind links, the code, or the reference list. So this counts what a
//! reader of the finished paper would read, the way a word processor would.

use std::time::Duration;

use editor::{Editor, EditorEvent};
use gpui::{App, Entity, Subscription, Task, WeakEntity};
use multi_buffer::MultiBufferOffset;
use settings::{RegisterSetting, Settings, SettingsStore};
use ui::{Tooltip, prelude::*};
use workspace::{HideStatusItem, StatusItemView, item::ItemHandle};

/// Typing edits the note many times a second; counting after a short pause
/// keeps a long note from being recounted on every keystroke.
const RECOUNT_DELAY: Duration = Duration::from_millis(250);
const SELECTION_DELAY: Duration = Duration::from_millis(100);

/// A common silent-reading rate for non-fiction, used only for the tooltip.
const READING_WORDS_PER_MINUTE: usize = 230;

#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct WordCountSettings {
    pub enabled: bool,
}

impl Settings for WordCountSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            enabled: content
                .markdown_live_preview
                .as_ref()
                .and_then(|preview| preview.word_count)
                .unwrap_or(true),
        }
    }
}

pub struct WordCount {
    document: Option<usize>,
    selection: Option<usize>,
    active_editor: Option<WeakEntity<Editor>>,
    _editor_subscription: Option<Subscription>,
    _settings_subscription: Subscription,
    document_task: Task<()>,
    selection_task: Task<()>,
}

impl WordCount {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            document: None,
            selection: None,
            active_editor: None,
            _editor_subscription: None,
            _settings_subscription: cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
            document_task: Task::ready(()),
            selection_task: Task::ready(()),
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.document = None;
        self.selection = None;
        self.active_editor = None;
        self._editor_subscription = None;
        self.document_task = Task::ready(());
        self.selection_task = Task::ready(());
        cx.notify();
    }

    fn recount_document(
        &mut self,
        editor: &Entity<Editor>,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let editor = editor.read(cx);
        // Checked on every recount rather than once: a note's language is
        // often detected only after its editor is already on screen.
        let buffer = editor.buffer().read(cx).as_singleton();
        let Some(buffer) = buffer.filter(|_| crate::is_markdown(editor, cx)) else {
            self.document = None;
            self.selection = None;
            cx.notify();
            return;
        };
        let snapshot = buffer.read(cx).snapshot();
        self.document_task = cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let words = cx
                .background_spawn(async move { count_words(&snapshot.text()) })
                .await;
            this.update(cx, |this, cx| {
                this.document = Some(words);
                cx.notify();
            })
            .ok();
        });
    }

    fn recount_selection(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let editor = editor.downgrade();
        self.selection_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SELECTION_DELAY).await;
            let Ok(selected) = editor.update(cx, |editor, cx| {
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let display = editor.display_snapshot(cx);
                let mut selected = String::new();
                for selection in editor.selections.all::<MultiBufferOffset>(&display) {
                    if selection.start != selection.end {
                        selected.extend(snapshot.text_for_range(selection.start..selection.end));
                        selected.push('\n');
                    }
                }
                selected
            }) else {
                return;
            };
            let words = if selected.is_empty() {
                None
            } else {
                Some(
                    cx.background_spawn(async move { count_words(&selected) })
                        .await,
                )
            };
            this.update(cx, |this, cx| {
                this.selection = words;
                cx.notify();
            })
            .ok();
        });
    }
}

impl Render for WordCount {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(words) = self
            .document
            .filter(|_| WordCountSettings::get_global(cx).enabled)
        else {
            return div();
        };
        let label = match self.selection {
            Some(selected) => format!("{} of {} words", group(selected), group(words)),
            None if words == 1 => "1 word".to_string(),
            None => format!("{} words", group(words)),
        };
        let minutes = words.div_ceil(READING_WORDS_PER_MINUTE).max(1);
        div().child(
            Button::new("word-count", label)
                .label_size(LabelSize::Small)
                .tooltip(Tooltip::text(format!(
                    "About {minutes} min to read. Frontmatter, code, math, cite keys and the reference list are not counted."
                ))),
        )
    }
}

impl StatusItemView for WordCount {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = active_pane_item.and_then(|item| item.downcast::<Editor>()) else {
            self.clear(cx);
            return;
        };
        self.active_editor = Some(editor.downgrade());
        self._editor_subscription = Some(cx.subscribe_in(
            &editor,
            window,
            |this, editor, event: &EditorEvent, _window, cx| match event {
                EditorEvent::BufferEdited | EditorEvent::Reparsed(_) => {
                    this.recount_document(editor, RECOUNT_DELAY, cx)
                }
                EditorEvent::SelectionsChanged { .. } => this.recount_selection(editor, cx),
                _ => {}
            },
        ));
        self.selection = None;
        self.recount_document(&editor, Duration::ZERO, cx);
        self.recount_selection(&editor, cx);
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        Some(HideStatusItem::new(|settings| {
            settings
                .markdown_live_preview
                .get_or_insert_default()
                .word_count = Some(false);
        }))
    }
}

/// `12345` as `12,345`.
fn group(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn is_references_heading(title: &str) -> bool {
    matches!(
        title.trim().to_ascii_lowercase().as_str(),
        "references" | "bibliography" | "works cited" | "reference list"
    )
}

/// An ATX heading's level and title.
fn heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    (rest.is_empty() || rest.starts_with(' ')).then(|| (level, rest.trim()))
}

fn fence(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    ['`', '~'].into_iter().find_map(|marker| {
        let run: String = trimmed.chars().take_while(|&c| c == marker).collect();
        (run.len() >= 3).then_some(run)
    })
}

/// The words a reader of the finished document would read.
pub(crate) fn count_words(text: &str) -> usize {
    let mut words = 0;
    let mut lines = text.lines().peekable();

    if lines
        .peek()
        .is_some_and(|line| matches!(line.trim_end(), "---" | "+++"))
    {
        let opener = lines.next().unwrap_or_default().trim_end().to_string();
        for line in lines.by_ref() {
            let line = line.trim_end();
            if line == opener || (opener == "---" && line == "...") {
                break;
            }
        }
    }

    let mut open_fence: Option<String> = None;
    let mut in_display_math = false;
    let mut in_comment = false;
    let mut skipped_section: Option<usize> = None;

    for line in lines {
        if let Some(marker) = &open_fence {
            let trimmed = line.trim();
            let closes = marker
                .chars()
                .next()
                .is_some_and(|fence_character| trimmed.chars().all(|c| c == fence_character));
            if closes && trimmed.len() >= marker.len() {
                open_fence = None;
            }
            continue;
        }
        if in_display_math {
            if line.trim_end().ends_with("$$") {
                in_display_math = false;
            }
            continue;
        }
        if let Some((level, title)) = heading(line) {
            if skipped_section.is_some_and(|skipped| level <= skipped) {
                skipped_section = None;
            }
            if is_references_heading(title) {
                skipped_section = Some(level);
                continue;
            }
        }
        if skipped_section.is_some() {
            continue;
        }
        if let Some(marker) = fence(line) {
            open_fence = Some(marker);
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("$$") {
            in_display_math = !(trimmed.len() > 2 && trimmed.ends_with("$$"));
            continue;
        }

        let mut visible = String::with_capacity(line.len());
        let mut rest = line;
        while !rest.is_empty() {
            if in_comment {
                match rest.find("-->") {
                    Some(end) => {
                        rest = &rest[end + 3..];
                        in_comment = false;
                    }
                    None => rest = "",
                }
                continue;
            }
            match rest.find("<!--") {
                Some(start) => {
                    visible.push_str(&rest[..start]);
                    visible.push(' ');
                    rest = &rest[start + 4..];
                    in_comment = true;
                }
                None => {
                    visible.push_str(rest);
                    rest = "";
                }
            }
        }

        words += count_line(&strip_inline(&visible));
    }
    words
}

/// Removes what a reader never sees in a line: embeds, images, link
/// destinations, cite keys, footnote markers, callout types, inline math and
/// HTML tags. Removed spans leave a space so their neighbors do not merge.
fn strip_inline(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        if rest.starts_with("![[")
            && let Some(end) = rest.find("]]")
        {
            output.push(' ');
            rest = &rest[end + 2..];
            continue;
        }
        if rest.starts_with("[[")
            && let Some(end) = rest[2..].find("]]")
        {
            let inner = &rest[2..2 + end];
            let shown = match inner.split_once('|') {
                Some((_, alias)) => alias,
                None => inner.split('#').next().unwrap_or(inner),
            };
            output.push_str(shown);
            rest = &rest[2 + end + 2..];
            continue;
        }
        if rest.starts_with("![")
            && let Some((_, after)) = link_parts(&rest[1..])
        {
            output.push(' ');
            rest = after;
            continue;
        }
        if character == '[' {
            if let Some((label, after)) = link_parts(rest) {
                output.push_str(label);
                rest = after;
                continue;
            }
            if let Some(close) = rest.find(']') {
                let inner = &rest[1..close];
                if inner.contains('@') || inner.starts_with('^') || inner.starts_with('!') {
                    output.push(' ');
                    rest = &rest[close + 1..];
                    continue;
                }
            }
        }
        if character == '$'
            && let Some(length) = inline_math_length(rest)
        {
            output.push(' ');
            rest = &rest[length..];
            continue;
        }
        if character == '<'
            && let Some(close) = rest.find('>')
            && rest[1..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_alphabetic() || next == '/')
        {
            output.push(' ');
            rest = &rest[close + 1..];
            continue;
        }
        output.push(character);
        rest = &rest[character.len_utf8()..];
    }
    output
}

/// For text starting at `[`: the label of an inline link and the text after
/// its closing parenthesis.
fn link_parts(text: &str) -> Option<(&str, &str)> {
    let close = text.find(']')?;
    let after = &text[close + 1..];
    let destination = after.strip_prefix('(')?;
    let end = destination.find(')')?;
    Some((&text[1..close], &destination[end + 1..]))
}

/// The length of a `$…$` span at the start of `text`, using Pandoc's rule so
/// prices are not taken for math: the opening `$` is not followed by a space,
/// the closing one is not preceded by a space or followed by a digit.
fn inline_math_length(text: &str) -> Option<usize> {
    let body = text.strip_prefix('$')?;
    if body.starts_with('$') || body.starts_with(char::is_whitespace) {
        return None;
    }
    let mut search = 0;
    while let Some(found) = body[search..].find('$') {
        let position = search + found;
        let before = body[..position].chars().next_back();
        let after = body[position + 1..].chars().next();
        if position > 0
            && !before.is_some_and(char::is_whitespace)
            && !after.is_some_and(|c| c.is_ascii_digit())
        {
            return Some(position + 2);
        }
        search = position + 1;
    }
    None
}

/// Whitespace-separated words, as a word processor counts them, except that
/// each CJK character is a word of its own, since CJK text has no spaces.
/// Tokens with no letter or digit (list bullets, table pipes, `**`) are not
/// words, and neither are bare `@key` citations or `#tags`.
fn count_line(line: &str) -> usize {
    let mut words = 0;
    for token in line.split_whitespace() {
        if token.starts_with('@') || token.starts_with("-@") {
            continue;
        }
        if token.starts_with('#') && token[1..].starts_with(|c: char| c.is_alphabetic()) {
            continue;
        }
        let mut in_word = false;
        for character in token.chars() {
            if crate::is_cjk(character) {
                words += 1;
                in_word = false;
            } else if character.is_alphanumeric() {
                if !in_word {
                    words += 1;
                    in_word = true;
                }
            } else if !matches!(character, '\'' | '’' | '-' | '.' | ',') {
                // Joining punctuation keeps `don't`, `e.g.` and `3,000` one
                // word; anything else, such as `/` or `*`, separates.
                in_word = false;
            }
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_counts_like_a_word_processor() {
        assert_eq!(count_words("The quick brown fox jumps."), 5);
        assert_eq!(count_words("Don't split e.g. or 3,000 or well-known."), 7);
    }

    #[test]
    fn markdown_syntax_is_not_words() {
        assert_eq!(
            count_words("# A Title\n\n- **bold** and *italic*\n> quoted"),
            6
        );
        assert_eq!(count_words("| a | b |\n| --- | --- |\n| c | d |"), 4);
    }

    #[test]
    fn frontmatter_is_not_counted() {
        assert_eq!(
            count_words("---\ntitle: A Long Title Here\ncsl: ieee\n---\n\nTwo words."),
            2
        );
    }

    #[test]
    fn code_and_math_are_not_counted() {
        assert_eq!(
            count_words("One\n\n```rust\nlet a = 1;\n```\n\n$$\nx^2\n$$\n\nTwo $E=mc^2$ three."),
            3
        );
    }

    #[test]
    fn prices_are_not_mistaken_for_math() {
        assert_eq!(count_words("It costs $5 and $10 now."), 6);
    }

    #[test]
    fn citations_are_not_counted() {
        assert_eq!(
            count_words("As shown [@vaswani2017; see @knuth1984, p. 3], and @doe2020 agrees."),
            4
        );
    }

    #[test]
    fn links_count_their_text_and_not_their_destination() {
        assert_eq!(
            count_words("Read [the paper](https://example.com/a/long/path) now."),
            4
        );
        assert_eq!(
            count_words("See [[Method Note|the method]] and [[Other#Part]]."),
            5
        );
    }

    #[test]
    fn images_embeds_tags_footnotes_and_comments_are_not_counted() {
        assert_eq!(
            count_words(
                "A ![alt text](x.png) b ![[Embedded]] c #tag d[^1] <!-- note to self --> e"
            ),
            5
        );
        assert_eq!(count_words("One\n<!--\nhidden\nlines\n-->\nTwo"), 2);
        assert_eq!(count_words("> [!warning] Mind the gap"), 3);
    }

    #[test]
    fn the_reference_list_is_not_counted() {
        // Paper, Body text, then counting resumes at the next section.
        let note = "# Paper\n\nBody text.\n\n## References\n\nKnuth, D. The TeXbook.\n\n## Appendix\n\nMore.";
        assert_eq!(count_words(note), 5);
    }

    #[test]
    fn each_cjk_character_is_a_word() {
        assert_eq!(count_words("中文文本"), 4);
        assert_eq!(count_words("用 Suzuri 写作"), 4);
    }

    #[test]
    fn html_tags_are_not_words() {
        assert_eq!(count_words("line<br>break <kbd>Cmd</kbd>"), 3);
    }

    #[test]
    fn large_counts_are_grouped() {
        assert_eq!(group(7), "7");
        assert_eq!(group(1234), "1,234");
        assert_eq!(group(1234567), "1,234,567");
    }

    /// The status item itself, wired to a real editor: counts the note once
    /// it becomes active, counts a selection against it, and recounts after
    /// an edit. The label cannot be read from outside the app, so this is
    /// the check that the pieces are connected.
    #[gpui::test]
    async fn counts_the_active_note_its_selection_and_edits(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let mut cx = editor::test::editor_test_context::EditorTestContext::new(cx).await;
        cx.update_buffer(|buffer, cx| buffer.set_language(Some(language::markdown_lang()), cx));
        cx.set_state("---\ntitle: Ignored Words\n---\n\nˇOne two three [@key].\n");

        let editor = cx.editor.clone();
        let word_count = cx.update(|window, cx| {
            let word_count = cx.new(WordCount::new);
            word_count.update(cx, |word_count, cx| {
                word_count.set_active_pane_item(Some(&editor as &dyn ItemHandle), window, cx)
            });
            word_count
        });
        let settle = |cx: &mut editor::test::editor_test_context::EditorTestContext| {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.executor().run_until_parked();
        };

        settle(&mut cx);
        assert_eq!(
            word_count.read_with(&cx.cx, |count, _| count.document),
            Some(3)
        );
        assert_eq!(
            word_count.read_with(&cx.cx, |count, _| count.selection),
            None
        );

        cx.set_state("---\ntitle: Ignored Words\n---\n\n«One twoˇ» three [@key].\n");
        settle(&mut cx);
        assert_eq!(
            word_count.read_with(&cx.cx, |count, _| count.selection),
            Some(2)
        );

        cx.set_state("---\ntitle: Ignored Words\n---\n\nˇOne two three four five.\n");
        settle(&mut cx);
        assert_eq!(
            word_count.read_with(&cx.cx, |count, _| count.document),
            Some(5)
        );
        assert_eq!(
            word_count.read_with(&cx.cx, |count, _| count.selection),
            None
        );
    }

    #[gpui::test]
    fn the_setting_defaults_to_on(cx: &mut gpui::App) {
        let store = SettingsStore::new(cx, &settings::default_settings());
        cx.set_global(store);
        assert!(WordCountSettings::get_global(cx).enabled);
    }
}
