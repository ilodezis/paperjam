//! Polish PESEL (Powszechny Elektroniczny System Ewidencji Ludności), 11 digits.
//!
//! Source: the PESEL check-digit rule (weights 1,3,7,9 repeated over the first
//! ten digits; check = (10 - Σ mod 10) mod 10) as published by the Polish
//! Ministry of Digital Affairs for the PESEL number. The legal basis is the
//! Act of 24 September 2010 on the population register (ustawa o ewidencji
//! ludności). I could not confirm the exact article or the official document
//! text from here, so the citation is unverified. Only the checksum is
//! implemented; the date encoded in the first six digits is not checked.

use super::Validation;

const WEIGHTS: [u32; 10] = [1, 3, 7, 9, 1, 3, 7, 9, 1, 3];

pub fn is_format(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let b = s.as_bytes();
    let sum: u32 = b[..10]
        .iter()
        .zip(WEIGHTS)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    let check = (10 - sum % 10) % 10;
    if check == u32::from(b[10] - b'0') {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_synthetic_valid_numbers() {
        assert_eq!(validate("01234567897"), Validation::Valid);
        assert_eq!(validate("98765432107"), Validation::Valid);
    }

    #[test]
    fn rejects_one_changed_digit() {
        // Changing any single digit changes the weighted sum by a non-multiple
        // of 10, so each of these must fail.
        assert_eq!(validate("01234567896"), Validation::Invalid);
        assert_eq!(validate("11234567897"), Validation::Invalid);
        assert_eq!(validate("98765432117"), Validation::Invalid);
    }

    #[test]
    fn wrong_length_is_not_applicable() {
        assert_eq!(validate("0123456789"), Validation::NotApplicable);
        assert_eq!(validate("012345678970"), Validation::NotApplicable);
    }

    #[test]
    fn non_digits_are_not_applicable() {
        assert_eq!(validate("0123456789A"), Validation::NotApplicable);
        assert_eq!(validate("01234567-97"), Validation::NotApplicable);
    }
}
