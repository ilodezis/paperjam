//! Russian INN (taxpayer number), 10 digits for legal entities and 12 for
//! individuals.
//!
//! Source: Federal Tax Service order ММВ-7-6/435@ (2012) "check digit
//! algorithm"; the weights below are the published ones. The check digit is
//! `(Σ dᵢ·wᵢ mod 11) mod 10`.

use super::Validation;

const W10: [u32; 9] = [2, 4, 10, 3, 5, 9, 4, 6, 8];
const W11: [u32; 10] = [7, 2, 4, 10, 3, 5, 9, 4, 6, 8];
const W12: [u32; 11] = [3, 7, 2, 4, 10, 3, 5, 9, 4, 6, 8];

pub fn is_format(digits: &str) -> bool {
    matches!(digits.len(), 10 | 12) && digits.bytes().all(|b| b.is_ascii_digit())
}

fn control(digits: &[u8], weights: &[u32]) -> u32 {
    let sum: u32 = digits
        .iter()
        .zip(weights)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    sum % 11 % 10
}

pub fn validate(digits: &str) -> Validation {
    if !is_format(digits) {
        return Validation::NotApplicable;
    }
    let b = digits.as_bytes();
    let digit = |i: usize| u32::from(b[i] - b'0');
    let ok = if b.len() == 10 {
        control(&b[..9], &W10) == digit(9)
    } else {
        control(&b[..10], &W11) == digit(10) && control(&b[..11], &W12) == digit(11)
    };
    if ok {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}
