//! When a document has to be renewed, replaced or paid (a travel passport,
//! an ID card, a residence permit expire; a lease runs out; an invoice is
//! due; a Russian internal passport is replaced by age), so the archive can
//! say what is about to run out.
//!
//! The date is derived from the stored fields each time a document is indexed
//! or shown; whether it is "soon" is decided at query time (`has:expiring`),
//! because an index-time flag would be stale the next day.

use chrono::{Datelike, NaiveDate};

use super::fields::FieldValue;
use crate::matching;

/// Ages at which a Russian internal passport is replaced.
const REPLACEMENT_AGES: [i32; 2] = [20, 45];

/// The date fields that can name the day a document is due, in the order a
/// type that maps to none of them (a custom type) is read.
const DATE_FIELD_KEYS: [&str; 4] = ["expiry_date", "end_date", "warranty_end_date", "due_date"];

/// The field that holds the due date of each built-in type that has one:
/// identity documents and certificates print `expiry_date`, contracts run
/// to `end_date`, a warranty ends on `warranty_end_date`, an invoice is
/// paid by `due_date`.
const TYPE_DUE_FIELDS: [(&str, &str); 11] = [
    ("passport", "expiry_date"),
    ("travel_passport", "expiry_date"),
    ("id_card", "expiry_date"),
    ("residence_permit", "expiry_date"),
    ("driver_license", "expiry_date"),
    ("certificate_other", "expiry_date"),
    ("rental_agreement", "end_date"),
    ("contract", "end_date"),
    ("insurance_policy", "end_date"),
    ("warranty", "warranty_end_date"),
    ("invoice", "due_date"),
];

/// The field keys `due_date` reads; callers that load fields for it need
/// no others.
pub const DUE_FIELD_KEYS: [&str; 8] = [
    "expiry_date",
    "end_date",
    "warranty_end_date",
    "due_date",
    "nationality",
    "issuing_authority",
    "birth_date",
    "issue_date",
];

/// The date `document` is due to be renewed, replaced or paid, or `None`
/// when it has no such date. `field` reads a stored field value by key.
///
/// A type's own field comes first (see [`TYPE_DUE_FIELDS`]); a type the
/// table does not know, usually one the operator defined, is read in the
/// order of [`DATE_FIELD_KEYS`], whichever of those fields it defines and
/// the document fills. A Russian internal passport prints none of them: it
/// has to be replaced on reaching 20 and 45 (Government Decree of the
/// Russian Federation No. 828 of 8 July 1997, on the passport of a citizen
/// of the Russian Federation), so its due date is the holder's 20th birthday
/// when it was issued before that, the 45th birthday when it was issued from
/// the 20th birthday up to the 45th, and none when it was issued at 45 or
/// later (then it has no term).
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
    let own = type_key.and_then(|key| {
        TYPE_DUE_FIELDS
            .iter()
            .find(|(t, _)| *t == key)
            .map(|(_, field)| *field)
    });
    let printed = match own {
        Some(key) => date(key),
        None => DATE_FIELD_KEYS.iter().find_map(|key| date(key)),
    };
    if let Some(due) = printed {
        return Some(due);
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
                "letter",
                &[
                    ("birth_date", date("1990-05-17")),
                    ("nationality", text("RUS"))
                ]
            ),
            None
        );
    }

    // --- the field each type is due on -----------------------------------

    #[test]
    fn a_contract_runs_to_its_end_date() {
        for key in ["rental_agreement", "contract", "insurance_policy"] {
            assert_eq!(
                due(key, &[("end_date", date("2026-12-06"))]),
                Some(d("2026-12-06")),
                "{key}"
            );
        }
    }

    #[test]
    fn a_warranty_ends_on_its_warranty_end_date() {
        assert_eq!(
            due("warranty", &[("warranty_end_date", date("2028-01-31"))]),
            Some(d("2028-01-31"))
        );
        // The type's own field only: an `expiry_date` it does not define is
        // not what a warranty is due on.
        assert_eq!(
            due("warranty", &[("expiry_date", date("2028-01-31"))]),
            None
        );
    }

    #[test]
    fn an_invoice_is_due_on_its_payment_date() {
        assert_eq!(
            due("invoice", &[("due_date", date("2026-11-25"))]),
            Some(d("2026-11-25"))
        );
    }

    #[test]
    fn a_custom_type_is_due_on_the_first_date_field_it_fills() {
        // A type the table does not know, in the documented read order.
        assert_eq!(
            due("club_card", &[("end_date", date("2027-03-01"))]),
            Some(d("2027-03-01"))
        );
        assert_eq!(
            due("club_card", &[("warranty_end_date", date("2028-03-01"))]),
            Some(d("2028-03-01"))
        );
        // Both an expiry and an end date: the expiry is read first.
        assert_eq!(
            due(
                "club_card",
                &[
                    ("end_date", date("2027-03-01")),
                    ("expiry_date", date("2031-03-01")),
                ]
            ),
            Some(d("2031-03-01"))
        );
        // None of the date fields, or the type's age rule: no due date.
        assert_eq!(
            due("club_card", &[("birth_date", date("1990-05-17"))]),
            None
        );
    }
}
