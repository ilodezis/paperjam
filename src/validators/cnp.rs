//! Romanian CNP (personal numeric code), 13 digits: sex/century `S`,
//! birth date `YYMMDD`, county `JJ`, serial `NNN`, check digit `C`.
//!
//! Source: Romanian Government Decision 1375/2006, Annex 1 (the control
//! number "279146358279"): the first twelve digits are multiplied by
//! 2 7 9 1 4 6 3 5 8 2 7 9, summed, taken modulo 11; a remainder of 10
//! becomes 1.

use super::Validation;

const WEIGHTS: [u32; 12] = [2, 7, 9, 1, 4, 6, 3, 5, 8, 2, 7, 9];

pub fn is_format(digits: &str) -> bool {
    digits.len() == 13
        && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(digits.as_bytes()[0], b'1'..=b'8')
}

pub fn validate(digits: &str) -> Validation {
    if !is_format(digits) {
        return Validation::NotApplicable;
    }
    let b = digits.as_bytes();
    let num = |from: usize, to: usize| digits[from..to].parse::<u32>().unwrap_or(0);
    // Month 01-12, day 01-31; the county code (JJ) is 01-52 (52 = Bucharest
    // sector range), anything else is not a CNP.
    let (month, day, county) = (num(3, 5), num(5, 7), num(7, 9));
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1..=52).contains(&county) {
        return Validation::Invalid;
    }
    let sum: u32 = b[..12]
        .iter()
        .zip(WEIGHTS)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    let mut control = sum % 11;
    if control == 10 {
        control = 1;
    }
    if control == u32::from(b[12] - b'0') {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}
