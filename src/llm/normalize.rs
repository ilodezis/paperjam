//! Deterministic clean-up of what the model returns: the model is asked for a
//! shape, but its answers drift (`M` / `МУЖ.`, `Place of birth: X`, an issuer
//! with the field label inside its name, a title that starts with the
//! correspondent), so the enumerable and structural parts are normalized here
//! and never left to the prompt alone.

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::NaiveDate;

use crate::matching;
use crate::model::fields::Labels;

/// Type labels stripped from the front of one title, at most (a model that
/// writes the type twice: "Payment confirmation — Payment order — 2026").
const MAX_TYPE_PREFIXES: usize = 3;

/// Characters that separate the parts of a title.
const SEPARATORS: &[char] = &['-', '–', '—', ':', '|', '·', ',', '/', ';'];

/// A printed field label is at most this many words.
const MAX_LABEL_WORDS: usize = 8;

/// A label the vocabulary does not know is still taken for one when it is
/// this short and sits before a colon in a correspondent name.
const MAX_UNKNOWN_LABEL_WORDS: usize = 4;

const DOCUMENT_EXTENSIONS: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "webp", "heic", "heif", "gif", "tif", "tiff", "doc", "docx",
    "odt", "txt", "md", "rtf",
];

/// Words that make up the labels printed next to values on identity documents
/// and forms (English, Romanian, Russian transliterated by `matching`).
/// A prefix before a colon is a label when all its words are here.
const LABEL_WORDS: &[&str] = &[
    // place of birth
    "place",
    "of",
    "birth",
    "birthplace",
    "born",
    "date",
    "lieu",
    "de",
    "naissance",
    "loc",
    "locul",
    "nasterii",
    "nastere",
    "mesto",
    "mesta",
    "rozhdeniya",
    "rozhdeniye",
    "rozhdenie",
    // issuing authority
    "authority",
    "authorities",
    "issuing",
    "issued",
    "issuer",
    "by",
    "organ",
    "organul",
    "emitent",
    "emitenta",
    "autoritatea",
    "emis",
    "emisa",
    "eliberat",
    "eliberata",
    "vydan",
    "vydano",
    "vydavshiy",
    "vydavshii",
    "kem",
    "dokument",
    "document",
    "ausstellende",
    "behorde",
    "autorite",
    "delivre",
    "par",
    // other identity fields
    "sex",
    "gender",
    "pol",
    "gen",
    "nationality",
    "nationalitate",
    "citizenship",
    "cetatenie",
    "grazhdanstvo",
    "surname",
    "name",
    "given",
    "names",
    "prenume",
    "nume",
    "familiya",
    "imya",
    "address",
    "adresa",
    "adres",
];

const PLACEHOLDERS: &[&str] = &[
    "illegible",
    "unreadable",
    "unknown",
    "unclear",
    "undefined",
    "none",
    "null",
];

/// Placeholder stems in other languages, matched at the start of a word.
const PLACEHOLDER_STEMS: &[&str] = &["nerazborch", "nelizib", "neizvest", "necunosc"];

static LABEL_VOCABULARY: LazyLock<HashSet<String>> =
    LazyLock::new(|| LABEL_WORDS.iter().flat_map(|w| tokens(w)).collect());

