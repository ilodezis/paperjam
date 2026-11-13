//! International Bank Account Number (IBAN).
//!
//! Source: ISO 13616-1:2007 (IBAN structure) and ISO/IEC 7064:2003 (the
//! MOD 97-10 check). The check: move the first four characters to the end,
//! map letters to 10..=35, and the resulting integer mod 97 must be 1.
//! Test vectors are the sample IBANs from the Wikipedia IBAN article (DE, GB,
//! FR, NO); I did not re-check them against the SWIFT IBAN Registry itself.
//! No per-country length table is applied.

use super::Validation;

pub fn is_format(s: &str) -> bool {
    let b = s.as_bytes();
    (15..=34).contains(&b.len())
        && b.iter().all(|c| c.is_ascii_alphanumeric())
        && b[..2].iter().all(|c| c.is_ascii_alphabetic())
        && b[2..4].iter().all(|c| c.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let b = s.as_bytes();
    // Rearranged string is b[4..] followed by b[..4]; reduce mod 97 digit by
    // digit so no big integer is needed.
    let rem = b[4..].iter().chain(&b[..4]).fold(0u32, |r, c| {
        let c = c.to_ascii_uppercase();
        if c.is_ascii_digit() {
            (r * 10 + u32::from(c - b'0')) % 97
        } else {
            (r * 100 + u32::from(c - b'A' + 10)) % 97
        }
    });
    if rem == 1 {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_published_sample_ibans() {
        assert_eq!(validate("DE89370400440532013000"), Validation::Valid);
        assert_eq!(validate("GB82WEST12345698765432"), Validation::Valid);
        assert_eq!(validate("FR1420041010050500013M02606"), Validation::Valid);
        assert_eq!(validate("NO9386011117947"), Validation::Valid);
    }

    #[test]
    fn accepts_lowercase() {
        assert_eq!(validate("de89370400440532013000"), Validation::Valid);
    }

    #[test]
    fn rejects_one_changed_digit() {
        assert_eq!(validate("DE89370400440532013001"), Validation::Invalid);
        assert_eq!(validate("GB82WEST12345698765433"), Validation::Invalid);
        assert_eq!(validate("FR1420041010050500013M02607"), Validation::Invalid);
        assert_eq!(validate("NO9386011117948"), Validation::Invalid);
    }

    #[test]
    fn wrong_length_is_not_applicable() {
        assert_eq!(validate("DE89"), Validation::NotApplicable);
        assert_eq!(
            validate(&format!("DE89{}", "0".repeat(31))),
            Validation::NotApplicable
        );
    }

    #[test]
    fn wrong_characters_are_not_applicable() {
        assert_eq!(
            validate("DE8937040044053201300-"),
            Validation::NotApplicable
        );
        assert_eq!(
            validate("1289370400440532013000"),
            Validation::NotApplicable
        );
        assert_eq!(
            validate("DEX9370400440532013000"),
            Validation::NotApplicable
        );
    }
}
