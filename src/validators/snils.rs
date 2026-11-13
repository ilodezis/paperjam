//! Russian SNILS (pension insurance number), 11 digits: nine digits of the
//! number followed by a two-digit check value.
//!
//! Source: Pension Fund of Russia, "Порядок присвоения СНИЛС": the first nine
//! digits are weighted 9 down to 1 and summed; a sum below 100 is the check
//! value, 100 and 101 give `00`, anything larger is reduced modulo 101 and
//! the same rule applies again. The rule is defined for numbers above
//! 001-001-998, smaller ones carry no check value.

use super::Validation;

pub fn is_format(digits: &str) -> bool {
    digits.len() == 11 && digits.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(digits: &str) -> Validation {
    if !is_format(digits) {
        return Validation::NotApplicable;
    }
    let b = digits.as_bytes();
    let number: u32 = digits[..9].parse().unwrap_or(0);
    if number <= 1_001_998 {
        return Validation::NotApplicable;
    }
    let sum: u32 = b[..9]
        .iter()
        .enumerate()
        .map(|(i, d)| u32::from(d - b'0') * (9 - i as u32))
        .sum();
    let mut control = sum;
    if control > 101 {
        control %= 101;
    }
    if control >= 100 {
        control = 0;
    }
    let given: u32 = digits[9..].parse().unwrap_or(u32::MAX);
    if control == given {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}
