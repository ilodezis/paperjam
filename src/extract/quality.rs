//! Text quality score: tells a real text layer from decoding garbage.
//!
//! A PDF text layer with a broken font encoding looks like text to a parser
//! but is noise. The score is the product of four plausibility factors, so
//! any one of them failing drags it down:
//!
//! 1. the share of characters that belong in documents (letters of the
//!    expected scripts, digits, ordinary punctuation),
//! 2. the share of characters that never do (replacement characters,
//!    control codes, private-use code points, `ÿ`/`þ`-style mojibake),
//! 3. the share of absurdly long "words",
//! 4. the share of words of four or more letters that contain no vowel.

/// Scores at or above this are treated as exact text (no OCR needed). Tuned
/// on the synthetic corpus: clean text scores above 0.95, the broken
/// encoding fixture below 0.4.
pub const GOOD_THRESHOLD: f32 = 0.6;

/// A "word" longer than this is not a word.
const MAX_WORD_CHARS: usize = 40;

fn is_bad(c: char) -> bool {
    matches!(c, '\u{FFFD}' | 'ÿ' | 'þ' | 'Þ' | 'ð' | 'Ð')
        || (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        || ('\u{E000}'..='\u{F8FF}').contains(&c)
}

fn is_plain_letter(c: char) -> bool {
    c.is_alphabetic()
        && (c.is_ascii()
            || ('\u{00C0}'..='\u{024F}').contains(&c)
            || ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn is_ordinary_symbol(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '«' | '»'
                | '„'
                | '“'
                | '”'
                | '‘'
                | '’'
                | '–'
                | '—'
                | '№'
                | '€'
                | '°'
                | '§'
                | '•'
                | '…'
        )
}

fn is_vowel(c: char) -> bool {
    matches!(
        c.to_lowercase().next().unwrap_or(c),
        'a' | 'e'
            | 'i'
            | 'o'
            | 'u'
            | 'y'
            | 'ă'
            | 'â'
            | 'î'
            | 'а'
            | 'е'
            | 'ё'
            | 'и'
            | 'о'
            | 'у'
            | 'ы'
            | 'э'
            | 'ю'
            | 'я'
            | 'і'
    ) || ('\u{00C0}'..='\u{00FF}').contains(&c)
}

/// Quality of `text` in `[0, 1]`; empty text scores 0.
pub fn score(text: &str) -> f32 {
    let visible: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if visible.is_empty() {
        return 0.0;
    }
    let total = visible.len() as f32;
    // "(cid:123)" is what extractors print for glyphs they cannot map.
    let unmapped_glyphs = text.matches("(cid:").count() as f32;
    let bad = visible.iter().filter(|c| is_bad(**c)).count() as f32 + 5.0 * unmapped_glyphs;
    let ordinary = visible
        .iter()
        .filter(|c| {
            !is_bad(**c) && (is_plain_letter(**c) || c.is_numeric() || is_ordinary_symbol(**c))
        })
        .count() as f32;

    let words: Vec<&str> = text.split_whitespace().collect();
    let long = words
        .iter()
        .filter(|w| w.chars().count() > MAX_WORD_CHARS)
        .count() as f32;
    let word_like: Vec<&&str> = words
        .iter()
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 4)
        .collect();
    let vowelless = word_like
        .iter()
        .filter(|w| !w.chars().any(is_vowel))
        .count() as f32;
    let vowel_factor = if word_like.is_empty() {
        1.0
    } else {
        1.0 - 0.8 * vowelless / word_like.len() as f32
    };

    let score = (ordinary / total)
        * (1.0 - (3.0 * bad / total).min(1.0))
        * (1.0 - long / words.len() as f32)
        * vowel_factor;
    score.clamp(0.0, 1.0)
}

pub fn is_good(score: f32) -> bool {
    score >= GOOD_THRESHOLD
}

/// An embedded text layer with fewer words than this may be only a stamp on
/// a scan ("Scanned with …", a page number), so the page is read by OCR too.
pub const THIN_WORDS: usize = 20;

/// The score an embedded layer is given once OCR shows that it is only a part
/// of what the page says: still kept and searchable, no longer the page text.
pub const PARTIAL_SCORE: f32 = GOOD_THRESHOLD / 2.0;

/// Words of two or more letters or digits: what OCR noise mostly is not.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().filter(|c| c.is_alphanumeric()).count() >= 2)
        .count()
}