/// The model's title shaped to the archive's format: no file extension and no
/// type label in front (the interface shows the type next to the title).
pub struct TitleContext<'a> {
    /// Names and aliases of the correspondents that must not stand in front of
    /// a type label.
    pub correspondents: &'a [String],
    /// Every label of every document type, in every language.
    pub type_labels: &'a [String],
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lower-case alphanumeric words, Cyrillic transliterated.
fn tokens(s: &str) -> Vec<String> {
    matching::normalize(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// `s` as one comparison string: case, script, spacing and punctuation gone.
fn squash(s: &str) -> String {
    matching::normalize(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn is_separator_or_space(c: char) -> bool {
    c.is_whitespace() || SEPARATORS.contains(&c)
}

// --- dates ------------------------------------------------------------------

/// A date the model printed in ISO or a common day-first form. Month-first is
/// never guessed.
pub fn date(raw: &str) -> Option<NaiveDate> {
    let t = raw.trim().trim_end_matches(['.', ' ']);
    // "15.04.2012 г."
    let t = t
        .strip_suffix('г')
        .unwrap_or(t)
        .trim_end_matches(['.', ' ']);
    let parts: Vec<&str> = t.split(['.', '/', '-']).collect();
    let [a, b, c] = parts[..] else {
        return None;
    };
    if ![a, b, c]
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    let number = |s: &str| s.parse::<u32>().ok();
    if a.len() == 4 && b.len() <= 2 && c.len() <= 2 {
        NaiveDate::from_ymd_opt(i32::try_from(number(a)?).ok()?, number(b)?, number(c)?)
    } else if c.len() == 4 && a.len() <= 2 && b.len() <= 2 {
        NaiveDate::from_ymd_opt(i32::try_from(number(c)?).ok()?, number(b)?, number(a)?)
    } else {
        None
    }
}

// --- enumerable values --------------------------------------------------------

/// `M`, `F` or `X` (the MRZ codes) for the ways documents print the sex.
pub fn sex(raw: &str) -> Option<&'static str> {
    if !raw.trim().is_empty() && raw.trim().chars().all(|c| c == '<') {
        return Some("X");
    }
    tokens(raw).iter().find_map(|token| {
        let t = token.as_str();
        if matches!(
            t,
            "m" | "male"
                | "man"
                | "masc"
                | "masculin"
                | "masculino"
                | "homme"
                | "mannlich"
                | "barbat"
                | "barbatesc"
        ) || t.starts_with("muzh")
        {
            Some("M")
        } else if matches!(
            t,
            "f" | "female" | "woman" | "fem" | "feminin" | "femeie" | "femme" | "weiblich" | "zh"
        ) || t.starts_with("zhen")
        {
            Some("F")
        } else if matches!(t, "x" | "other" | "unspecified" | "nonbinary") {
            Some("X")
        } else {
            None
        }
    })
}

// --- field values -------------------------------------------------------------

/// The text after a printed label (`Place of birth: X` gives `X`), when the
/// words before the first colon are all label words or one of the field's own
/// labels.
fn strip_label(value: &str, own_labels: &Labels) -> Option<String> {
    let own: HashSet<String> = own_labels.values().flat_map(|l| tokens(l)).collect();
    let is_label = |word: &String| LABEL_VOCABULARY.contains(word) || own.contains(word);
    let label_words = |prefix: &str| {
        let words = tokens(prefix);
        (!words.is_empty() && words.len() <= MAX_LABEL_WORDS && words.iter().all(is_label))
            .then_some(words.len())
    };
    if let Some(idx) = value.find(':')
        && label_words(&value[..idx]).is_some()
    {
        return Some(value[idx + 1..].trim().to_string());
    }
    // A label joined to its value by a comma, a dash or a slash ("Autoritatea
    // emitentă, București"). Two words at least, so that a name that merely
    // starts with a small word is not cut.
    let idx = value.find([',', '—', '–', '|', '/'])?;
    let separator = value[idx..].chars().next().map_or(1, char::len_utf8);
    (label_words(&value[..idx])? >= 2).then(|| value[idx + separator..].trim().to_string())
}

fn strip_labels(value: &str, own_labels: &Labels) -> String {
    let mut value = collapse(value);
    let mut stripped = false;
    // A model that copies the label twice.
    for _ in 0..3 {
        match strip_label(&value, own_labels) {
            Some(rest) => {
                value = rest;
                stripped = true;
            }
            None => break,
        }
    }
    if stripped {
        // The same label printed in a second language is left in front of the
        // value ("Autoritatea emitentă / Authority București").
        let words: Vec<&str> = value.split(' ').collect();
        let keep = words
            .iter()
            .position(|w| !tokens(w).iter().all(|t| LABEL_VOCABULARY.contains(t)))
            .unwrap_or(words.len());
        if keep > 0 && keep < words.len() {
            value = words[keep..].join(" ");
        }
    }
    value
}

/// A place as a name: capitals-only text (`SPRINGFIELD`) is written as a name
/// (`Springfield`); text with lower-case letters is left as printed.
fn place(value: &str) -> String {
    let value = value.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':'));
    let letters = || value.chars().filter(|c| c.is_alphabetic());
    let shouting = letters().count() > 3 && letters().all(|c| !c.is_lowercase());
    if !shouting {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut word_start = true;
    for c in value.chars() {
        if c.is_alphabetic() {
            if word_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            word_start = false;
        } else {
            out.push(c);
            word_start = !c.is_alphanumeric() && c != '\'';
        }
    }
    out
}

/// A text field value cleaned of its printed label and, for the fields the
/// archive knows (`sex`, `birth_place`), brought to one form. Empty when
/// nothing but a label was there.
pub fn field_text(key: &str, labels: &Labels, raw: &str) -> String {
    let value = strip_labels(raw, labels);
    match key {
        "sex" => sex(&value).map_or(value, str::to_string),
        "birth_place" => place(&value),
        _ => value,
    }
}

// --- correspondents -------------------------------------------------------------

/// Typographic double quotes that are written as the plain `"`.
const PLAIN_QUOTE_VARIANTS: &[char] = &['“', '”', '„', '‟', '″', '＂'];

/// An organization name with its whitespace collapsed, no space just inside
/// quotes or guillemets, and the double quotes in one form: `ООО " Ромашка"`
/// and `ООО „Ромашка”` both become `ООО "Ромашка"`. Guillemets are kept as
/// printed. The letter case is never touched: official names are often
/// capitals.
///
/// Straight quotes are paired in reading order; a name with an odd number of
/// them (`5" screens Ltd`) keeps its spacing around them.
fn tidy_quotes(name: &str) -> String {
    let name = collapse(name);
    let chars: Vec<char> = name
        .chars()
        .map(|c| {
            if PLAIN_QUOTE_VARIANTS.contains(&c) {
                '"'
            } else {
                c
            }
        })
        .collect();
    let paired = chars.iter().filter(|c| **c == '"').count() % 2 == 0;
    let mut out = String::with_capacity(name.len());
    let mut inside = false;
    let mut skip_space = false;
    for &c in &chars {
        if skip_space && c == ' ' {
            continue;
        }
        skip_space = false;
        let opening = match c {
            '«' => true,
            '»' => false,
            '"' if paired => {
                inside = !inside;
                inside
            }
            _ => {
                out.push(c);
                continue;
            }
        };
        if opening {
            // A company form glued to its opening quote ("ООО«Ромашка»").
            if out.chars().last().is_some_and(char::is_alphanumeric) {
                out.push(' ');
            }
            skip_space = true;
        } else {
            while out.ends_with(' ') {
                out.pop();
            }
        }
        out.push(c);
    }
    out
}

/// A usable correspondent name, or `None` for placeholders, bare field labels
/// and text the model marked as unreadable. The name comes out tidy (see
/// [`tidy_quotes`]), so the same organization is one name however it was
/// spaced.
pub fn correspondent_name(raw: &str) -> Option<String> {
    let mut name = strip_labels(raw, &Labels::new());
    // A label the vocabulary does not know: short, before a colon.
    if let Some(idx) = name.find(':') {
        let words = tokens(&name[..idx]).len();
        if (1..=MAX_UNKNOWN_LABEL_WORDS).contains(&words) {
            name = name[idx + 1..].trim().to_string();
        }
    }
    let name = tidy_quotes(&name)
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(c, '.' | ',' | ';' | ':' | '-' | '–' | '—' | '|' | '·' | '/')
        })
        .to_string();
    let words = tokens(&name);
    if name.chars().filter(|c| c.is_alphabetic()).count() < 2
        || words.iter().all(|w| LABEL_VOCABULARY.contains(w))
    {
        return None;
    }
    let placeholder = words.iter().any(|w| {
        matches!(w.as_str(), "illegible" | "unreadable")
            || PLACEHOLDER_STEMS.iter().any(|s| w.starts_with(s))
    }) || (words.len() <= 2
        && words
            .iter()
            .all(|w| PLACEHOLDERS.contains(&w.as_str()) || w == "n" || w == "a"));
    (!placeholder).then_some(name)
}

