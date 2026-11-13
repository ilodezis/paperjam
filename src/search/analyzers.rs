//! Text analyzers. Body text is indexed through all the configured ones, so a
//! query in any of those languages finds inflected forms of the same word.
//!
//! * One stemming analyzer per language (`en`, `ru`, `de`, ...): lower case,
//!   ASCII folding for Latin diacritics (other scripts pass through
//!   untouched), then the language's stemmer. The set follows the
//!   recognition languages (`PAPERJAM_OCR_LANGS`), see [`stem_tags`].
//! * `exact`: lower case and folding, no stemming; identifiers such as
//!   `AB1234567` stay whole tokens.
//! * `norm`: for names that were already folded with `matching::normalize`.

use tantivy::Index;
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer,
    TextAnalyzer,
};

pub const EN: &str = "en";
pub const EXACT: &str = "exact";
pub const NORM: &str = "norm";

/// Tokens longer than this are dropped (base64 blobs, OCR noise).
const MAX_TOKEN_BYTES: usize = 80;

/// Languages with a stemmer: Tesseract code, analyzer tag, stemmer.
const STEMMED: [(&str, &str, Language); 18] = [
    ("eng", "en", Language::English),
    ("rus", "ru", Language::Russian),
    ("ron", "ro", Language::Romanian),
    ("deu", "de", Language::German),
    ("fra", "fr", Language::French),
    ("spa", "es", Language::Spanish),
    ("ita", "it", Language::Italian),
    ("por", "pt", Language::Portuguese),
    ("nld", "nl", Language::Dutch),
    ("swe", "sv", Language::Swedish),
    ("dan", "da", Language::Danish),
    ("nor", "no", Language::Norwegian),
    ("fin", "fi", Language::Finnish),
    ("hun", "hu", Language::Hungarian),
    ("tur", "tr", Language::Turkish),
    ("ell", "el", Language::Greek),
    ("ara", "ar", Language::Arabic),
    ("tam", "ta", Language::Tamil),
];

/// Upper bound of [`stem_tags`] (the number of languages above).
pub const MAX_STEMMED: usize = STEMMED.len();

/// The tags of the stemming analyzers for a Tesseract language list such as
/// `eng+deu`: English always (summaries and queries are often English), then
/// every listed language that has a stemmer. Languages without one (Chinese,
/// Polish, ...) are still recognized and found by exact match.
pub fn stem_tags(ocr_langs: &str) -> Vec<&'static str> {
    let mut tags = vec![EN];
    for code in ocr_langs.split('+') {
        let code = code.trim();
        if let Some((_, tag, _)) = STEMMED.iter().find(|(c, _, _)| *c == code)
            && !tags.contains(tag)
        {
            tags.push(tag);
        }
    }
    tags
}

fn stemmed(language: Language) -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(MAX_TOKEN_BYTES))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .filter(Stemmer::new(language))
        .build()
}

fn unstemmed() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(MAX_TOKEN_BYTES))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

pub fn register(index: &Index, tags: &[&'static str]) {
    let manager = index.tokenizers();
    for (_, tag, language) in STEMMED {
        if tags.contains(&tag) {
            manager.register(tag, stemmed(language));
        }
    }
    manager.register(EXACT, unstemmed());
    manager.register(NORM, unstemmed());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_always_stemmed() {
        assert_eq!(stem_tags(""), ["en"]);
        assert_eq!(stem_tags("deu"), ["en", "de"]);
    }

    #[test]
    fn listed_languages_follow_in_order_without_duplicates() {
        assert_eq!(stem_tags("eng+rus+ron"), ["en", "ru", "ro"]);
        assert_eq!(stem_tags("fra+eng+fra"), ["en", "fr"]);
    }

    #[test]
    fn languages_without_a_stemmer_are_skipped() {
        assert_eq!(stem_tags("eng+pol+chi_sim"), ["en"]);
    }

    #[test]
    fn every_stemmed_language_has_a_distinct_tag() {
        let mut tags: Vec<_> = STEMMED.iter().map(|(_, t, _)| *t).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), MAX_STEMMED);
    }
}
