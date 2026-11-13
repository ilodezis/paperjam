//! Spanish DNI (8 digits + control letter) and NIE (X, Y or Z + 7 digits +
//! control letter). For an NIE, X, Y and Z are read as 0, 1 and 2 prefixed to
//! the seven digits. Control letter = "TRWAGMYFPDXBNJZSQVHLCKE"[number mod 23].
//!
//! Source: Real Decreto 1553/2005 (Spain, DNI/NIE regulation) and the control
//! letter algorithm as published by the Spanish national police. The exact
//! article was not verified in this session; the algorithm is taken from the
//! implementation brief.

use super::Validation;

const CONTROL_LETTERS: &[u8; 23] = b"TRWAGMYFPDXBNJZSQVHLCKE";

/// Parses a DNI or NIE into the number to reduce mod 23 and its control letter.
fn parse(s: &str) -> Option<(u64, u8)> {
    let u = s.to_ascii_uppercase();
    let (&letter, body) = u.as_bytes().split_last()?;
    if body.len() != 8 || !letter.is_ascii_uppercase() {
        return None;
    }
    let (prefix, digits) = match body[0] {
        b'X' => (0, &body[1..]),
        b'Y' => (1, &body[1..]),
        b'Z' => (2, &body[1..]),
        b'0'..=b'9' => (0, body),
        _ => return None,
    };
    if !digits.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n = digits
        .iter()
        .fold(prefix, |acc, c| acc * 10 + u64::from(c - b'0'));
    Some((n, letter))
}

pub fn is_format(s: &str) -> bool {
    parse(s).is_some()
}

pub fn validate(s: &str) -> Validation {
    match parse(s) {
        None => Validation::NotApplicable,
        Some((n, letter)) => {
            if CONTROL_LETTERS[(n % 23) as usize] == letter {
                Validation::Valid
            } else {
                Validation::Invalid
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_dni_and_nie() {
        assert_eq!(validate("12345678Z"), Validation::Valid);
        assert_eq!(validate("00000007F"), Validation::Valid);
        assert_eq!(validate("98765432M"), Validation::Valid);
        assert_eq!(validate("X1234567L"), Validation::Valid);
        assert_eq!(validate("Y0000001S"), Validation::Valid);
        assert_eq!(validate("Z7654321H"), Validation::Valid);
    }

    #[test]
    fn lowercase_is_normalised() {
        assert_eq!(validate("12345678z"), Validation::Valid);
        assert_eq!(validate("x1234567l"), Validation::Valid);
    }

    #[test]
    fn wrong_letter_or_digit_is_invalid() {
        assert_eq!(validate("12345678A"), Validation::Invalid);
        assert_eq!(validate("12355678Z"), Validation::Invalid);
        assert_eq!(validate("00000007A"), Validation::Invalid);
        assert_eq!(validate("00010007F"), Validation::Invalid);
        assert_eq!(validate("X1234567A"), Validation::Invalid);
        assert_eq!(validate("X1235567L"), Validation::Invalid);
        assert_eq!(validate("Y0000001A"), Validation::Invalid);
        assert_eq!(validate("Y0001001S"), Validation::Invalid);
    }

    #[test]
    fn bad_shape_is_not_applicable() {
        assert_eq!(validate("1234567Z"), Validation::NotApplicable);
        assert_eq!(validate("123456789"), Validation::NotApplicable);
        assert_eq!(validate("W1234567L"), Validation::NotApplicable);
        assert_eq!(validate("X123456L"), Validation::NotApplicable);
        assert_eq!(validate("12345X78Z"), Validation::NotApplicable);
        assert_eq!(validate("XX234567L"), Validation::NotApplicable);
    }
}