// --- titles -----------------------------------------------------------------------

/// Byte ranges of the whitespace-separated words of `s`.
fn word_spans(s: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(from)) => {
                spans.push((from, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        spans.push((from, s.len()));
    }
    spans
}

/// Where the leading words of `text` that read as `phrase` end (case, script,
/// spacing and punctuation do not matter).
fn leading_phrase(text: &str, phrase: &str) -> Option<usize> {
    let wanted = squash(phrase);
    let n = phrase.split_whitespace().count();
    if n == 0 || wanted.is_empty() {
        return None;
    }
    let end = word_spans(text).get(n - 1)?.1;
    (squash(&text[..end]) == wanted).then_some(end)
}

/// What follows a phrase that ended at `end`: whether a separator came right
/// after it, and the rest without the separator.
fn after_phrase(text: &str, end: usize) -> (bool, &str) {
    let head = text[..end].trim_end();
    let rest = &text[end..];
    let separated = head.ends_with(SEPARATORS) || rest.trim_start().starts_with(SEPARATORS);
    (separated, rest.trim_start_matches(is_separator_or_space))
}

fn strip_extension(title: &str) -> &str {
    match title.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && DOCUMENT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) =>
        {
            stem.trim_end()
        }
        _ => title,
    }
}

/// Where the leading words of `text` end when they are one of `labels`;
/// the longest label wins ("Travel passport" before "Passport").
fn leading_label(text: &str, labels: &[&String]) -> Option<usize> {
    labels.iter().find_map(|l| leading_phrase(text, l))
}

/// The labels, longest first, so that a longer label is tried before a
/// shorter one it starts with.
fn by_length(labels: &[String]) -> Vec<&String> {
    let mut sorted: Vec<&String> = labels.iter().collect();
    sorted.sort_by_key(|l| std::cmp::Reverse(l.chars().count()));
    sorted
}

