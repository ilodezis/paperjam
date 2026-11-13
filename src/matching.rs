//! Name normalization and alias matching (spec §7.6).
//!
//! `normalize` folds a string to a comparison form: NFKD, combining marks
//! removed, lower case, and Cyrillic transliterated to Latin with the fixed
//! table below (ISO 9 / GOST 7.79 system B, simplified: `й` loses its breve
//! to the mark stripping and becomes `i`, hard and soft signs vanish). The
//! table only has to be consistent, since both sides of every comparison go
//! through it, so "Марик" and "Maric" end up one edit apart.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::ids::{CorrespondentId, PersonId};

/// Tokens of at least this many characters may differ by one edit.
const FUZZY_MIN_LEN: usize = 5;
/// All tokens of an alias must be found within this many consecutive tokens.
const WINDOW: usize = 6;

fn transliterate(c: char) -> Option<&'static str> {
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' | 'ґ' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'є' => "ye",
        'ж' => "zh",
        'з' => "z",
        'и' | 'і' | 'ї' => "i",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' | 'ў' => "u",
        'ф' => "f",
        'х' => "h",
        'ц' => "c",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shh",
        'ъ' | 'ь' => "",
        'ы' => "y",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

/// Comparison form of `s`: see the module documentation.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfkd().filter(|c| !is_combining_mark(*c)) {
        for lower in c.to_lowercase() {
            match transliterate(lower) {
                Some(latin) => out.push_str(latin),
                None => out.push(lower),
            }
        }
    }
    out
}

/// An organization's name as one comparison key: its normalized tokens joined
/// by single spaces, so quotes, spacing around them and other punctuation do
/// not tell `ООО " Ромашка"` from `ООО "Ромашка"`.
pub fn name_key(name: &str) -> String {
    tokens(name).join(" ")
}

