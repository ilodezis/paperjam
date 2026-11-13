//! ICAO 9303 machine-readable zone parser (TD1, TD2, TD3).
//!
//! Source: ICAO Doc 9303 Part 3 §4.9 (check digits: weights 7-3-1 repeated,
//! digits keep their value, A-Z map to 10-35, `<` is 0, sum modulo 10),
//! Part 4 (TD3 passports), Part 5 (TD1 cards), Part 6 (TD2).
//!
//! Also the zone of the Russian internal passport: TD3-sized, document code
//! `PN`, no expiry date, names in a Russian-specific Latin coding (digits
//! stand for some Cyrillic letters) and the series, issue date and department
//! code in the optional data.

use chrono::{Datelike, NaiveDate, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MrzFormat {
    TD1,
    TD2,
    TD3,
    /// Russian internal passport (`PNRUS`, 2x44).
    RuInternal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mrz {
    pub format: MrzFormat,
    pub document_number: String,
    pub surname: String,
    pub given_names: String,
    pub nationality: String,
    pub birth_date: NaiveDate,
    pub sex: Option<char>,
    /// `None` when the zone leaves it blank (the Russian internal passport).
    pub expiry_date: Option<NaiveDate>,
    pub personal_number: Option<String>,
    /// The series printed before the number (Russian internal passport),
    /// as printed: "45 09".
    pub series: Option<String>,
    pub issue_date: Option<NaiveDate>,
    pub checks_ok: bool,
}

/// ICAO check digit of `field`, `None` if it holds a character outside
/// `0-9A-Z<`.
pub fn check_digit(field: &str) -> Option<u8> {
    const WEIGHTS: [u32; 3] = [7, 3, 1];
    let mut sum = 0;
    for (i, c) in field.chars().enumerate() {
        let value = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            '<' => 0,
            _ => return None,
        };
        sum += value * WEIGHTS[i % 3];
    }
    Some((sum % 10) as u8)
}

/// Find and parse the first machine-readable zone in OCR text. Prefers a
/// zone whose check digits all verify over one that merely parses.
pub fn find(text: &str) -> Option<Mrz> {
    let lines: Vec<Vec<char>> = text
        .lines()
        .map(clean_line)
        .filter(|l| !l.is_empty())
        .map(|l| fit_length(l.chars().collect()))
        .collect();
    let is_zone_line = |l: &Vec<char>, len: usize| {
        l.len() == len && l.iter().all(|c| matches!(c, '0'..='9' | 'A'..='Z' | '<'))
    };
    let mut lines = lines;
    for i in 0..lines.len().saturating_sub(1) {
        let target = lines[i + 1].len();
        let all_zone_chars = lines[i]
            .iter()
            .all(|c| matches!(c, '0'..='9' | 'A'..='Z' | '<'));
        if matches!(target, 36 | 44) && is_zone_line(&lines[i + 1], target) && all_zone_chars {
            lines[i] = fit_names_line(std::mem::take(&mut lines[i]), target);
        }
    }
    let mut first_parsed = None;
    let no_names = vec!['<'; 30];
    for i in 0..lines.len() {
        let zone = if i + 2 < lines.len() && (0..3).all(|k| is_zone_line(&lines[i + k], 30)) {
            parse_td1(&lines[i], &lines[i + 1], &lines[i + 2])
        } else if i + 1 < lines.len()
            && is_zone_line(&lines[i], 30)
            && is_zone_line(&lines[i + 1], 30)
        {
            // The names line comes last and is read worst (Cyrillic look-alikes,
            // a cut-off line). The two lines above carry every check digit, so
            // the zone still counts; it just brings no names.
            parse_td1(&lines[i], &lines[i + 1], &no_names)
        } else if i + 1 < lines.len()
            && is_zone_line(&lines[i], 44)
            && is_zone_line(&lines[i + 1], 44)
        {
            parse_two_line(MrzFormat::TD3, &lines[i], &lines[i + 1])
        } else if i + 1 < lines.len()
            && is_zone_line(&lines[i], 36)
            && is_zone_line(&lines[i + 1], 36)
        {
            parse_two_line(MrzFormat::TD2, &lines[i], &lines[i + 1])
        } else {
            None
        };
        if let Some(zone) = zone {
            if zone.checks_ok {
                return Some(zone);
            }
            first_parsed.get_or_insert(zone);
        }
    }
    first_parsed
}

/// Line lengths of the machine-readable zone formats (TD1, TD2, TD3).
const LINE_LENGTHS: [usize; 3] = [30, 36, 44];
/// How far off a names line may be and still be fitted to its second line.
const MAX_NAMES_DRIFT: usize = 30;

/// OCR sometimes drops or invents a character inside a run of filler, which
/// leaves the line a few characters off its format's length. When exactly one
/// length is that close, trims or extends the longest `<` run to reach it.
fn fit_length(line: Vec<char>) -> Vec<char> {
    let mut close = LINE_LENGTHS
        .iter()
        .filter(|&&n| n != line.len() && n.abs_diff(line.len()) <= 3);
    let (Some(&target), None) = (close.next(), close.next()) else {
        return line;
    };
    resize_filler(line, target)
}

/// The names line of a two-line zone ends in a long run of `<` that OCR miscounts
/// by more than [`fit_length`] forgives. Its length is known from the second
/// line (the one with the check digits), so the run is resized to match.
fn fit_names_line(line: Vec<char>, target: usize) -> Vec<char> {
    let names_line = line.len() >= 8
        && line.len().abs_diff(target) <= MAX_NAMES_DRIFT
        && line[0].is_ascii_uppercase()
        // `P<`, or a two-letter document code: `PN`, `PP`, `PA`.
        && (line[1] == '<' || line[1].is_ascii_uppercase());
    if line.len() == target || !names_line {
        return line;
    }
    resize_filler(line, target)
}

/// Trims or extends the longest `<` run of `line` until it is `target` long.
fn resize_filler(mut line: Vec<char>, target: usize) -> Vec<char> {
    if line.len() > target {
        // A K glued to the last name letter and the filler is more likely
        // noise than a letter once the line is too long (`IVANK<<<`).
        // A K before `<<` and another name ("NOVAK<<IVAN") is a letter.
        if let Some(k) = (1..line.len().saturating_sub(3)).find(|&k| {
            line[k] == 'K'
                && line[k + 1..k + 4].iter().all(|c| *c == '<')
                && line[k - 1].is_ascii_alphabetic()
        }) {
            line.remove(k);
        }
    }
    let Some((start, len)) = longest_filler_run(&line).filter(|&(_, len)| len >= 3) else {
        return line;
    };
    if line.len() < target {
        let missing = target - line.len();
        line.splice(start..start, std::iter::repeat_n('<', missing));
    } else if line.len() > target {
        // Keep at least two characters of the run.
        let cut = (line.len() - target).min(len - 2);
        line.drain(start..start + cut);
    }
    line
}

/// `(start, length)` of the longest run of `<`.
fn longest_filler_run(line: &[char]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < line.len() {
        if line[i] != '<' {
            i += 1;
            continue;
        }
        let start = i;
        while i < line.len() && line[i] == '<' {
            i += 1;
        }
        if best.is_none_or(|(_, len)| i - start > len) {
            best = Some((start, i - start));
        }
    }
    best
}

/// Strip whitespace, upper-case, and repair filler (`<`) that OCR read as
/// `«` or `K`.
fn clean_line(raw: &str) -> String {
    let chars: Vec<char> = raw
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            if c == '«' {
                '<'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if !matches!(chars[i], '<' | 'K') {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && matches!(chars[j], '<' | 'K') {
            j += 1;
        }
        let run = &chars[i..j];
        let has_filler = run.contains(&'<');
        if run.len() >= 3 && (has_filler || run.len() >= 5) {
            // A single K glued to a letter and followed by real filler is
            // most likely the last letter of a name ("ISAK<<<").
            let keep_first = has_filler
                && run[0] == 'K'
                && run[1] == '<'
                && i > 0
                && chars[i - 1].is_ascii_alphabetic();
            for (k, c) in run.iter().enumerate() {
                out.push(if k == 0 && keep_first { *c } else { '<' });
            }
        } else {
            out.extend(run);
        }
        i = j;
    }
    out
}

/// Replace the OCR confusion `O` by `0` inside the given numeric ranges.
fn repair_numeric(line: &[char], ranges: &[std::ops::Range<usize>]) -> Vec<char> {
    let mut fixed = line.to_vec();
    for range in ranges {
        for c in &mut fixed[range.clone()] {
            if *c == 'O' {
                *c = '0';
            }
        }
    }
    fixed
}

fn text(chars: &[char]) -> String {
    chars.iter().collect()
}

fn verified(field: &[char], check: char) -> bool {
    check
        .to_digit(10)
        .is_some_and(|d| check_digit(&text(field)) == Some(d as u8))
}

fn optional_verified(field: &[char], check: char) -> bool {
    if field.iter().all(|c| *c == '<') {
        matches!(check, '<' | '0')
    } else {
        verified(field, check)
    }
}

fn parse_date(chars: &[char], birth: bool) -> Option<NaiveDate> {
    let digits = text(chars);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let yy: i32 = digits[..2].parse().ok()?;
    let month: u32 = digits[2..4].parse().ok()?;
    let day: u32 = digits[4..6].parse().ok()?;
    let mut year = 2000 + yy;
    if birth && year > Utc::now().year() {
        year -= 100;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

/// A country or issuer code: letters only, so an OCR zero is an O.
fn country(field: &[char]) -> String {
    text(field).trim_matches('<').replace('0', "O")
}

/// Surname and given names of a names field. Names have no check digit, so a
/// field that cannot be split safely brings no names at all rather than a
/// guess: several words without the `<<` between surname and given names
/// (OCR lost it) would put the given names into the surname.
fn parse_names(field: &[char]) -> (String, String) {
    let all = text(field);
    // The names end where the filler starts; anything OCR invents inside the
    // filler run is not part of them.
    let all = all.split("<<<").next().unwrap_or_default();
    let words = |s: &str| {
        s.split('<')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let (surname, given) = match all.split_once("<<") {
        Some((surname, given)) => (words(surname), words(given)),
        None if words(all).len() > 1 => return (String::new(), String::new()),
        None => (words(all), Vec::new()),
    };
    (surname.join(" "), given.join(" "))
}

/// ICAO names hold letters only: a digit in them is an OCR misread of the
/// letter it looks like.
fn repair_icao_names(field: &[char]) -> Vec<char> {
    field
        .iter()
        .map(|c| match c {
            '0' => 'O',
            '1' => 'I',
            '2' => 'Z',
            '5' => 'S',
            '6' => 'G',
            '8' => 'B',
            other => *other,
        })
        .collect()
}

/// Codes of the Russian internal passport that stand for consonants (`Й`,
/// coded `Q`, is left out: no hard or soft sign follows it).
fn ru_consonant(c: char) -> bool {
    matches!(
        c,
        'B' | 'V'
            | 'G'
            | 'D'
            | 'J'
            | 'Z'
            | 'K'
            | 'L'
            | 'M'
            | 'N'
            | 'P'
            | 'R'
            | 'S'
            | 'T'
            | 'F'
            | 'H'
            | 'C'
            | '3'
            | '4'
            | 'W'
    )
}

/// Repairs the names field of a Russian internal passport where the coding
/// makes a misread obvious. `0`, `1` and `5` code no letter (`O`, `I`, `S`).
/// `X` codes the hard sign `Ъ`, which only ever stands between a consonant
/// and `Е`, `Ё`, `Ю` or `Я`; anywhere else it is filler OCR read as `X`.
fn repair_ru_names(field: &[char]) -> Vec<char> {
    let mut out: Vec<char> = field
        .iter()
        .map(|c| match c {
            '0' => 'O',
            '1' => 'I',
            '5' => 'S',
            other => *other,
        })
        .collect();
    for i in 0..out.len() {
        if out[i] != 'X' {
            continue;
        }
        let after_consonant = i > 0 && ru_consonant(out[i - 1]);
        let before_iotated = out
            .get(i + 1)
            .is_some_and(|c| matches!(c, 'E' | '2' | '7' | '8'));
        if !(after_consonant && before_iotated) {
            out[i] = '<';
        }
    }
    out
}

/// Whether a decoded Russian name could be a name at all: no one-letter
/// word, no word that starts with `Ъ`, `Ь` or `Ы`, no `Ъ` or `Ь` after a
/// vowel. A names line that fails is OCR noise, not a name.
fn ru_plausible(name: &str) -> bool {
    const VOWELS: &str = "АЕЁИОУЫЭЮЯ";
    name.split(' ').filter(|w| !w.is_empty()).all(|word| {
        let chars: Vec<char> = word.chars().collect();
        chars.len() >= 2
            && !matches!(chars[0], 'Ъ' | 'Ь' | 'Ы')
            && chars.windows(2).all(|p| {
                !(matches!(p[1], 'Ъ' | 'Ь') && (VOWELS.contains(p[0]) || matches!(p[0], 'Ъ' | 'Ь')))
            })
            && chars.iter().all(|c| ('А'..='Я').contains(c) || *c == 'Ё')
    })
}

fn sex(c: char) -> Option<char> {
    matches!(c, 'M' | 'F' | 'X').then_some(c)
}

fn personal(field: &[char]) -> Option<String> {
    let value = text(field).replace('<', "");
    (!value.is_empty()).then_some(value)
}

/// TD3 (passport, 2x44) and TD2 (2x36) share the layout of the second line
/// up to the expiry check digit.
fn parse_two_line(format: MrzFormat, l1: &[char], l2: &[char]) -> Option<Mrz> {
    let (optional_end, composite_at) = match format {
        MrzFormat::TD3 => (42, 43),
        _ => (35, 35),
    };
    let numeric: Vec<std::ops::Range<usize>> = if format == MrzFormat::TD3 {
        vec![9..10, 13..20, 21..28, 42..44]
    } else {
        vec![9..10, 13..20, 21..28, 35..36]
    };
    if format == MrzFormat::TD3 {
        if l1.starts_with(&['P', 'N', 'R', 'U', 'S']) {
            return parse_ru_internal(&l1[5..], l2);
        }
        // A speck before the code (`PMNRUS`): the filler at the end makes up
        // for the extra character, so the names merely start one later.
        if l1[0] == 'P' && l1[2..6] == ['N', 'R', 'U', 'S'] {
            return parse_ru_internal(&l1[6..], l2);
        }
    }
    let l2 = repair_numeric(l2, &numeric);
    if !l1[0].is_ascii_alphabetic() {
        return None;
    }
    let birth_date = parse_date(&l2[13..19], true)?;
    let (expiry_date, expiry_ok) = expiry(&l2[21..28])?;

    let optional = &l2[28..optional_end];
    let mut checks_ok = verified(&l2[0..9], l2[9]) && verified(&l2[13..19], l2[19]) && expiry_ok;
    if format == MrzFormat::TD3 {
        checks_ok &= optional_verified(optional, l2[42]);
    }
    let composite: Vec<char> = [&l2[0..10], &l2[13..20], &l2[21..composite_at]].concat();
    checks_ok &= verified(&composite, l2[composite_at]);

    let (surname, given_names) = parse_names(&repair_icao_names(&l1[5..]));
    Some(Mrz {
        format,
        document_number: text(&l2[0..9]).replace('<', ""),
        surname,
        given_names,
        nationality: country(&l2[10..13]),
        birth_date,
        sex: sex(l2[20]),
        expiry_date,
        personal_number: personal(optional),
        series: None,
        issue_date: None,
        checks_ok,
    })
}

/// The expiry date and its check digit (7 characters): the date and whether
/// it verifies. A field left entirely blank (`<<<<<<<`) is no date and no
/// error; `None` when the date is neither blank nor a date.
fn expiry(field: &[char]) -> Option<(Option<NaiveDate>, bool)> {
    if field.iter().all(|c| *c == '<') {
        return Some((None, true));
    }
    let date = parse_date(&field[..6], false)?;
    Some((Some(date), verified(&field[..6], field[6])))
}

/// The Latin letter or digit the Russian internal passport writes for a
/// Cyrillic letter, read backwards: `2` is `Ё`, `3` is `Ч`, and so on.
fn ru_letter(c: char) -> char {
    match c {
        'A' => 'А',
        'B' => 'Б',
        'V' => 'В',
        'G' => 'Г',
        'D' => 'Д',
        'E' => 'Е',
        '2' => 'Ё',
        'J' => 'Ж',
        'Z' => 'З',
        'I' => 'И',
        'Q' => 'Й',
        'K' => 'К',
        'L' => 'Л',
        'M' => 'М',
        'N' => 'Н',
        'O' => 'О',
        'P' => 'П',
        'R' => 'Р',
        'S' => 'С',
        'T' => 'Т',
        'U' => 'У',
        'F' => 'Ф',
        'H' => 'Х',
        'C' => 'Ц',
        '3' => 'Ч',
        '4' => 'Ш',
        'W' => 'Щ',
        'X' => 'Ъ',
        'Y' => 'Ы',
        '9' => 'Ь',
        '6' => 'Э',
        '7' => 'Ю',
        '8' => 'Я',
        other => other,
    }
}

fn ru_name(coded: &str) -> String {
    coded.chars().map(ru_letter).collect()
}

/// The code of a Cyrillic letter in the Russian internal passport, the
/// inverse of [`ru_letter`].
fn ru_code(c: char) -> Option<char> {
    Some(match c {
        'А' => 'A',
        'Б' => 'B',
        'В' => 'V',
        'Г' => 'G',
        'Д' => 'D',
        'Е' => 'E',
        'Ё' => '2',
        'Ж' => 'J',
        'З' => 'Z',
        'И' => 'I',
        'Й' => 'Q',
        'К' => 'K',
        'Л' => 'L',
        'М' => 'M',
        'Н' => 'N',
        'О' => 'O',
        'П' => 'P',
        'Р' => 'R',
        'С' => 'S',
        'Т' => 'T',
        'У' => 'U',
        'Ф' => 'F',
        'Х' => 'H',
        'Ц' => 'C',
        'Ч' => '3',
        'Ш' => '4',
        'Щ' => 'W',
        'Ъ' => 'X',
        'Ы' => 'Y',
        'Ь' => '9',
        'Э' => '6',
        'Ю' => '7',
        'Я' => '8',
        _ => return None,
    })
}

/// How the Latin spelling of a Cyrillic letter in a travel document's zone
/// may read: ICAO 9303 (since 2013) first, then the older Russian table.
fn icao_letter(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        'А' => ("A", "A"),
        'Б' => ("B", "B"),
        'В' => ("V", "V"),
        'Г' | 'Ґ' => ("G", "G"),
        'Д' => ("D", "D"),
        'Е' | 'Ё' | 'Э' => ("E", "E"),
        'Є' => ("IE", "YE"),
        'Ж' => ("ZH", "ZH"),
        'З' => ("Z", "Z"),
        'И' | 'І' => ("I", "I"),
        'Ї' => ("I", "YI"),
        'Й' => ("I", "Y"),
        'К' => ("K", "K"),
        'Л' => ("L", "L"),
        'М' => ("M", "M"),
        'Н' => ("N", "N"),
        'О' => ("O", "O"),
        'П' => ("P", "P"),
        'Р' => ("R", "R"),
        'С' => ("S", "S"),
        'Т' => ("T", "T"),
        'У' | 'Ў' => ("U", "U"),
        'Ф' => ("F", "F"),
        'Х' => ("KH", "KH"),
        'Ц' => ("TS", "TS"),
        'Ч' => ("CH", "CH"),
        'Ш' => ("SH", "SH"),
        'Щ' => ("SHCH", "SHCH"),
        'Ъ' => ("IE", ""),
        'Ы' => ("Y", "Y"),
        'Ь' => ("", ""),
        'Ю' => ("IU", "YU"),
        'Я' => ("IA", "YA"),
        _ => return None,
    })
}

/// How a name read from the zone compares with the same name read off the
/// printed page (by the model). Names carry no check digit, so the printed
/// reading is the one to trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameAgreement {
    /// The same name: the zone's spelling stands.
    Same,
    /// The same name once the usual OCR confusions of the zone are forgiven
    /// (`8`/`B`, `0`/`O`, `1`/`I`, filler read as `X`): the zone holds a
    /// misread and the printed spelling replaces it.
    Misread,
    /// Different names: the zone's names line is noise.
    Different,
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// Words of a name in zone characters (`A-Z`, `0-9`): apostrophes dropped,
/// any other separator splits.
fn zone_words(s: &str) -> Vec<String> {
    s.chars()
        .filter(|c| !matches!(c, '\'' | '’' | 'ʼ'))
        .collect::<String>()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Upper-case Latin with the diacritics folded away; with `expand`, the
/// ICAO spellings of umlauts and ligatures (`Ü` → `UE`, `ß` → `SS`).
fn latin(s: &str, expand: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let upper: String = c.to_uppercase().collect();
        let expanded = match upper.as_str() {
            "Ä" | "Æ" if expand => Some("AE"),
            "Ö" | "Ø" | "Œ" if expand => Some("OE"),
            "Ü" if expand => Some("UE"),
            "Å" if expand => Some("AA"),
            "ẞ" | "SS" => Some("SS"),
            "Þ" => Some("TH"),
            _ => None,
        };
        match expanded {
            Some(e) => out.push_str(e),
            None => out.push_str(&crate::matching::normalize(&upper).to_uppercase()),
        }
    }
    out
}

/// The spellings a printed name may take in the zone.
fn zone_spellings(visual: &str, russian_internal: bool) -> Vec<Vec<String>> {
    if russian_internal {
        let coded: String = visual
            .to_uppercase()
            .chars()
            .map(|c| ru_code(c).unwrap_or(c))
            .collect();
        return vec![zone_words(&coded)];
    }
    let mut spellings = Vec::new();
    if visual.chars().any(is_cyrillic) {
        for old in [false, true] {
            let spelled: String = visual
                .to_uppercase()
                .chars()
                .map(|c| match icao_letter(c) {
                    Some((icao, older)) => (if old { older } else { icao }).to_string(),
                    None => c.to_string(),
                })
                .collect();
            spellings.push(zone_words(&latin(&spelled, true)));
        }
    }
    for expand in [true, false] {
        spellings.push(zone_words(&latin(visual, expand)));
    }
    spellings
}

/// Folds the characters OCR confuses in a zone into one, `russian_internal`
/// also the hard sign `X` into filler.
fn fold_misreads(words: &[String], russian_internal: bool) -> Vec<String> {
    let folded: String = words
        .join("<")
        .chars()
        .map(|c| match c {
            '8' => 'B',
            '0' | 'Q' | 'D' => 'O',
            '1' => 'I',
            '5' => 'S',
            '2' => 'Z',
            '6' => 'G',
            'X' if russian_internal => '<',
            other => other,
        })
        .collect();
    zone_words(&folded)
}

/// A Russian patronymic (Петрович, Петровна, Ильич, Кузьминична).
fn is_patronymic(word: &str) -> bool {
    let word = word.to_lowercase();
    ["ович", "евич", "ич", "овна", "евна", "ична", "инична"]
        .iter()
        .any(|ending| word.chars().count() > ending.chars().count() + 1 && word.ends_with(ending))
}

/// Compares a name stored from the zone with the printed reading of the same
/// field. The zone's name may be cut short (whole words missing at the end);
/// in a travel document the printed reading may also leave out further given
/// names. A Russian internal passport's zone is never longer than the
/// printed name: extra words there are the next field run in.
pub fn compare_name(zone_value: &str, visual: &str) -> NameAgreement {
    let russian_internal = zone_value.chars().any(is_cyrillic);
    let zone = if russian_internal {
        zone_spellings(zone_value, true).remove(0)
    } else {
        zone_words(&latin(zone_value, false))
    };
    let spellings = zone_spellings(visual, russian_internal);
    if zone.is_empty() || spellings.iter().all(Vec::is_empty) {
        return NameAgreement::Same;
    }
    // A Russian internal zone holds the given name and the patronymic; a
    // reading of the given name alone leaves out only the patronymic.
    let ends_in_patronymic = zone_value
        .split_whitespace()
        .last()
        .is_some_and(is_patronymic);
    let only_patronymic_left_out = |zone: &[String], printed: &[String]| {
        ends_in_patronymic
            && !printed.is_empty()
            && zone.len() == printed.len() + 1
            && zone.starts_with(printed)
    };
    let agree = |zone: &[String], printed: &[String]| {
        printed.starts_with(zone)
            || (!russian_internal && zone.starts_with(printed))
            || (russian_internal && only_patronymic_left_out(zone, printed))
    };
    let usable = || spellings.iter().filter(|s| !s.is_empty());
    if usable().any(|s| agree(&zone, s)) {
        return NameAgreement::Same;
    }
    let zone = fold_misreads(&zone, russian_internal);
    if usable().any(|s| agree(&zone, &fold_misreads(s, russian_internal))) {
        NameAgreement::Misread
    } else {
        NameAgreement::Different
    }
}

/// The Russian internal passport: line 2 holds the first three digits of the
/// series and the number (positions 1-9, check digit 10), nationality, birth
/// date, sex, a blank expiry (22-28), then the last digit of the series, the
/// issue date and the department code (29-41), the check digit of that
/// optional part (43) and the composite check digit (44).
fn parse_ru_internal(names: &[char], l2: &[char]) -> Option<Mrz> {
    let l2 = repair_numeric(l2, &[0..10, 13..20, 28..41, 42..44]);
    let birth_date = parse_date(&l2[13..19], true)?;
    let issue_date = parse_date(&l2[29..35], true);
    let blank_expiry = l2[21..28].iter().all(|c| *c == '<');
    let composite: Vec<char> = [&l2[0..10], &l2[13..20], &l2[21..43]].concat();
    let checks_ok = blank_expiry
        && verified(&l2[0..9], l2[9])
        && verified(&l2[13..19], l2[19])
        && verified(&l2[28..42], l2[42])
        && verified(&composite, l2[43]);
    let series = format!("{} {}{}", text(&l2[0..2]), l2[2], l2[28]);
    let (surname, given_names) = parse_names(&repair_ru_names(names));
    let (mut surname, mut given_names) = (ru_name(&surname), ru_name(&given_names));
    if !ru_plausible(&surname) || !ru_plausible(&given_names) {
        // Like an unreadable names line: the zone brings no names.
        surname.clear();
        given_names.clear();
    }
    Some(Mrz {
        format: MrzFormat::RuInternal,
        document_number: text(&l2[3..9]),
        surname,
        given_names,
        nationality: country(&l2[10..13]),
        birth_date,
        sex: sex(l2[20]),
        expiry_date: None,
        personal_number: None,
        series: Some(series),
        issue_date,
        checks_ok,
    })
}

fn parse_td1(l1: &[char], l2: &[char], l3: &[char]) -> Option<Mrz> {
    let range = 14..15;
    let l1 = repair_numeric(l1, std::slice::from_ref(&range));
    let l2 = repair_numeric(l2, &[0..7, 8..15, 29..30]);
    if !l1[0].is_ascii_alphabetic() {
        return None;
    }
    let birth_date = parse_date(&l2[0..6], true)?;
    let expiry_date = parse_date(&l2[8..14], false)?;

    // A document number longer than nine characters spills into the
    // optional field (check digit position holds `<`); that form is not
    // supported and simply fails its check.
    let composite: Vec<char> = [&l1[5..30], &l2[0..7], &l2[8..15], &l2[18..29]].concat();
    let checks_ok = verified(&l1[5..14], l1[14])
        && verified(&l2[0..6], l2[6])
        && verified(&l2[8..14], l2[14])
        && verified(&composite, l2[29]);

    let (surname, given_names) = parse_names(l3);
    Some(Mrz {
        format: MrzFormat::TD1,
        document_number: text(&l1[5..14]).replace('<', ""),
        surname,
        given_names,
        nationality: country(&l2[15..18]),
        birth_date,
        sex: sex(l2[7]),
        expiry_date: Some(expiry_date),
        personal_number: personal(&l1[15..30]),
        series: None,
        issue_date: None,
        checks_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TD3_L1: &str = "P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<";
    const TD3_L2: &str = "L898902C36UTO7408122F1204159ZE184226B<<<<<10";
    const TD1: &str = "I<UTOD231458907<<<<<<<<<<<<<<<\n7408122F1204159UTO<<<<<<<<<<<6\nERIKSSON<<ANNA<MARIA<<<<<<<<<<";

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn check_digit_icao_examples() {
        assert_eq!(check_digit("L898902C3"), Some(6));
        assert_eq!(check_digit("740812"), Some(2));
        assert_eq!(check_digit("120415"), Some(9));
        assert_eq!(check_digit("ZE184226B<<<<<"), Some(1));
        assert_eq!(check_digit("abc"), None);
    }

    #[test]
    fn mrz_td3_icao_specimen() {
        let text = format!("some header\n{TD3_L1}\n{TD3_L2}\nfooter");
        let m = find(&text).expect("mrz");
        assert_eq!(m.format, MrzFormat::TD3);
        assert_eq!(m.document_number, "L898902C3");
        assert_eq!(m.surname, "ERIKSSON");
        assert_eq!(m.given_names, "ANNA MARIA");
        assert_eq!(m.nationality, "UTO");
        assert_eq!(m.birth_date, d(1974, 8, 12));
        assert_eq!(m.sex, Some('F'));
        assert_eq!(m.expiry_date, Some(d(2012, 4, 15)));
        assert_eq!(m.personal_number.as_deref(), Some("ZE184226B"));
        assert!(m.checks_ok);
    }

    /// A made-up Russian internal passport (ТЕСТОВ ПЁТР СЕРГЕЕВИЧ, series
    /// 45 09, number 123456, issued 2010-02-01 by department 770-001).
    const RU_L1: &str = "PNRUSTESTOV<<P2TR<SERGEEVI3<<<<<<<<<<<<<<<<<";
    const RU_L2: &str = "4501234568RUS9001158M<<<<<<<9100201770001<42";

    #[test]
    fn russian_internal_passport() {
        let m = find(&format!("{RU_L1}\n{RU_L2}")).expect("mrz");
        assert_eq!(m.format, MrzFormat::RuInternal);
        assert!(m.checks_ok);
        assert_eq!(m.series.as_deref(), Some("45 09"));
        assert_eq!(m.document_number, "123456");
        assert_eq!(m.surname, "ТЕСТОВ");
        assert_eq!(m.given_names, "ПЁТР СЕРГЕЕВИЧ");
        assert_eq!(m.nationality, "RUS");
        assert_eq!(m.birth_date, d(1990, 1, 15));
        assert_eq!(m.sex, Some('M'));
        assert_eq!(m.expiry_date, None);
        assert_eq!(m.issue_date, Some(d(2010, 2, 1)));
        // The series and number are not a personal number.
        assert_eq!(m.personal_number, None);
    }

    #[test]
    fn russian_internal_passport_with_a_bad_check_digit_is_flagged() {
        let broken = RU_L2.replace("4501234568", "4501234569");
        let m = find(&format!("{RU_L1}\n{broken}")).expect("mrz");
        assert!(!m.checks_ok);
    }

    #[test]
    fn russian_internal_passport_names_line_with_miscounted_filler() {
        let short = RU_L1.trim_end_matches('<').to_string() + "<<<<<<<<";
        let m = find(&format!("{short}\n{RU_L2}")).expect("mrz");
        assert!(m.checks_ok);
        assert_eq!(m.surname, "ТЕСТОВ");
    }

    #[test]
    fn mrz_td1_icao_specimen() {
        let m = find(TD1).expect("mrz");
        assert_eq!(m.format, MrzFormat::TD1);
        assert_eq!(m.document_number, "D23145890");
        assert_eq!(m.surname, "ERIKSSON");
        assert_eq!(m.given_names, "ANNA MARIA");
        assert_eq!(m.nationality, "UTO");
        assert_eq!(m.birth_date, d(1974, 8, 12));
        assert_eq!(m.sex, Some('F'));
        assert_eq!(m.expiry_date, Some(d(2012, 4, 15)));
        assert_eq!(m.personal_number, None);
        assert!(m.checks_ok);
    }

    #[test]
    fn mrz_td2_icao_specimen() {
        let text = "I<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<\nD231458907UTO7408122F1204159<<<<<<<6";
        let m = find(text).expect("mrz");
        assert_eq!(m.format, MrzFormat::TD2);
        assert_eq!(m.document_number, "D23145890");
        assert_eq!(m.surname, "ERIKSSON");
        assert!(m.checks_ok);
    }

    #[test]
    fn mrz_with_bad_check_digit_is_flagged() {
        let text = format!("{TD3_L1}\n{}", TD3_L2.replace("L898902C36", "L898902C37"));
        let m = find(&text).expect("still parsed");
        assert_eq!(m.document_number, "L898902C3");
        assert!(!m.checks_ok);
    }

    #[test]
    fn ocr_letter_o_in_numeric_positions_is_repaired() {
        // birth date "740812" read as "74O812"
        let text = format!("{TD3_L1}\n{}", TD3_L2.replace("7408122", "74O8122"));
        let m = find(&text).expect("mrz");
        assert_eq!(m.birth_date, d(1974, 8, 12));
        assert!(m.checks_ok);
    }

    #[test]
    fn ocr_letter_o_in_document_number_is_kept() {
        // alphanumeric positions are never rewritten: the check digit then fails
        let line2 = TD3_L2.replace("L898902C36", "L8O8902C36");
        let m = find(&format!("{TD3_L1}\n{line2}")).expect("mrz");
        assert_eq!(m.document_number, "L8O8902C3");
        assert!(!m.checks_ok);
    }

    #[test]
    fn a_names_line_with_a_two_letter_code_and_far_too_much_filler_is_refitted() {
        // As a model transcribes it: eight extra `<` after the names.
        let names = "PPUTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<<<<<<<<<";
        let second = "L898902C36UTO7408122F1204159ZE184226B<<<<<10";
        let m = find(&format!("{names}\n{second}")).expect("mrz");
        assert!(m.checks_ok);
        assert_eq!(m.surname, "ERIKSSON");
    }

    #[test]
    fn a_surname_ending_in_k_keeps_it_when_the_names_line_runs_long() {
        // As a model copies it: too much filler after the names.
        let names = "PNRUSTESTOVK<<P2TR<SERGEEVI3<<<<<<<<<<<<<<<<<<<<<<";
        let m = find(&format!("{names}\n{RU_L2}")).expect("mrz");
        assert!(m.checks_ok);
        assert_eq!(m.surname, "ТЕСТОВК");
    }

    #[test]
    fn a_russian_internal_second_line_with_extra_filler_still_verifies() {
        // Three `<` too many in the blank expiry, as a model wrote it.
        let second = RU_L2.replace("M<<<<<<<9", "M<<<<<<<<<<9");
        let m = find(&format!("{RU_L1}\n{second}")).expect("mrz");
        assert!(m.checks_ok);
        assert_eq!(m.document_number, "123456");
    }

    #[test]
    fn a_miscounted_filler_run_in_the_names_line_still_pairs_it_with_its_second_line() {
        // OCR is often several characters off in a long run of `<`; the
        // second line carries the check digits and fixes the length.
        for filler in [3, 12, 24, 27] {
            let line1 = format!("P<UTOERIKSSON<<ANNA<MARIA{}", "<".repeat(filler));
            let m = find(&format!(
                "{line1}
{TD3_L2}"
            ))
            .expect("mrz");
            assert_eq!(m.surname, "ERIKSSON", "filler {filler}");
            assert_eq!(m.given_names, "ANNA MARIA", "filler {filler}");
            assert!(m.checks_ok, "filler {filler}");
        }
        // A line that is not a names line is left alone.
        assert!(
            find(&format!(
                "HELLO<WORLD
{TD3_L2}"
            ))
            .is_none()
        );
    }

    #[test]
    fn filler_read_as_guillemets_or_k_is_repaired() {
        let l1 = "P<UTOERIKSSON««ANNA«MARIA«««««««««««««««««««";
        let l1k = "P<UTOERIKSSON<<ANNA<MARIAKKKKKKKKKKKKKKKKKKK";
        for line1 in [l1, l1k] {
            let m = find(&format!("{line1}\n{TD3_L2}")).expect("mrz");
            assert_eq!(m.surname, "ERIKSSON");
            assert_eq!(m.given_names, "ANNA MARIA");
            assert!(m.checks_ok);
        }
    }

    #[test]
    fn ocr_zero_in_the_country_code_is_repaired() {
        // Tesseract reads the letter O of "UTO" as a zero.
        let text = format!("{TD3_L1}\n{}", TD3_L2.replace("UTO", "UT0"));
        let m = find(&text).expect("mrz");
        assert_eq!(m.nationality, "UTO");
        assert!(m.checks_ok);
        let td1 = TD1.replace("UTO", "UT0");
        assert_eq!(find(&td1).expect("mrz").nationality, "UTO");
    }

    #[test]
    fn garbage_after_the_names_is_dropped() {
        // OCR sometimes invents letters inside the long filler run.
        let line1 = format!(
            "P<UTOERIKSSON<<ANNA{}eseKeK{}",
            "<".repeat(12),
            "<".repeat(7)
        );
        assert_eq!(line1.chars().count(), 44);
        let m = find(&format!("{line1}\n{TD3_L2}")).expect("mrz");
        assert_eq!(m.surname, "ERIKSSON");
        assert_eq!(m.given_names, "ANNA");
    }

    #[test]
    fn an_extra_character_inside_the_filler_is_absorbed() {
        // Seen from Tesseract 5.5 on Linux: 45 characters, a K glued to the
        // name and more K in the filler.
        let line1 = "P<UTOTESTOV<<IVANK<<<<<<<<<<<<<<<<<<KKKK<<<<<";
        assert_eq!(line1.chars().count(), 45);
        let line2 = "TS12345675UT08503127M32090112001234567890<16";
        let m = find(&format!(
            "{line1}
{line2}"
        ))
        .expect("mrz");
        assert_eq!(m.surname, "TESTOV");
        assert_eq!(m.given_names, "IVAN");
        assert!(m.checks_ok);
    }

    #[test]
    fn a_missing_filler_character_is_restored() {
        let line1 = format!("P<UTOERIKSSON<<ANNA{}", "<".repeat(24));
        assert_eq!(line1.chars().count(), 43);
        let m = find(&format!(
            "{line1}
{TD3_L2}"
        ))
        .expect("mrz");
        assert_eq!(m.given_names, "ANNA");
        assert!(m.checks_ok);
    }

    #[test]
    fn spaces_inside_lines_are_ignored() {
        let l2 = "L898902C36 UTO 7408122 F1204159 ZE184226B<<<<<10";
        let m = find(&format!("{TD3_L1}\n{l2}")).expect("mrz");
        assert!(m.checks_ok);
    }

    #[test]
    fn ordinary_text_has_no_mrz() {
        assert!(find("Паспорт гражданина\nIvan Testov\n1990-01-01").is_none());
        assert!(find("").is_none());
    }

    /// A made-up holder ЯТЕСТОВА АННА ПЕТРОВНА: `Я` codes as `8`, which OCR
    /// reads as `B`. The second line is [`RU_L2`]; names have no check digit.
    fn ru_names(line: &str) -> Mrz {
        let line1 = format!("{line}{}", "<".repeat(44 - line.chars().count()));
        find(&format!("{line1}\n{RU_L2}")).expect("mrz")
    }

    #[test]
    fn a_russian_name_with_the_ya_code_is_decoded() {
        let m = ru_names("PNRUS8TESTOVA<<ANNA<PETROVNA");
        assert_eq!(m.surname, "ЯТЕСТОВА");
        assert_eq!(m.given_names, "АННА ПЕТРОВНА");
    }

    #[test]
    fn a_names_line_that_lost_the_surname_separator_brings_no_names() {
        // Otherwise the given names end up in the surname.
        let m = ru_names("PNRUS8TESTOVA<ANNA<PETROVNA");
        assert!(m.checks_ok);
        assert_eq!(m.document_number, "123456");
        assert_eq!((m.surname.as_str(), m.given_names.as_str()), ("", ""));
        let td3 = find(&format!(
            "P<UTOERIKSSON<ANNA<MARIA<<<<<<<<<<<<<<<<<<<<\n{TD3_L2}"
        ))
        .expect("mrz");
        assert!(td3.checks_ok);
        assert_eq!((td3.surname.as_str(), td3.given_names.as_str()), ("", ""));
    }

    #[test]
    fn filler_read_as_x_between_russian_names_is_filler() {
        // `Ъ` never follows a vowel: the X is a misread `<`.
        let m = ru_names("PNRUS8TESTOVA<<ANNAXPETROVNA");
        assert_eq!(m.given_names, "АННА ПЕТРОВНА");
        // A real hard sign stays (ОБЪЕКТОВА).
        let m = ru_names("PNRUSOBXEKTOVA<<ANNA");
        assert_eq!(m.surname, "ОБЪЕКТОВА");
    }

    #[test]
    fn a_speck_before_the_russian_document_code_is_skipped() {
        let m = ru_names("PMNRUS8TESTOVA<<ANNA<PETROVNA");
        assert_eq!(m.format, MrzFormat::RuInternal);
        assert!(m.checks_ok);
        assert_eq!(m.surname, "ЯТЕСТОВА");
        assert_eq!(m.given_names, "АННА ПЕТРОВНА");
    }

    #[test]
    fn an_impossible_russian_name_brings_no_names() {
        // A one-letter word, a word that starts with Ь, a soft sign after a vowel.
        for line in [
            "PNRUSBTESTOVAK<<A<ANNA<PETROVNA",
            "PNRUS8TESTOVA<<9ANNA",
            "PNRUS8TESTOVA9<<ANNA",
        ] {
            let m = ru_names(line);
            assert!(m.checks_ok, "{line}");
            assert_eq!(
                (m.surname.as_str(), m.given_names.as_str()),
                ("", ""),
                "{line}"
            );
        }
    }

    #[test]
    fn digits_in_icao_names_are_the_letters_they_look_like() {
        let line1 = TD3_L1.replace("ERIKSSON", "ER1KSS0N");
        let m = find(&format!("{line1}\n{TD3_L2}")).expect("mrz");
        assert_eq!(m.surname, "ERIKSSON");
    }

    #[test]
    fn a_zone_name_that_matches_the_printed_one_stands() {
        use NameAgreement::Same;
        // Russian internal passport: the zone value is decoded Cyrillic.
        assert_eq!(compare_name("ЯТЕСТОВА", "Ятестова"), Same);
        assert_eq!(compare_name("АННА ПЕТРОВНА", "АННА ПЕТРОВНА"), Same);
        // Travel documents: ICAO and the older Russian table, case, umlauts.
        assert_eq!(compare_name("IATESTOVA", "ЯТЕСТОВА"), Same);
        assert_eq!(compare_name("YATESTOVA", "Ятестова"), Same);
        assert_eq!(compare_name("ERIKSSON", "Eriksson"), Same);
        assert_eq!(compare_name("MUELLER", "Müller"), Same);
        // The given name read without the patronymic the zone carries.
        assert_eq!(compare_name("АННА ПЕТРОВНА", "Анна"), Same);
        assert_eq!(compare_name("ИВАН ИЛЬИЧ", "ИВАН"), Same);
        // A zone cut short, or further given names the page leaves out.
        assert_eq!(compare_name("ANNA", "Anna Maria"), Same);
        assert_eq!(compare_name("ANNA MARIA", "Anna"), Same);
        // Nothing printed to compare with.
        assert_eq!(compare_name("ERIKSSON", ""), Same);
    }

    #[test]
    fn a_zone_name_with_an_ocr_confusion_is_a_misread() {
        use NameAgreement::Misread;
        // `8` (Я) read as `B` (Б).
        assert_eq!(compare_name("БТЕСТОВА", "ЯТЕСТОВА"), Misread);
        assert_eq!(compare_name("ERIKSS0N", "Eriksson"), Misread);
    }

    #[test]
    fn a_garbled_zone_name_differs_from_the_printed_one() {
        use NameAgreement::Different;
        // What OCR made of a names line that lost its `<<`: the given names
        // in the surname, a stray K, filler read as X.
        assert_eq!(
            compare_name("БТЕСТОВАК Ъ АННА ПЕТРОВНА", "ЯТЕСТОВА"),
            Different
        );
        assert_eq!(compare_name("БТЕСТОВАК", "ЯТЕСТОВА"), Different);
        // A Russian internal zone never holds more than the printed name,
        // a patronymic aside.
        assert_eq!(compare_name("ЯТЕСТОВА АННА", "ЯТЕСТОВА"), Different);
        assert_eq!(compare_name("АННА ПЕТРОВНА", "Мария"), Different);
        assert_eq!(compare_name("TESTOV", "Petrov"), Different);
    }

    #[test]
    fn valid_zone_wins_over_earlier_broken_one() {
        let broken = format!("{TD3_L1}\n{}", TD3_L2.replace("L898902C36", "L898902C37"));
        let text = format!("{broken}\n\n{TD3_L1}\n{TD3_L2}");
        assert!(find(&text).expect("mrz").checks_ok);
    }
}