/// A correspondent standing in front of a type label ("Acme Registry —
/// Passport — ...") goes with the label. A correspondent that is the title's
/// own subject (a receipt's shop) is left: only the type label after it marks
/// the prefix.
fn strip_correspondent_before_type(title: String, ctx: &TitleContext<'_>) -> String {
    let labels = by_length(ctx.type_labels);
    let mut names: Vec<&String> = ctx
        .correspondents
        .iter()
        .filter(|n| squash(n).chars().count() >= 3)
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(squash(n).len()));
    for name in names {
        let Some(end) = leading_phrase(&title, name) else {
            continue;
        };
        let (_, rest) = after_phrase(&title, end);
        if leading_label(rest, &labels).is_some() {
            return rest.to_string();
        }
    }
    title
}

/// Type labels in front of the title, in any language, joined to the rest by
/// a separator. The interface shows the type as a chip next to the title, so
/// the label would only repeat it. Prose that merely starts with a label
/// ("Passport of Ivan") and a title that is only a label are kept.
fn strip_type_labels(mut title: String, ctx: &TitleContext<'_>) -> String {
    let labels = by_length(ctx.type_labels);
    for _ in 0..MAX_TYPE_PREFIXES {
        let Some(end) = leading_label(&title, &labels) else {
            break;
        };
        let (separated, rest) = after_phrase(&title, end);
        if !separated || rest.is_empty() {
            break;
        }
        title = rest.to_string();
    }
    title
}

pub fn title(raw: &str, ctx: &TitleContext<'_>) -> String {
    let collapsed = collapse(raw);
    let title = strip_extension(&collapsed).trim().to_string();
    let title = strip_correspondent_before_type(title, ctx);
    let title = strip_type_labels(title, ctx);
    title.trim_matches(is_separator_or_space).to_string()
}

// --- prompts -----------------------------------------------------------------------

/// The English name of a UI locale, for the instructions to the model; an
/// unknown locale is passed through as its code.
pub fn language_name(locale: &str) -> String {
    match locale.split(['-', '_']).next().unwrap_or(locale) {
        "en" => "English".to_string(),
        "ru" => "Russian".to_string(),
        _ => locale.to_string(),
    }
}

// --- summaries ---------------------------------------------------------------------

/// Numbers this long in a summary must be printed in the document: a year, a
/// document number, an amount in thousands. Shorter ones (a day, a count) are
/// too common to check.
const GROUNDED_DIGITS: usize = 4;

/// The summary without the sentences that state a number the document does
/// not print. `source` is the document text plus the stored field values, so
/// a year the document prints as "07" but a field holds as "1990-01-15" counts
/// as printed. A model that writes "born in 1994" about a passport that says
/// 2007 loses that sentence instead of contradicting the fields.
///
/// Amounts are compared by their whole part with the thousands and decimal
/// separators ignored, in either direction: "1,250.00", "1.250,00" and
/// "1 250" in the document back "1250" in the summary, and "1,250.00" in the
/// summary is backed by a printed "1250". The cents are not compared.
pub fn grounded_summary(summary: &str, source: &str) -> String {
    let squeezed: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let wholes: HashSet<String> = numbers(source)
        .iter()
        .filter_map(|(groups, seps)| whole_part(groups, seps))
        .collect();
    let printed_digits = |digits: &str| squeezed.contains(digits) || wholes.contains(digits);
    let printed = |sentence: &str| {
        numbers(sentence)
            .iter()
            .all(|(groups, seps)| match whole_part(groups, seps) {
                Some(whole) => whole.len() < GROUNDED_DIGITS || printed_digits(&whole),
                None => groups
                    .iter()
                    .all(|g| g.len() < GROUNDED_DIGITS || printed_digits(g)),
            })
    };
    sentences(summary)
        .into_iter()
        .filter(|s| printed(s))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// What may sit between the digit groups of one written number: thousands
/// and decimal marks, including the apostrophe and the spaces of other locales.
fn is_number_separator(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | '\'' | '\u{2019}' | ' ' | '\u{a0}' | '\u{2009}' | '\u{202f}'
    )
}

/// The numbers in `text` as their digit groups and the separators between
/// them: "1,250.00" is `(["1", "250", "00"], [',', '.'])`, a plain "2021" has
/// one group and no separator. A date "01.02.2021" comes out as three groups.
fn numbers(text: &str) -> Vec<(Vec<String>, Vec<char>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let (mut groups, mut seps) = (Vec::new(), Vec::new());
        loop {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            groups.push(chars[start..i].iter().collect::<String>());
            let joined = i + 1 < chars.len()
                && is_number_separator(chars[i])
                && chars[i + 1].is_ascii_digit();
            if !joined {
                break;
            }
            seps.push(chars[i]);
            i += 1;
        }
        out.push((groups, seps));
    }
    out
}

