//! UK NHS number: 10 digits. The last digit is a modulus-11 check digit over
//! the first nine, weighted 10 down to 2.
//!
//! Source: NHS Digital, NHS Data Model and Dictionary, "NHS number" data
//! standard (modulus-11 check). The exact document title and URL were not
//! identified in this session, and the text was not checked. The weights and
//! the 11 -> 0 and 10 -> invalid cases come from the task specification.
//! Treat this citation as unverified. Test numbers use the 999 prefix, which
//! the task specification names as the published test range.

use super::Validation;

pub fn is_format(s: &str) -> bool {
    s.len() == 10 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let digits: Vec<u32> = s.bytes().map(|b| u32::from(b - b'0')).collect();
    let sum: u32 = digits[..9]
        .iter()
        .zip((2..=10).rev())
        .map(|(d, w)| d * w)
        .sum();
    let check = match 11 - sum % 11 {
        11 => 0,
        // A remainder that yields 10 has no valid check digit.
        10 => return Validation::Invalid,
        c => c,
    };
    if check == digits[9] {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_check_digits() {
        assert_eq!(validate("9990000018"), Validation::Valid);
        assert_eq!(validate("9990000026"), Validation::Valid);
    }

    #[test]
    fn check_value_eleven_becomes_zero() {
        // The weighted sum of 999000005 leaves remainder 0, so the check is 11 -> 0.
        assert_eq!(validate("9990000050"), Validation::Valid);
    }

    #[test]
    fn check_value_ten_is_always_invalid() {
        // The weighted sum of 999000000 leaves remainder 1, so no tenth digit works.
        for last in b'0'..=b'9' {
            let s = format!("999000000{}", last as char);
            assert_eq!(validate(&s), Validation::Invalid, "{s}");
        }
    }

    #[test]
    fn one_digit_change_is_invalid() {
        assert_eq!(validate("9990000019"), Validation::Invalid);
        assert_eq!(validate("9990000028"), Validation::Invalid);
        assert_eq!(validate("9990000027"), Validation::Invalid);
    }

    #[test]
    fn wrong_shape_is_not_applicable() {
        assert_eq!(validate("999000001"), Validation::NotApplicable);
        assert_eq!(validate("99900000180"), Validation::NotApplicable);
        assert_eq!(validate("99900000a8"), Validation::NotApplicable);
        assert_eq!(validate(""), Validation::NotApplicable);
    }
}
