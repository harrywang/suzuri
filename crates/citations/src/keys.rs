//! Citation keys Suzuri mints for entries it adds to the vault library.
//!
//! Zotero stores a citation key but never generates one; Better BibTeX is
//! the only generator in the ecosystem, and its keys routinely break Typst
//! (a trailing period, a slash). Suzuri mints its own and pins them in
//! `refs.bib`, constrained to the intersection of what pandoc, Typst labels
//! and BibTeX all accept: ASCII letters and digits, with `_` and `-` allowed
//! inside but never at either end.

use crate::bibliography::first_year;

/// Whether `key` is legal, unchanged, as a pandoc cite key, a Typst label
/// and a BibTeX key.
pub fn is_portable_key(key: &str) -> bool {
    let first_ok = key
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    let last_ok = key
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    first_ok
        && last_ok
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

/// `surname + year + first significant title word`, folded to lowercase
/// ASCII: `vaswani2017attention`. Parts that are missing are simply left
/// out; an entry with nothing usable becomes `untitled`, which
/// [`disambiguate`] then suffixes.
pub fn mint_key(surname: Option<&str>, year: Option<&str>, title: Option<&str>) -> String {
    let mut key = String::new();
    if let Some(surname) = surname {
        key.push_str(&fold_ascii_lower(surname));
    }
    if let Some(year) = year.and_then(first_year) {
        key.push_str(&year);
    }
    if let Some(word) = title.and_then(title_word) {
        key.push_str(&word);
    }
    if key.is_empty() {
        key.push_str("untitled");
    }
    key
}

/// The first key derived from `base` that `taken` does not reject: `base`,
/// then `basea`, `baseb`, …, then `base2`, `base3`, … past `z`.
pub fn disambiguate(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    for suffix in 'a'..='z' {
        let candidate = format!("{base}{suffix}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    let mut number = 2;
    loop {
        let candidate = format!("{base}{number}");
        if !taken(&candidate) {
            return candidate;
        }
        number += 1;
    }
}

/// Lowercase ASCII letters and digits only. Common Latin diacritics fold
/// to their base letter so `Müller` becomes `muller` rather than `mller`;
/// anything else non-ASCII is dropped.
fn fold_ascii_lower(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            folded.push(character);
            continue;
        }
        let replacement = match character {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
            'æ' => "ae",
            'ç' | 'ć' | 'č' | 'ĉ' => "c",
            'ď' | 'đ' | 'ð' => "d",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
            'ğ' | 'ĝ' => "g",
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => "i",
            'ł' | 'ľ' | 'ĺ' => "l",
            'ñ' | 'ń' | 'ň' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
            'œ' => "oe",
            'ř' | 'ŕ' => "r",
            'ś' | 'š' | 'ş' | 'ș' => "s",
            'ß' => "ss",
            'ť' | 'ţ' | 'ț' | 'þ' => "t",
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
            'ý' | 'ÿ' => "y",
            'ź' | 'ž' | 'ż' => "z",
            _ => "",
        };
        folded.push_str(replacement);
    }
    folded
}

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "on", "in", "for", "and", "or", "to", "with", "from", "by", "at", "is",
    "are", "as", "its", "via", "into", "over", "under", "about", "toward", "towards", "how",
    "what", "why", "when", "do", "does", "can", "not", "all", "you", "need",
];

/// The first title word worth keying on: not a stopword, at least three
/// letters once folded, capped so a long compound does not bloat the key.
fn title_word(title: &str) -> Option<String> {
    let words = title
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(fold_ascii_lower)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let chosen = words
        .iter()
        .find(|word| word.len() >= 3 && !STOPWORDS.contains(&word.as_str()))
        .or_else(|| words.first())?;
    Some(chosen.chars().take(20).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_surname_year_title_word() {
        assert_eq!(
            mint_key(
                Some("Vaswani"),
                Some("2017-06-12"),
                Some("Attention Is All You Need")
            ),
            "vaswani2017attention"
        );
        assert_eq!(
            mint_key(Some("Knuth"), Some("1984"), Some("The TeXbook")),
            "knuth1984texbook"
        );
    }

    #[test]
    fn folds_diacritics_and_drops_the_rest() {
        assert_eq!(
            mint_key(
                Some("Müller-Lüdenscheidt"),
                Some("2020"),
                Some("Über Größe")
            ),
            "mullerludenscheidt2020uber"
        );
        assert_eq!(mint_key(Some("王"), Some("2021"), Some("论文")), "2021");
    }

    #[test]
    fn missing_parts_are_left_out() {
        assert_eq!(mint_key(None, None, Some("Only a Title")), "only");
        assert_eq!(mint_key(Some("Doe"), None, None), "doe");
        assert_eq!(mint_key(None, None, None), "untitled");
        assert_eq!(mint_key(None, None, Some("   ")), "untitled");
    }

    #[test]
    fn minted_keys_are_always_portable() {
        for key in [
            mint_key(Some("O'Brien"), Some("1999"), Some("A/B: Testing...")),
            mint_key(Some("van der Berg"), Some("2001"), Some("(Un)certainty")),
            mint_key(None, None, None),
        ] {
            assert!(is_portable_key(&key), "{key:?} is not portable");
        }
    }

    #[test]
    fn portability_matches_pandoc_typst_and_bibtex_at_once() {
        assert!(is_portable_key("vaswani2017attention"));
        assert!(is_portable_key("smith-jones2020a"));
        assert!(is_portable_key("a_b"));
        assert!(!is_portable_key(""));
        assert!(!is_portable_key("Segawa2019QuantumInf.Process."));
        assert!(!is_portable_key("DBLP:books/lib/Knuth86a"));
        assert!(!is_portable_key("-leading"));
        assert!(!is_portable_key("trailing_"));
        assert!(!is_portable_key("müller2020"));
    }

    #[test]
    fn disambiguates_with_letters_then_numbers() {
        let taken = |key: &str| key == "doe2020" || key == "doe2020a";
        assert_eq!(disambiguate("doe2020", taken), "doe2020b");
        assert_eq!(disambiguate("new2020", taken), "new2020");
        let all_letters =
            |key: &str| key == "doe2020" || key.ends_with(|c: char| c.is_ascii_alphabetic());
        assert_eq!(disambiguate("doe2020", all_letters), "doe20202");
    }
}