/// The whole part of a number written with separators, digits only: "1250"
/// for "1,250.00", "1.250,00", "1 250" and "1'250". `None` for a plain run of
/// digits and for groups that are not an amount (a date, a phone number).
fn whole_part(groups: &[String], seps: &[char]) -> Option<String> {
    let n = groups.len();
    if n < 2 {
        return None;
    }
    // The last group is the decimals unless it has three digits and every
    // separator before it is the same mark ("1.250.000").
    let decimals = groups[n - 1].len() != 3 || seps[..n - 2].iter().any(|s| *s != seps[n - 2]);
    let whole = if decimals { &groups[..n - 1] } else { groups };
    let grouped = whole.len() == 1
        || ((1..=3).contains(&whole[0].len())
            && whole[1..].iter().all(|g| g.len() == 3)
            && seps[..whole.len() - 1].windows(2).all(|w| w[0] == w[1]));
    grouped.then(|| whole.concat())
}

/// `text` cut after each `.`, `!` or `?` that ends a sentence: followed by
/// whitespace and not closing a short lower-case abbreviation ("г.", "ул.",
/// "nr.").
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, &(at, c)) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let next_is_space = chars.get(i + 1).is_none_or(|(_, n)| n.is_whitespace());
        if !next_is_space {
            continue;
        }
        let word: String = chars[..i]
            .iter()
            .rev()
            .map(|(_, c)| *c)
            .take_while(|c| c.is_alphabetic())
            .collect();
        let abbreviation = c == '.'
            && !word.is_empty()
            && word.chars().count() < 4
            && word.chars().all(char::is_lowercase);
        if abbreviation {
            continue;
        }
        let end = at + c.len_utf8();
        let sentence = text[start..end].trim();
        if !sentence.is_empty() {
            out.push(sentence);
        }
        start = end;
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

// --- facts -------------------------------------------------------------------------

