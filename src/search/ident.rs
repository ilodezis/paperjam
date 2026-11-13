//! Identifier spellings. A passport number is printed as `AP 1234567`, typed
//! as `AP1234567` and stored as either; a series and number can be split as
//! `45 21 123456`. Every spelling has to find every other one, so adjacent
//! identifier-like words are also indexed glued together (`text_ident`), and
//! a query is looked up glued as well as word by word.

/// Most words one spaced identifier is made of (`45 21 499 770`).
const MAX_PARTS: usize = 4;
/// Longest glued identifier worth indexing, in characters.
const MAX_LEN: usize = 32;
/// Letters-only parts are series prefixes (`AP`, `MD`, `АП`), not words.
const MAX_LETTER_PART: usize = 3;
/// What may sit between the words of one identifier.
const SEPARATORS: [char; 6] = [' ', '\t', '\u{a0}', '-', '.', '/'];
const MAX_SEPARATOR_CHARS: usize = 2;

/// Can this word be a piece of a spaced identifier: it has a digit, or it is
/// a short run of letters in front of one.
pub(super) fn is_part(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(char::is_alphanumeric)
        && (has_digit(word) || word.chars().count() <= MAX_LETTER_PART)
}

pub(super) fn has_digit(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
}

/// The alphanumeric characters of a value, nothing else: the form an
/// identifier is stored in for typed-field lookups.
pub(super) fn squash(value: &str) -> String {
    value.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Whitespace-separated list of every glued spelling of two to
/// [`MAX_PARTS`] adjacent identifier-like words of `text` that contain a
/// digit. Words on different lines are never joined.
pub(super) fn glued_spellings(text: &str) -> String {
    let words = words_with_gaps(text);
    let mut out = String::new();
    for (start, &(first, _)) in words.iter().enumerate() {
        if !is_part(first) {
            continue;
        }
        let mut glued = String::from(first);
        for &(word, gap_before) in words.iter().skip(start + 1).take(MAX_PARTS - 1) {
            if !gap_before || !is_part(word) {
                break;
            }
            glued.push_str(word);
            if glued.chars().count() > MAX_LEN {
                break;
            }
            if has_digit(&glued) {
                out.push_str(&glued);
                out.push(' ');
            }
        }
    }
    out
}

/// Alphanumeric words, each with whether it follows the previous word across
/// nothing but an identifier separator.
fn words_with_gaps(text: &str) -> Vec<(&str, bool)> {
    let mut words = Vec::new();
    let mut previous_end = None;
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if !c.is_alphanumeric() {
            chars.next();
            continue;
        }
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if !c.is_alphanumeric() {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        let joined = previous_end.is_some_and(|prev: usize| {
            let gap = &text[prev..start];
            !gap.is_empty()
                && gap.chars().count() <= MAX_SEPARATOR_CHARS
                && gap.chars().all(|c| SEPARATORS.contains(&c))
        });
        words.push((&text[start..end], joined));
        previous_end = Some(end);
    }
    words
}

/// Splits query items (each a list of analyzed words) into runs of
/// consecutive identifier-like items. Returns the index ranges of the runs
/// that span two or more items and contain a digit: those are the places
/// where the user may have split one identifier with spaces.
pub(super) fn spaced_runs(items: &[Option<Vec<String>>]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let is_run_item = |item: &Option<Vec<String>>| {
            item.as_ref()
                .is_some_and(|words| !words.is_empty() && words.iter().all(|w| is_part(w)))
        };
        if !is_run_item(&items[start]) {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < items.len() && is_run_item(&items[end]) {
            end += 1;
        }
        let digit = items[start..end]
            .iter()
            .flatten()
            .flatten()
            .any(|w| has_digit(w));
        if end - start >= 2 && digit {
            runs.push(start..end);
        }
        start = end;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glued(text: &str) -> Vec<String> {
        glued_spellings(text)
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    #[test]
    fn joins_a_series_prefix_and_number() {
        assert!(glued("Passport AP 1234567 issued").contains(&"AP1234567".to_string()));
        assert_eq!(glued("Passport AP 1234567 issued"), vec!["AP1234567"]);
    }

    #[test]
    fn joins_every_run_of_split_digits() {
        assert_eq!(
            glued("45 21 123456"),
            vec!["4521", "4521123456", "21123456"]
        );
    }

    #[test]
    fn hyphens_and_dots_join_but_newlines_do_not() {
        assert!(glued("112-233-445 95").contains(&"11223344595".to_string()));
        assert!(glued("AB.123").contains(&"AB123".to_string()));
        assert!(glued("AP\n1234567").is_empty());
        assert!(glued("AP, 1234567").is_empty());
    }

    #[test]
    fn plain_words_and_lone_numbers_add_nothing() {
        assert!(glued("the red apple 12 months").is_empty());
    }

    #[test]
    fn squash_keeps_only_letters_and_digits() {
        assert_eq!(squash("AP 123-45/6"), "AP123456");
        assert_eq!(squash("АП 12"), "АП12");
    }

    fn items(words: &[&[&str]]) -> Vec<Option<Vec<String>>> {
        words
            .iter()
            .map(|w| Some(w.iter().map(|s| (*s).to_string()).collect()))
            .collect()
    }

    #[test]
    fn query_runs_need_two_items_and_a_digit() {
        let q = items(&[&["series"], &["4521"], &["123456"], &["ap"]]);
        assert_eq!(spaced_runs(&q), vec![1..4]);
        assert!(spaced_runs(&items(&[&["red"], &["pie"]])).is_empty());
        assert!(spaced_runs(&items(&[&["4521"]])).is_empty());
        let mut with_negation = items(&[&["ap"], &["1234567"]]);
        with_negation.insert(1, None);
        assert!(spaced_runs(&with_negation).is_empty());
    }
}
