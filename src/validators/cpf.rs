//! Brazilian CPF (Cadastro de Pessoas Físicas): 11 digits, the last two of
//! which are modulus-11 check digits.
//!
//! Source: Receita Federal do Brasil, the published CPF check-digit rule.
//! The exact official publication and its number were not identified in
//! this session, and the rule was not checked against the primary text. The
//! weights (10..2, then 11..2) and the `r < 2 -> 0` rule come from the task
//! specification. Treat this citation as unverified.

use super::Validation;

/// Parse 11 ASCII digits, ignoring `.` and `/` (CPFs are often written with
/// dots). The shared normaliser in `mod.rs` removes spaces and hyphens only.
fn digits(s: &str) -> Option<[u8; 11]> {
    let mut out = [0u8; 11];
    let mut n = 0;
    for b in s.bytes().filter(|&b| b != b'.' && b != b'/') {
        if !b.is_ascii_digit() || n == out.len() {
            return None;
        }
        out[n] = b - b'0';
        n += 1;
    }
    (n == out.len()).then_some(out)
}

pub fn is_format(s: &str) -> bool {
    digits(s).is_some()
}

/// Modulus-11 check digit over `digits`, weighted `top, top-1, ..., 2`.
fn check_digit(digits: &[u8], top: u32) -> u32 {
    let sum: u32 = digits
        .iter()
        .zip((2..=top).rev())
        .map(|(d, w)| u32::from(*d) * w)
        .sum();
    let r = sum % 11;
    if r < 2 { 0 } else { 11 - r }
}

pub fn validate(s: &str) -> Validation {
    let Some(d) = digits(s) else {
        return Validation::NotApplicable;
    };
    if d.iter().all(|&x| x == d[0]) {
        return Validation::Invalid;
    }
    let first = check_digit(&d[..9], 10);
    let second = check_digit(&d[..10], 11);
    if u32::from(d[9]) == first && u32::from(d[10]) == second {
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
        assert_eq!(validate("10000000019"), Validation::Valid);
        assert_eq!(validate("00000000191"), Validation::Valid);
    }

    #[test]
    fn dots_and_slashes_are_stripped() {
        assert!(is_format("100.000.000.19"));
        assert_eq!(validate("100.000.000.19"), Validation::Valid);
        assert_eq!(validate("100/000.000.19"), Validation::Valid);
    }

    #[test]
    fn one_digit_change_is_invalid() {
        assert_eq!(validate("10000000029"), Validation::Invalid);
        assert_eq!(validate("10000000018"), Validation::Invalid);
        assert_eq!(validate("20000000019"), Validation::Invalid);
    }

    #[test]
    fn all_equal_digits_are_invalid() {
        // 00000000000 satisfies both check digits arithmetically; the
        // all-equal rule rejects it anyway.
        assert_eq!(validate("00000000000"), Validation::Invalid);
        assert_eq!(validate("11111111111"), Validation::Invalid);
    }

    #[test]
    fn wrong_shape_is_not_applicable() {
        assert_eq!(validate("1000000001"), Validation::NotApplicable);
        assert_eq!(validate("100000000190"), Validation::NotApplicable);
        assert_eq!(validate("1000000001a"), Validation::NotApplicable);
        assert_eq!(validate(""), Validation::NotApplicable);
    }
}