/// A fact key as a stable snake_case name of what the value is: ASCII lower
/// case, and no part that carries a digit, since those are values that leaked
/// into the key (`registration_removed_2024`). `fact` when nothing is left.
pub fn fact_key(raw: &str) -> String {
    let lowered: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let key = lowered
        .split('_')
        .filter(|part| !part.is_empty() && !part.chars().any(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join("_");
    if key.is_empty() {
        "fact".to_string()
    } else {
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_sentence_with_a_year_the_document_does_not_print_is_dropped() {
        let text = "ПАСПОРТ 45 09 123456 Дата рождения 15.01.1990 Дата выдачи 01.02.2010";
        let summary = "Паспорт на имя Иванова Ивана, 1994 года рождения. Выдан ГУ МВД по г. Москве \
                       01.02.2010. Номер 45 09 123456.";
        assert_eq!(
            grounded_summary(summary, text),
            "Выдан ГУ МВД по г. Москве 01.02.2010. Номер 45 09 123456."
        );
    }

    #[test]
    fn a_year_held_by_a_field_counts_as_printed() {
        // The page prints a two-digit year; the field holds the full date.
        let source = "Date of birth 15 JAN 90\n1990-01-15";
        let summary = "Born on 15 January 1990.";
        assert_eq!(grounded_summary(summary, source), summary);
    }

    #[test]
    fn a_summary_without_long_numbers_is_kept_whole() {
        let summary = "A lease for 12 months. Rent is due on the 5th!";
        assert_eq!(grounded_summary(summary, "nothing"), summary);
    }

    #[test]
    fn an_amount_printed_with_separators_backs_the_bare_number() {
        let summary = "Invoice for 1250 EUR.";
        for source in [
            "Total: 1,250.00 EUR",
            "Итого: 1.250,00 EUR",
            "Итого: 1 250,00 EUR",
            "Total 1\u{a0}250.00 EUR",
            "Total 1'250 EUR",
            "Total 1250.00 EUR",
        ] {
            assert_eq!(grounded_summary(summary, source), summary, "{source}");
        }
    }

    #[test]
    fn an_amount_in_the_summary_is_backed_whatever_its_separators() {
        let source = "Total 1250.00 EUR, paid 1 250 000 MDL";
        for summary in [
            "Invoice for 1,250.00 EUR.",
            "Invoice for 1.250,00 EUR.",
            "A payment of 1'250'000 MDL.",
            "A payment of 1 250 000 MDL.",
        ] {
            assert_eq!(grounded_summary(summary, source), summary, "{summary}");
        }
    }

    #[test]
    fn an_amount_the_document_does_not_print_is_dropped_however_it_is_written() {
        let source = "Total: 1,250.00 EUR";
        for summary in [
            "Invoice for 1350 EUR.",
            "Invoice for 1,350.00 EUR.",
            "Invoice for 12,500.00 EUR.",
            // 12.50 is not 1250.
            "Invoice for 1250 EUR.",
        ] {
            let source = if summary.contains("1250") {
                "Total: 12.50 EUR"
            } else {
                source
            };
            assert_eq!(grounded_summary(summary, source), "", "{summary}");
        }
    }

    #[test]
    fn dates_and_phone_numbers_are_not_read_as_amounts() {
        // "1.2.2010" is a date: only its year is checked, against "01.02.2010".
        let summary = "Issued 1.2.2010.";
        assert_eq!(grounded_summary(summary, "Issued 01.02.2010"), summary);
        assert_eq!(grounded_summary(summary, "Issued 01.02.2011"), "");
        // Phone numbers keep their spaces out of it.
        let phone = "Call +373 22 123 456.";
        assert_eq!(grounded_summary(phone, "tel. +373 22 123 456"), phone);
    }

    #[test]
    fn abbreviations_do_not_end_a_sentence() {
        assert_eq!(
            sentences("Выдан по г. Москве 2021 г. в срок. Второе."),
            vec!["Выдан по г. Москве 2021 г. в срок.", "Второе."]
        );
    }

    #[test]
    fn fact_keys_lose_values_and_odd_characters() {
        assert_eq!(fact_key("tax_id_inn"), "tax_id_inn");
        assert_eq!(
            fact_key("registration_removed_2024"),
            "registration_removed"
        );
        assert_eq!(fact_key("Registration Place"), "registration_place");
        assert_eq!(fact_key("Регистрация"), "fact");
        assert_eq!(fact_key("2024"), "fact");
    }

    fn ymd(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- dates ------------------------------------------------------------

    #[test]
    fn dates_come_out_iso_from_the_common_printed_forms() {
        for raw in [
            "2012-04-15",
            " 2012-04-15 ",
            "15.04.2012",
            "15/04/2012",
            "15-04-2012",
            "2012.04.15",
            "2012/04/15",
            "5.4.2012",
            "15.04.2012 г.",
            "15.04.2012.",
        ] {
            let want = if raw.contains('5') && raw.starts_with("5.") {
                ymd(2012, 4, 5)
            } else {
                ymd(2012, 4, 15)
            };
            assert_eq!(date(raw), want, "{raw}");
        }
    }

    #[test]
    fn impossible_or_ambiguous_dates_are_refused() {
        // Month-first is never guessed; the day-first reading is invalid.
        for raw in [
            "04/15/2012",
            "31.02.2012",
            "2012-13-45",
            "soon",
            "",
            "12.2012",
        ] {
            assert_eq!(date(raw), None, "{raw}");
        }
    }

    // --- sex --------------------------------------------------------------

    #[test]
    fn sex_is_one_code_whatever_the_language() {
        for raw in [
            "M",
            "m",
            "male",
            "Male",
            "МУЖ.",
            "муж",
            "Мужской",
            "М",
            "masculin",
            "M/М",
            "М / M",
            "Man",
        ] {
            assert_eq!(sex(raw), Some("M"), "{raw}");
        }
        for raw in [
            "F",
            "f",
            "female",
            "ЖЕН.",
            "жен",
            "Женский",
            "Ж",
            "feminin",
            "F/Ж",
            "Ж / F",
            "Woman",
        ] {
            assert_eq!(sex(raw), Some("F"), "{raw}");
        }
        for raw in ["X", "x", "unspecified", "<"] {
            assert_eq!(sex(raw), Some("X"), "{raw}");
        }
        for raw in ["", "yes", "42"] {
            assert_eq!(sex(raw), None, "{raw}");
        }
    }

    // --- field text -------------------------------------------------------

    #[test]
    fn a_field_keeps_the_value_and_loses_its_label() {
        let birth = labels(&[("en", "Place of Birth"), ("ru", "Место рождения")]);
        for (raw, want) in [
            ("Place of birth: Springfield", "Springfield"),
            ("Место рождения: Springfield", "Springfield"),
            (
                "Locul nașterii / Lieu de naissance: Springfield",
                "Springfield",
            ),
            ("Place of birth: Place of birth: Springfield", "Springfield"),
            ("  Springfield  ", "Springfield"),
            ("Spring  field", "Spring field"),
            ("Springfield,", "Springfield"),
            ("Springfield, Utopia", "Springfield, Utopia"),
        ] {
            assert_eq!(field_text("birth_place", &birth, raw), want, "{raw}");
        }
    }

    #[test]
    fn a_colon_that_is_not_a_label_stays() {
        let address = labels(&[("en", "Address")]);
        assert_eq!(
            field_text("address", &address, "Apt: 5, Main Street 1"),
            "Apt: 5, Main Street 1"
        );
        // The field's own label counts as a label.
        assert_eq!(
            field_text("address", &address, "Address: Main Street 1"),
            "Main Street 1"
        );
    }

    #[test]
    fn a_value_that_is_only_a_label_is_empty() {
        let birth = labels(&[("en", "Place of Birth")]);
        assert_eq!(field_text("birth_place", &birth, "Place of birth:"), "");
    }

    #[test]
    fn places_printed_in_capitals_are_written_as_names() {
        let birth = Labels::new();
        for (raw, want) in [
            ("SPRINGFIELD", "Springfield"),
            ("SANKT-PETERBURG", "Sankt-Peterburg"),
            ("СПРИНГФИЛД", "Спрингфилд"),
            ("ST. LOUIS", "St. Louis"),
            ("Springfield", "Springfield"),
            ("Chișinău", "Chișinău"),
            ("MD", "MD"),
        ] {
            assert_eq!(field_text("birth_place", &birth, raw), want, "{raw}");
        }
    }

    #[test]
    fn sex_values_are_coded_and_other_fields_are_left_alone() {
        let none = Labels::new();
        assert_eq!(field_text("sex", &none, "МУЖ."), "M");
        assert_eq!(field_text("sex", &none, "Sex: Female"), "F");
        assert_eq!(field_text("sex", &none, "n/a"), "n/a");
        assert_eq!(field_text("surname", &none, "TESTOV"), "TESTOV");
    }

    // --- correspondents ---------------------------------------------------

    #[test]
    fn a_correspondent_name_loses_the_field_label() {
        for (raw, want) in [
            ("Acme Bank", Some("Acme Bank")),
            ("  Acme   Bank. ", Some("Acme Bank")),
            ("Issuing authority: Acme Registry", Some("Acme Registry")),
            ("Autoritatea emitentă: Acme Registry", Some("Acme Registry")),
            ("Кем выдан: Acme Registry", Some("Acme Registry")),
            // A label the vocabulary has not seen, short and before a colon.
            ("Ausgestellt von: Acme Registry", Some("Acme Registry")),
            // A label joined by a comma or a slash, printed in two languages.
            ("Autoritatea emitentă, București", Some("București")),
            (
                "Autoritatea emitentă / Authority Acme Registry",
                Some("Acme Registry"),
            ),
            // A name that merely starts with a small word is not a label.
            ("De Beers, Inc", Some("De Beers, Inc")),
        ] {
            assert_eq!(correspondent_name(raw).as_deref(), want, "{raw}");
        }
    }

    #[test]
    fn a_field_value_loses_a_label_joined_by_a_comma() {
        let none = Labels::new();
        assert_eq!(
            field_text("issuing_authority", &none, "Autoritatea emitentă, ASP"),
            "ASP"
        );
        assert_eq!(
            field_text("issuing_authority", &none, "ASP, Chișinău"),
            "ASP, Chișinău"
        );
    }

    #[test]
    fn placeholders_and_bare_labels_are_not_correspondents() {
        for raw in [
            "",
            "   ",
            "[illegible]",
            "illegible",
            "Illegible authority",
            "unknown",
            "N/A",
            "неразборчиво",
            "nelizibil",
            "Issuing authority:",
            "Autoritatea emitentă",
            "12.04.2020",
            "-",
            "A",
        ] {
            assert_eq!(correspondent_name(raw), None, "{raw}");
        }
    }

    // --- titles -----------------------------------------------------------

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// The labels of a few types in two languages, as the taxonomy has them.
    fn type_labels() -> Vec<String> {
        strings(&[
            "Passport",
            "Паспорт",
            "Travel passport",
            "Загранпаспорт",
            "Receipt",
            "Чек",
            "Payment confirmation",
            "Подтверждение оплаты",
            "Other",
            "Другое",
        ])
    }

    fn shaped(raw: &str, correspondents: &[&str]) -> String {
        title(
            raw,
            &TitleContext {
                correspondents: &strings(correspondents),
                type_labels: &type_labels(),
            },
        )
    }

    #[test]
    fn a_type_label_in_front_of_the_title_is_dropped_in_any_language() {
        for (raw, want) in [
            (
                "Чек — Acme Market · 450.00 MDL · 12.03.2026",
                "Acme Market · 450.00 MDL · 12.03.2026",
            ),
            (
                "Receipt - Acme Market · 450.00 MDL",
                "Acme Market · 450.00 MDL",
            ),
            ("Паспорт: Иван Тестов, 2031", "Иван Тестов, 2031"),
            ("passport | Ivan Testov, 2031", "Ivan Testov, 2031"),
            // The catch-all type is a label like any other.
            (
                "Другое — Список лечебных учреждений",
                "Список лечебных учреждений",
            ),
            // The longer label wins over the shorter one it contains.
            ("Загранпаспорт — Иван Тестов", "Иван Тестов"),
        ] {
            assert_eq!(shaped(raw, &[]), want, "{raw}");
        }
    }

    #[test]
    fn a_type_written_twice_is_dropped_both_times() {
        assert_eq!(shaped("Подтверждение оплаты — Чек — 2026", &[]), "2026");
        assert_eq!(
            shaped("Подтверждение оплаты — Платёжное поручение — 2026", &[]),
            "Платёжное поручение — 2026"
        );
    }

    #[test]
    fn prose_that_starts_with_a_label_and_a_title_that_is_only_one_stay() {
        for raw in [
            "Passport of Ivan Testov",
            "Чек на оплату услуг",
            "Passport",
            "Чек —",
        ] {
            let want = raw.trim_end_matches(" —");
            assert_eq!(shaped(raw, &[]), want, "{raw}");
        }
    }

    #[test]
    fn a_correspondent_in_front_of_the_type_label_goes_with_it() {
        let names = ["Acme Registry", "ACME REG"];
        for raw in [
            "Acme Registry - Passport — Ivan Testov, 2024",
            "Acme Registry: Passport — Ivan Testov, 2024",
            "ACME  REGISTRY — Passport — Ivan Testov, 2024",
            "acme registry | Passport — Ivan Testov, 2024",
            // No separator, but what follows is the type label.
            "Acme Registry Passport — Ivan Testov, 2024",
        ] {
            assert_eq!(shaped(raw, &names), "Ivan Testov, 2024", "{raw}");
        }
        // Across scripts.
        assert_eq!(
            shaped(
                "Agentstvo Akme - Passport — Ivan Testov",
                &["Агентство Акме"]
            ),
            "Ivan Testov"
        );
    }

    #[test]
    fn a_correspondent_that_is_the_subject_of_the_title_stays() {
        // A receipt is named for its shop: no type label follows the name.
        assert_eq!(
            shaped("Acme Market · 450.00 MDL · 12.03.2026", &["Acme Market"]),
            "Acme Market · 450.00 MDL · 12.03.2026"
        );
        assert_eq!(
            shaped("Чек — Acme Market · 450.00 MDL", &["Acme Market"]),
            "Acme Market · 450.00 MDL"
        );
        assert_eq!(shaped("Acme Registry", &["Acme Registry"]), "Acme Registry");
    }

    #[test]
    fn a_file_extension_is_dropped() {
        assert_eq!(shaped("Scan 2024.pdf", &[]), "Scan 2024");
        assert_eq!(shaped("Scan 2024.JPEG", &[]), "Scan 2024");
        assert_eq!(shaped("Version 2.5", &[]), "Version 2.5");
        assert_eq!(
            shaped("  Sticker   from the garage.pdf ", &[]),
            "Sticker from the garage"
        );
    }

    // --- organization names -----------------------------------------------

    #[test]
    fn spaces_just_inside_quotes_are_removed_from_an_organization_name() {
        for (raw, want) in [
            // The name seen in a real archive.
            (
                r#"ООО " ТЕСТОВАЯ ЛАБОРАТОРИЯ""#,
                r#"ООО "ТЕСТОВАЯ ЛАБОРАТОРИЯ""#,
            ),
            (
                r#"ООО "ТЕСТОВАЯ ЛАБОРАТОРИЯ" "#,
                r#"ООО "ТЕСТОВАЯ ЛАБОРАТОРИЯ""#,
            ),
            (r#"ООО  "  Ромашка  ""#, r#"ООО "Ромашка""#),
            ("ООО « Ромашка »", "ООО «Ромашка»"),
            // Typographic quotes become the plain ones; guillemets stay.
            ("ООО „Ромашка”", r#"ООО "Ромашка""#),
            ("ООО “ Ромашка ”", r#"ООО "Ромашка""#),
            ("ООО«Ромашка»", "ООО «Ромашка»"),
            // Case is never changed.
            (r#"Acme "Rentals" Ltd"#, r#"Acme "Rentals" Ltd"#),
            // An odd number of quotes keeps its spacing.
            (r#"5" screens Ltd"#, r#"5" screens Ltd"#),
        ] {
            assert_eq!(correspondent_name(raw).as_deref(), Some(want), "{raw}");
        }
    }

    // --- language names ---------------------------------------------------

    #[test]
    fn locales_are_named_for_the_model() {
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("ru"), "Russian");
        assert_eq!(language_name("ru-RU"), "Russian");
        assert_eq!(language_name("de"), "de");
    }
}
