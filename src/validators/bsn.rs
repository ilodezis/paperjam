//! Dutch BSN (burgerservicenummer), 9 digits, checked with the "11-proef".
//!
//! Source: the 11-proef as described in the Dutch Wet algemene bepalingen
//! burgerservicenummer (Wabb) and the RvIG (Dienst Basisregistraties)
//! documentation of the BSN. I could not confirm the exact article or the
//! official document from here, so the citation is unverified. Rule: weights
//! 9,8,7,6,5,4,3,2,-1 on the nine digits; the weighted sum must be divisible
//! by 11. The all-zero number satisfies the sum but is rejected explicitly.

use super::Validation;

const WEIGHTS: [i32; 9] = [9, 8, 7, 6, 5, 4, 3, 2, -1];

pub fn is_format(s: &str) -> bool {
    s.len() == 9 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    if s.bytes().all(|b| b == b'0') {
        return Validation::Invalid;
    }
    let sum: i32 = s
        .bytes()
        .zip(WEIGHTS)
        .map(|(d, w)| i32::from(d - b'0') * w)
        .sum();
    if sum % 11 == 0 {
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
        assert_eq!(validate("123456782"), Validation::Valid);
        assert_eq!(validate("987654329"), Validation::Valid);
    }

    #[test]
    fn rejects_one_changed_digit() {
        assert_eq!(validate("123456783"), Validation::Invalid);
        assert_eq!(validate("223456782"), Validation::Invalid);
        assert_eq!(validate("123456792"), Validation::Invalid);
    }

    #[test]
    fn all_zero_is_invalid() {
        assert_eq!(validate("000000000"), Validation::Invalid);
    }

    #[test]
    fn wrong_length_is_not_applicable() {
        assert_eq!(validate("12345678"), Validation::NotApplicable);
        assert_eq!(validate("1234567820"), Validation::NotApplicable);
    }

    #[test]
    fn non_digits_are_not_applicable() {
        assert_eq!(validate("12345678A"), Validation::NotApplicable);
        assert_eq!(validate("12345-782"), Validation::NotApplicable);
    }
}