pub fn is_thin(text: &str) -> bool {
    word_count(text) < THIN_WORDS
}

/// A page is read by OCR unless its embedded text (with its score) is good
/// and says enough to be the page's text rather than a stamp on a scan.
pub fn needs_ocr(embedded: &Option<(String, f32)>) -> bool {
    embedded
        .as_ref()
        .is_none_or(|(text, score)| !is_good(*score) || is_thin(text))
}

/// Whether an OCR reading says clearly more than the embedded text of the
/// same page, i.e. the embedded text is a stamp on a scan, not its text.
pub fn ocr_says_more(embedded: &str, ocr: &str) -> bool {
    let (embedded, ocr) = (word_count(embedded), word_count(ocr));
    ocr >= 2 * embedded && ocr >= embedded + 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_text_scores_low() {
        assert!(score("\u{FFFD}\u{FFFD}ÿþ%%") < 0.3);
    }

    #[test]
    fn empty_and_blank_text_scores_zero() {
        assert_eq!(score(""), 0.0);
        assert_eq!(score(" \n\t "), 0.0);
    }

    #[test]
    fn clean_text_in_expected_scripts_scores_high() {
        for text in [
            "Договор аренды жилого помещения № 15 от 01.02.2026",
            "Contractul de închiriere nr. 15 din 1 februarie 2026, Chișinău",
            "Rental agreement between the parties, effective 1 February 2026.",
            "AB1234567 / 2026-02-01 / 1 500,00 EUR",
        ] {
            let s = score(text);
            assert!(is_good(s), "{text:?} scored {s}");
        }
    }

    #[test]
    fn broken_encoding_is_not_good() {
        for text in [
            "(cid:12)(cid:45)(cid:7) \u{1}\u{2}\u{3} \u{FFFD}\u{FFFD}",
            "Xkjhg Qwrtp Zxcvb Nmklj Bvcxz Ghjkl",
            "\u{E001}\u{E002}\u{E003}\u{E004} \u{E005}\u{E006}",
        ] {
            let s = score(text);
            assert!(!is_good(s), "{text:?} scored {s}");
        }
    }

    #[test]
    fn a_very_long_unbroken_run_is_penalised() {
        let blob = "a".repeat(200);
        assert!(score(&blob) < GOOD_THRESHOLD);
    }

    #[test]
    fn short_numeric_text_is_fine() {
        assert!(is_good(score("12.50")));
    }

    #[test]
    fn a_scanner_stamp_is_thin_and_a_page_of_text_is_not() {
        assert!(is_thin("Scanned with PocketScan\nScanned with PocketScan"));
        assert!(!is_thin(
            &"Rental agreement between the parties, effective 1 February 2026. ".repeat(3)
        ));
        assert!(!is_good(PARTIAL_SCORE));
    }

    #[test]
    fn ocr_says_more_only_when_it_reads_a_lot_more() {
        let stamp = "Scanned with PocketScan";
        let page = "РОССИЙСКАЯ ФЕДЕРАЦИЯ ГУ МВД РОССИИ Паспорт выдан Дата выдачи \
                    Код подразделения Фамилия Имя Отчество Пол Дата рождения \
                    Место рождения Личная подпись";
        assert!(ocr_says_more(stamp, page));
        // OCR noise around the same few words does not demote them.
        assert!(!ocr_says_more(stamp, "Scanned with PocketScan | . , ~ i l"));
        assert!(!ocr_says_more(page, page));
    }
}
