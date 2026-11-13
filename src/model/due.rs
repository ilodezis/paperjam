//! When a document has to be renewed or replaced (a travel passport, an ID
//! card, a residence permit expire; a Russian internal passport is replaced by
//! age), so the archive can say what is about to run out.
//!
//! The date is derived from the stored fields each time a document is indexed
//! or shown; whether it is "soon" is decided at query time (`has:expiring`),
//! because an index-time flag would be stale the next day.

use chrono::{Datelike, NaiveDate};

use super::fields::FieldValue;
use crate::matching;

/// Ages at which a Russian internal passport is replaced.
const REPLACEMENT_AGES: [i32; 2] = [20, 45];

/// The field keys `due_date` reads; callers that load fields for it need
/// no others.
pub const DUE_FIELD_KEYS: [&str; 5] = [
    "expiry_date",
    "nationality",
    "issuing_authority",
    "birth_date",
    "issue_date",
];

/// The date `document` is due to be renewed or replaced, or `None` when it
/// has no such date. `field` reads a stored field value by key.
///
/// The printed `expiry_date` is the due date wherever a document has one. A
/// Russian internal passport prints none: it has to be replaced on reaching
/// 20 and 45 (Government Decree of the Russian Federation No. 828 of
/// 8 July 1997, on the passport of a citizen of the Russian Federation), so
/// its due date is the holder's 20th birthday when it was issued before that,
/// the 45th birthday when it was issued from the 20th birthday up to the
/// 45th, and none when it was issued at 45 or later (then it has no term).
pub fn due_date<'a>(
    type_key: Option<&str>,
    field: impl Fn(&str) -> Option<&'a FieldValue>,
) -> Option<NaiveDate> {
    let read = |key: &str| {
        debug_assert!(
            DUE_FIELD_KEYS.contains(&key),
            "{key} is not in DUE_FIELD_KEYS"
        );
        field(key)
    };
    let date = |key: &str| match read(key) {
        Some(FieldValue::Date(d)) => Some(*d),
        _ => None,
    };
    let text = |key: &str| match read(key) {
        Some(FieldValue::Text(t) | FieldValue::Identifier(t)) => Some(t.as_str()),
        _ => None,
    };
    if let Some(expiry) = date("expiry_date") {
        return Some(expiry);
    }
    if type_key == Some("passport") && is_russian(text("nationality"), text("issuing_authority")) {
        return replacement_due(date("birth_date")?, date("issue_date")?);
    }
    None
}

/// Whether the passport is Russian: the nationality is Russia's or the
/// issuer is the interior ministry's migration service (an internal passport
/// prints no nationality).
fn is_russian(nationality: Option<&str>, authority: Option<&str>) -> bool {
    let folded = |s: Option<&str>| s.map(matching::normalize).unwrap_or_default();
    let nationality = folded(nationality);
    let authority = folded(authority);
    let nationality_ru = nationality.starts_with("rus")
        || nationality.starts_with("ross")
        || nationality == "rf"
        || nationality == "ru";
    nationality_ru || authority.contains("mvd") || authority.contains("fms")
}

/// The birthday `age` years after `birth`; a 29 February birthday falls on
/// 1 March in a year without one.
fn birthday(birth: NaiveDate, age: i32) -> Option<NaiveDate> {
    let year = birth.year().checked_add(age)?;
    NaiveDate::from_ymd_opt(year, birth.month(), birth.day())
        .or_else(|| NaiveDate::from_ymd_opt(year, 3, 1))
}

/// The next replacement age after the issue date, as a date.
fn replacement_due(birth: NaiveDate, issued: NaiveDate) -> Option<NaiveDate> {
    REPLACEMENT_AGES
        .iter()
        .filter_map(|age| birthday(birth, *age))
        .find(|due| issued < *due)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn due(type_key: &str, values: &[(&str, FieldValue)]) -> Option<NaiveDate> {
        let fields: BTreeMap<String, FieldValue> = values
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        due_date(Some(type_key), |k| fields.get(k))
    }

    fn date(s: &str) -> FieldValue {
        FieldValue::Date(d(s))
    }

    fn text(s: &str) -> FieldValue {
        FieldValue::Text(s.into())
    }

    fn internal_passport(issued: &str) -> Option<NaiveDate> {
        due(
            "passport",
            &[
                ("birth_date", date("1990-05-17")),
                ("issue_date", date(issued)),
                ("issuing_authority", text("ГУ МВД России по г. Москве")),
            ],
        )
    }

    #[test]
    fn a_printed_expiry_date_is_the_due_date() {
        assert_eq!(
            due(
                "travel_passport",
                &[
                    ("expiry_date", date("2031-02-03")),
                    ("nationality", text("UTO")),
                ]
            ),
            Some(d("2031-02-03"))
        );
    }

    #[test]
    fn issued_at_fourteen_is_due_at_the_twentieth_birthday() {
        assert_eq!(internal_passport("2004-06-01"), Some(d("2010-05-17")));
    }

    #[test]
    fn issued_between_twenty_and_forty_five_is_due_at_the_forty_fifth_birthday() {
        assert_eq!(internal_passport("2010-05-17"), Some(d("2035-05-17")));
        assert_eq!(internal_passport("2012-01-10"), Some(d("2035-05-17")));
    }

    #[test]
    fn issued_at_forty_five_or_later_is_never_due() {
        assert_eq!(internal_passport("2035-05-17"), None);
        assert_eq!(internal_passport("2040-01-01"), None);
    }

    #[test]
    fn the_day_before_the_twentieth_birthday_is_still_the_first_term() {
        assert_eq!(internal_passport("2010-05-16"), Some(d("2010-05-17")));
    }

    #[test]
    fn a_leap_day_birthday_falls_on_the_first_of_march_in_other_years() {
        // 2004 + 45 is not a leap year.
        let due = due(
            "passport",
            &[
                ("birth_date", date("2004-02-29")),
                ("issue_date", date("2024-03-10")),
                ("nationality", text("RUS")),
            ],
        );
        assert_eq!(due, Some(d("2049-03-01")));
    }

    #[test]
    fn a_passport_of_another_country_without_expiry_has_no_due_date() {
        assert_eq!(
            due(
                "passport",
                &[
                    ("birth_date", date("1990-05-17")),
                    ("issue_date", date("2004-06-01")),
                    ("nationality", text("UTO")),
                    ("issuing_authority", text("Ministry of the Interior")),
                ]
            ),
            None
        );
    }

    #[test]
    fn a_russian_passport_needs_both_dates() {
        assert_eq!(
            due(
                "passport",
                &[
                    ("issue_date", date("2004-06-01")),
                    ("nationality", text("РОССИЯ")),
                ]
            ),
            None
        );
    }

    #[test]
    fn other_documents_have_none() {
        assert_eq!(
            due(
                "contract",
                &[
                    ("birth_date", date("1990-05-17")),
                    ("nationality", text("RUS"))
                ]
            ),
            None
        );
    }
}
