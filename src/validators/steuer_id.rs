//! German tax identification number (Steuerliche Identifikationsnummer, IdNr):
//! 11 digits, first digit never 0. The 11th digit is an ISO 7064 MOD 11,10
//! check digit over the first ten digits.
//!
//! Source: Bundeszentralamt für Steuern (BZSt), IdNr as defined in §139b AO
//! (Abgabenordnung); check digit per ISO 7064 MOD 11,10. The BZSt technical
//! document was not verified in this session; the algorithm is taken from the
//! implementation brief. The digit-frequency structure rule is not implemented.

use super::Validation;

pub fn is_format(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let d: Vec<u32> = s.bytes().map(|b| u32::from(b - b'0')).collect();
    if d[0] == 0 {
        return Validation::Invalid;
    }
    let mut product = 10;
    for &digit in &d[..10] {
        let mut sum = (digit + product) % 10;
        if sum == 0 {
            sum = 10;
        }
        product = (sum * 2) % 11;
    }
    let check = match 11 - product {
        10 => 0,
        c => c,
    };
    if check == d[10] {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_checksums() {
        assert_eq!(validate("47110831514"), Validation::Valid);
        assert_eq!(validate("10000000018"), Validation::Valid);
        assert_eq!(validate("58304259761"), Validation::Valid);
    }

    #[test]
    fn wrong_check_digit_or_digit_is_invalid() {
        assert_eq!(validate("47110831515"), Validation::Invalid);
        assert_eq!(validate("47111831514"), Validation::Invalid);
        assert_eq!(validate("10000000019"), Validation::Invalid);
        assert_eq!(validate("10001000018"), Validation::Invalid);
    }

    #[test]
    fn leading_zero_is_invalid() {
        assert_eq!(validate("07110831516"), Validation::Invalid);
    }

    #[test]
    fn bad_shape_is_not_applicable() {
        assert_eq!(validate("4711083151"), Validation::NotApplicable);
        assert_eq!(validate("471108315140"), Validation::NotApplicable);
        assert_eq!(validate("4711083151A"), Validation::NotApplicable);
    }
}