/// Normalized alphanumeric tokens of `s`.
fn tokens(s: &str) -> Vec<String> {
    normalize(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn within_one_edit(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    // What remains in the middle must be a single substitution, insertion
    // or deletion.
    (a.len() - prefix - suffix).max(b.len() - prefix - suffix) <= 1
}

fn token_matches(alias: &str, text: &str) -> bool {
    alias == text
        || (alias.chars().count() >= FUZZY_MIN_LEN
            && text.chars().count() >= FUZZY_MIN_LEN
            && within_one_edit(alias, text))
}

/// How well `alias` appears in `text` (already tokenized): the number of
/// characters matched, or 0 when some alias token has no match within a
/// window of [`WINDOW`] tokens.
fn alias_score(alias: &str, text: &[String]) -> usize {
    let alias = tokens(alias);
    if alias.is_empty() {
        return 0;
    }
    let found = (0..text.len()).any(|start| {
        let window = &text[start..(start + WINDOW).min(text.len())];
        alias
            .iter()
            .all(|a| window.iter().any(|t| token_matches(a, t)))
    });
    if found {
        alias.iter().map(|t| t.chars().count()).sum()
    } else {
        0
    }
}

fn best_score(aliases: &[String], text: &[String]) -> usize {
    aliases
        .iter()
        .map(|alias| alias_score(alias, text))
        .max()
        .unwrap_or(0)
}

/// People whose names or aliases appear in `text`, in input order.
pub fn find_people(text: &str, people: &[(PersonId, Vec<String>)]) -> Vec<PersonId> {
    let text = tokens(text);
    people
        .iter()
        .filter(|(_, aliases)| best_score(aliases, &text) > 0)
        .map(|(id, _)| *id)
        .collect()
}

/// Whether `text` backs `name` at all: every token of the name that carries a
/// digit is printed in the text, and at least one of its other tokens is too
/// (within one edit, as in alias matching). Looser than [`find_correspondent`]
/// on purpose: it checks a pick made by a model that may translate or
/// abbreviate, and only has to catch a pick the text does not support, such
/// as an office number the document never prints.
pub fn name_supported(name: &str, text: &str) -> bool {
    pick_backed([name], None, text)
}

/// Whether the document backs a correspondent a model picked. `names` are the
/// correspondent's name and its aliases; `printed` is the name the model says
/// it read off the document (its `new_name`).
///
/// A pick is backed when one of the names passes [`name_supported`] against
/// the text, or, for a name whose digit tokens are all printed in the text,
/// when one of its words is in `printed`. The second path is for a
/// correspondent stored in one language and a document in another: an alias
/// covers it when the user added one, and otherwise the model, which saw both
/// forms, writes `printed` close to the stored name.
///
/// The digits are never relaxed: an office number the text does not print
/// ("МВД 12345" for a passport issued by another office) rejects the pick
/// whatever `printed` says.
///
/// Trade-off: this is a word-overlap check, not a translation check. A name
/// stored as "Ministry of Internal Affairs" against a document that prints
/// only «Министерство внутренних дел», with no alias and a `printed` that
/// shares no word with it, stays unbacked, and the printed name becomes a new
/// correspondent next to the stored one. That duplicate is accepted over
/// linking the wrong organization; merging the two unions their aliases, so
/// later documents match directly.
pub fn pick_backed<'a>(
    names: impl IntoIterator<Item = &'a str>,
    printed: Option<&str>,
    text: &str,
) -> bool {
    let text = tokens(text);
    let printed = printed.map(tokens).unwrap_or_default();
    names.into_iter().any(|name| {
        let (numbers, words): (Vec<String>, Vec<String>) = tokens(name)
            .into_iter()
            .partition(|t| t.chars().any(|c| c.is_ascii_digit()));
        numbers.iter().all(|n| text.contains(n))
            && words.iter().any(|w| {
                text.iter()
                    .chain(printed.iter())
                    .any(|t| token_matches(w, t))
            })
    })
}

/// The entry of `list` that matches `text` best (most matched characters).
fn best_match<Id: Copy>(text: &str, list: &[(Id, Vec<String>)]) -> Option<Id> {
    let text = tokens(text);
    list.iter()
        .map(|(id, aliases)| (best_score(aliases, &text), *id))
        .filter(|(score, _)| *score > 0)
        .max_by_key(|(score, _)| *score)
        .map(|(_, id)| id)
}

/// The correspondent that matches `text` best (most matched characters), if
/// any. `list` entries carry the name and all aliases.
pub fn find_correspondent(
    text: &str,
    list: &[(CorrespondentId, Vec<String>)],
) -> Option<CorrespondentId> {
    best_match(text, list)
}

/// The one person that matches `text` best: for a name read off an identity
/// document, which has a single holder. `list` entries carry the name and all
/// aliases.
pub fn find_person(text: &str, list: &[(PersonId, Vec<String>)]) -> Option<PersonId> {
    best_match(text, list)
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// The script of the letters of `s`: `Some(true)` when all are Cyrillic,
/// `Some(false)` when none is, `None` when mixed or there are no letters.
pub fn cyrillic(s: &str) -> Option<bool> {
    let mut letters = s.chars().filter(|c| c.is_alphabetic()).peekable();
    let first = is_cyrillic(*letters.peek()?);
    letters.all(|c| is_cyrillic(c) == first).then_some(first)
}

/// A name printed in capitals ("ТЕСТОВ", "SMITH-JONES") in the form people
/// write it ("Тестов", "Smith-Jones"). A word that already mixes cases
/// ("McDonald") is left alone.
pub fn readable_name(name: &str) -> String {
    fn word(part: &str) -> String {
        let has_upper = part.chars().any(char::is_uppercase);
        let has_lower = part.chars().any(char::is_lowercase);
        if has_upper && has_lower {
            return part.to_string();
        }
        let mut chars = part.chars();
        match chars.next() {
            Some(first) => first
                .to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect(),
            None => String::new(),
        }
    }
    let mut out = String::with_capacity(name.len());
    let mut part = String::new();
    for c in name.trim().chars() {
        if c.is_alphabetic() {
            part.push(c);
        } else {
            out.push_str(&word(&part));
            part.clear();
            out.push(c);
        }
    }
    out.push_str(&word(&part));
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How `word` is printed in `text` in the other script (Cyrillic for a Latin
/// word and the reverse), when the text prints it both ways, as the data page
/// of an international passport does ("ТЕСТОВ / TESTOV"). `None` when the
/// text only has one spelling.
pub fn other_script_spelling(word: &str, text: &str) -> Option<String> {
    let cyr = cyrillic(word)?;
    let key = normalize(word);
    text.split(|c: char| !c.is_alphabetic() && c != '-')
        .filter(|w| cyrillic(w) == Some(!cyr))
        .find(|w| token_matches(&key, &normalize(w)))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(aliases: &[&str]) -> (PersonId, Vec<String>) {
        (
            PersonId::new(),
            aliases.iter().map(|a| a.to_string()).collect(),
        )
    }

    fn corr(aliases: &[&str]) -> (CorrespondentId, Vec<String>) {
        (
            CorrespondentId::new(),
            aliases.iter().map(|a| a.to_string()).collect(),
        )
    }

    #[test]
    fn marik_matches_maric() {
        let marik = person(&["Марик Тестов"]);
        assert_eq!(
            find_people("MARIC TESTOV", std::slice::from_ref(&marik)),
            vec![marik.0]
        );
    }

    #[test]
    fn cyrillic_alias_matches_latin_text() {
        let ivan = person(&["Иван Тестов"]);
        assert_eq!(
            find_people(
                "Passport of Ivan Testov, born 1990",
                std::slice::from_ref(&ivan)
            ),
            vec![ivan.0]
        );
    }

    #[test]
    fn latin_alias_matches_cyrillic_text() {
        let ivan = person(&["Ivan Testov"]);
        assert_eq!(
            find_people("Паспорт ТЕСТОВ ИВАН", std::slice::from_ref(&ivan)),
            vec![ivan.0]
        );
    }

    #[test]
    fn diacritics_ignored() {
        let stefan = person(&["Ștefan Popescu"]);
        assert_eq!(
            find_people("STEFAN POPESCU", std::slice::from_ref(&stefan)),
            vec![stefan.0]
        );
        assert_eq!(normalize("Ștefan"), normalize("Stefan"));
    }

    #[test]
    fn short_tokens_need_exact_match() {
        let ion = person(&["Ion"]);
        assert!(find_people("Ian", std::slice::from_ref(&ion)).is_empty());
        assert_eq!(find_people("ION", std::slice::from_ref(&ion)), vec![ion.0]);
    }

    #[test]
    fn two_edits_do_not_match() {
        let marik = person(&["Marik"]);
        assert!(find_people("Mirok", std::slice::from_ref(&marik)).is_empty());
    }

    #[test]
    fn token_order_is_irrelevant_but_window_is_bounded() {
        let p = person(&["Marik Testov"]);
        assert_eq!(
            find_people("Testov, Marik", std::slice::from_ref(&p)),
            vec![p.0]
        );
        let far = "Marik one two three four five six seven Testov";
        assert!(find_people(far, std::slice::from_ref(&p)).is_empty());
    }

    #[test]
    fn every_alias_token_must_match() {
        let p = person(&["Marik Testov"]);
        assert!(find_people("Marik Petrov", std::slice::from_ref(&p)).is_empty());
    }

    #[test]
    fn finds_several_people_in_input_order() {
        let a = person(&["Anna Testova"]);
        let b = person(&["Boris Testov"]);
        let c = person(&["Clara Other"]);
        let all = [a.clone(), b.clone(), c];
        assert_eq!(
            find_people("Boris Testov and Anna Testova", &all),
            vec![a.0, b.0]
        );
    }

    #[test]
    fn organization_names_differing_in_quotes_and_spacing_share_a_key() {
        let key = name_key(r#"ООО "ТЕСТОВАЯ ЛАБОРАТОРИЯ""#);
        for variant in [
            r#"ООО " ТЕСТОВАЯ ЛАБОРАТОРИЯ""#,
            "ООО «Тестовая лаборатория»",
            "ooo  „Testovaya Laboratoriya”",
        ] {
            assert_eq!(name_key(variant), key, "{variant}");
        }
        assert_ne!(name_key("ООО Ромашка"), name_key("ООО Ромашка 2"));
    }

    #[test]
    fn maib_alias_matches_full_name() {
        let maib = corr(&["Moldova Agroindbank", "MAIB"]);
        let other = corr(&["Moldindconbank", "MICB"]);
        let list = [other, maib.clone()];
        assert_eq!(
            find_correspondent("BC MAIB S.A. statement", &list),
            Some(maib.0)
        );
        assert_eq!(
            find_correspondent("Moldova-Agroindbank S.A.", &list),
            Some(maib.0)
        );
        assert_eq!(find_correspondent("Some other bank", &list), None);
    }

    #[test]
    fn longer_correspondent_match_wins() {
        let short = corr(&["Orange"]);
        let long = corr(&["Orange Moldova"]);
        let list = [short, long.clone()];
        assert_eq!(
            find_correspondent("Orange Moldova invoice", &list),
            Some(long.0)
        );
    }

    #[test]
    fn a_name_with_an_office_number_the_text_lacks_is_not_supported() {
        let text = "РОССИЙСКАЯ ФЕДЕРАЦИЯ ГУ МВД РОССИИ ПО Г.МОСКВЕ Код подразделения 770-001";
        assert!(!name_supported("МВД 12345", text));
        assert!(name_supported("ГУ МВД России по г. Москве", text));
        assert!(name_supported("MVD", text), "across scripts");
    }

    #[test]
    fn an_alias_in_the_documents_language_backs_a_translated_correspondent() {
        let text = "МИНИСТЕРСТВО ВНУТРЕННИХ ДЕЛ РОССИЙСКОЙ ФЕДЕРАЦИИ";
        let stored = "Ministry of Internal Affairs";
        assert!(!pick_backed([stored], None, text), "no alias yet");
        assert!(pick_backed(
            [stored, "Министерство внутренних дел"],
            None,
            text
        ));
    }

    #[test]
    fn the_name_the_model_read_backs_a_correspondent_stored_in_another_language() {
        let text = "МИНИСТЕРСТВО ВНУТРЕННИХ ДЕЛ РОССИЙСКОЙ ФЕДЕРАЦИИ";
        let stored = ["Ministry of Internal Affairs"];
        assert!(pick_backed(
            stored,
            Some("Ministry of Internal Affairs of the Russian Federation"),
            text
        ));
        // A printed name that shares nothing with the stored one is no help:
        // the documented duplicate.
        assert!(!pick_backed(
            stored,
            Some("Министерство внутренних дел"),
            text
        ));
    }

    #[test]
    fn an_office_number_the_text_lacks_is_never_backed_by_the_printed_name() {
        let text = "РОССИЙСКАЯ ФЕДЕРАЦИЯ ГУ МВД РОССИИ ПО Г.МОСКВЕ Код подразделения 770-001";
        assert!(!pick_backed(
            ["МВД 12345"],
            Some("ГУ МВД России по г. Москве"),
            text
        ));
        assert!(
            !pick_backed(["МВД 12345"], Some("МВД 12345"), text),
            "the model's own claim does not print the number"
        );
        assert!(pick_backed(
            ["МВД 12345"],
            Some("ГУ МВД России по г. Москве"),
            "МВД 12345 ГУ МВД России по г. Москве"
        ));
    }

    #[test]
    fn a_name_sharing_no_word_with_the_text_is_not_supported() {
        assert!(!name_supported("Orange Moldova", "Moldcell invoice 2024"));
        assert!(name_supported("Moldcell S.A.", "Moldcell invoice 2024"));
    }

    #[test]
    fn capitals_become_readable_names() {
        assert_eq!(readable_name("ТЕСТОВ"), "Тестов");
        assert_eq!(readable_name("  IVAN  PETROVICH "), "Ivan Petrovich");
        assert_eq!(readable_name("SMITH-JONES"), "Smith-Jones");
        assert_eq!(readable_name("O'NEIL"), "O'Neil");
        assert_eq!(readable_name("McDonald"), "McDonald");
        assert_eq!(readable_name("пётр сергеевич"), "Пётр Сергеевич");
    }

    #[test]
    fn scripts_are_told_apart() {
        assert_eq!(cyrillic("Тестов"), Some(true));
        assert_eq!(cyrillic("Testov"), Some(false));
        assert_eq!(cyrillic("Test-ов"), None);
        assert_eq!(cyrillic("1990"), None);
    }

    #[test]
    fn the_other_spelling_is_found_when_the_text_prints_both() {
        let text = "Фамилия / Surname ТЕСТОВ / TESTOV Имя / Given names ИВАН / IVAN";
        assert_eq!(
            other_script_spelling("ТЕСТОВ", text).as_deref(),
            Some("TESTOV")
        );
        assert_eq!(other_script_spelling("IVAN", text).as_deref(), Some("ИВАН"));
        assert_eq!(other_script_spelling("ТЕСТОВ", "Паспорт ТЕСТОВ"), None);
    }
}
