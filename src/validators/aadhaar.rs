//! Indian Aadhaar number: 12 digits. The first digit is 2 to 9, and the last
//! digit is a Verhoeff check digit computed over the first eleven.
//!
//! Source: J. Verhoeff, "Error Detecting Decimal Codes", Mathematical Centre
//! Tract 29, Amsterdam, 1969 (tract number from memory, not checked against a
//! copy). The tables `D`, `P` and `INV` are Verhoeff's: `D` is the
//! multiplication of the dihedral group D5, `P` the permutation applied per
//! position, and `INV` the group inverse. The Aadhaar format rules (12 digits,
//! leading digit 2 to 9) come from the task specification; the UIDAI document
//! was not consulted, so that part of the citation is unverified.

use super::Validation;

/// Multiplication in the dihedral group D5.
const D: [[u8; 10]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
    [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
    [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
    [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
    [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
    [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
    [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
    [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
    [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
];

/// Position permutation: a digit at position `i` is mapped by `P[i % 8]`.
const P: [[u8; 10]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
    [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
    [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
    [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
    [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
    [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
    [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
];

/// Group inverse in D5: `INV[c]` is the `x` with `D[c][x] == 0`.
const INV: [u8; 10] = [0, 4, 3, 2, 1, 5, 6, 7, 8, 9];

pub fn is_format(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit()) && matches!(s.as_bytes()[0], b'2'..=b'9')
}

/// Verhoeff check digit for `payload`, read right to left with the check
/// digit itself at position 0.
fn expected_check(payload: &[u8]) -> u8 {
    let mut c = 0usize;
    for (i, &digit) in payload.iter().rev().enumerate() {
        c = usize::from(D[c][usize::from(P[(i + 1) % 8][usize::from(digit)])]);
    }
    INV[c]
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let digits: Vec<u8> = s.bytes().map(|b| b - b'0').collect();
    if expected_check(&digits[..11]) == digits[11] {
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
        assert_eq!(validate("234567890124"), Validation::Valid);
        assert_eq!(validate("222222222227"), Validation::Valid);
        assert_eq!(validate("987654321096"), Validation::Valid);
    }

    #[test]
    fn one_digit_change_is_invalid() {
        assert_eq!(validate("234567890125"), Validation::Invalid);
        assert_eq!(validate("234568890124"), Validation::Invalid);
        assert_eq!(validate("234567890134"), Validation::Invalid);
    }

    #[test]
    fn every_single_digit_error_is_invalid() {
        // Verhoeff detects all single-digit substitutions.
        let base = b"234567890124";
        for pos in 0..base.len() {
            for digit in b'0'..=b'9' {
                if digit == base[pos] {
                    continue;
                }
                let mut changed = base.to_vec();
                changed[pos] = digit;
                let s = std::str::from_utf8(&changed).unwrap();
                // A leading 0 or 1 is not an Aadhaar shape at all.
                let expected = if pos == 0 && digit < b'2' {
                    Validation::NotApplicable
                } else {
                    Validation::Invalid
                };
                assert_eq!(validate(s), expected, "{s}");
            }
        }
    }

    #[test]
    fn leading_digit_outside_2_to_9_is_not_applicable() {
        // Both have a correct Verhoeff check digit, so only the leading digit
        // excludes them.
        assert_eq!(validate("134567890129"), Validation::NotApplicable);
        assert_eq!(validate("034567890128"), Validation::NotApplicable);
    }

    #[test]
    fn wrong_shape_is_not_applicable() {
        assert_eq!(validate("23456789012"), Validation::NotApplicable);
        assert_eq!(validate("2345678901240"), Validation::NotApplicable);
        assert_eq!(validate("23456789012a"), Validation::NotApplicable);
        assert_eq!(validate(""), Validation::NotApplicable);
    }
}
