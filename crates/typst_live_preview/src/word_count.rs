//! Tinymist-compatible word counting for rendered Typst text.
//!
//! This mirrors `tinymist::tool::word_count` so the count shown in the preview
//! matches the count Tinymist reports in `tinymist/compileStatus`. It counts
//! rendered text, not source markup: callers feed it the text items and content
//! hints from the preview IR.

use unicode_script::{Script, UnicodeScript};

/// Word, character, space, and CJK counts for a piece of rendered text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WordsCount {
    /// Number of words. Every CJK character counts as one word.
    pub words: usize,
    /// Number of characters, including whitespace.
    pub chars: usize,
    /// Number of whitespace runs; consecutive spaces count once.
    pub spaces: usize,
    /// Number of CJK characters.
    pub cjk_chars: usize,
}

impl WordsCount {
    /// A short label such as `12 words` or `1 word`.
    pub fn label(self) -> String {
        if self.words == 1 {
            "1 word".to_owned()
        } else {
            format!("{} words", self.words)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CountState {
    #[default]
    InSpace,
    InNonCjk,
    InCjk,
}

/// An incremental counter with the same state machine as Tinymist's counter.
#[derive(Debug, Clone, Default)]
pub struct WordCounter {
    count: WordsCount,
    state: CountState,
}

impl WordCounter {
    /// Creates an empty counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds rendered text into the counter.
    pub fn push_str(&mut self, text: &str) {
        for character in text.chars() {
            self.push(character);
        }
    }

    /// Feeds one rendered character into the counter.
    pub fn push(&mut self, character: char) {
        self.count.chars += 1;

        if character.is_whitespace() {
            if self.state != CountState::InSpace {
                self.count.spaces += 1;
            }
            self.state = CountState::InSpace;
            return;
        }

        if is_cjk(character) {
            self.count.words += 1;
            self.count.cjk_chars += 1;
            self.state = CountState::InCjk;
        } else {
            if self.state != CountState::InNonCjk {
                self.count.words += 1;
            }
            self.state = CountState::InNonCjk;
        }
    }

    /// Finishes counting and returns the totals.
    pub fn finish(self) -> WordsCount {
        self.count
    }
}

/// Counts one contiguous piece of rendered text.
pub fn count_words(text: &str) -> WordsCount {
    let mut counter = WordCounter::new();
    counter.push_str(text);
    counter.finish()
}

fn is_cjk(character: char) -> bool {
    matches!(
        character.script(),
        Script::Han | Script::Hiragana | Script::Katakana | Script::Hangul
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_whitespace_delimited_words() {
        assert_eq!(
            count_words("hello world"),
            WordsCount {
                words: 2,
                chars: 11,
                spaces: 1,
                cjk_chars: 0,
            }
        );
    }

    #[test]
    fn collapses_consecutive_whitespace_for_spaces() {
        assert_eq!(
            count_words("  hello \n\t world  "),
            WordsCount {
                words: 2,
                chars: 18,
                spaces: 2,
                cjk_chars: 0,
            }
        );
    }

    #[test]
    fn counts_each_cjk_character_as_a_word() {
        assert_eq!(
            count_words("你好 world"),
            WordsCount {
                words: 3,
                chars: 8,
                spaces: 1,
                cjk_chars: 2,
            }
        );
    }
}
