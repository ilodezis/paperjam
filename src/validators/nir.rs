//! French social security number (NIR, "numéro d'inscription au répertoire"):
//! 15 characters, 13 identity characters followed by a 2-digit key.
//!
//! Source: INSEE, NIR / RNIPP key specification: key = 97 - (N mod 97), with
//! Corsica written as 2A (read as 19) or 2B (read as 18) in the department
//! field. The exact INSEE document and section were not verified in this
//! session; the rule is taken from the implementation brief.

use super::Validation;

pub fn is_format(s: &str) -> bool {
    let u = s.to_ascii_uppercase();
    let b = u.as_bytes();
    if b.len() != 15 {
        return false;
    }
    let dept_ok = matches!((b[5], b[6]), (b'2', b'A' | b'B'))
        || (b[5].is_ascii_digit() && b[6].is_ascii_digit());
    dept_ok
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 5 || i == 6 || c.is_ascii_digit())
}

pub fn validate(s: &str) -> Validation {
    if !is_format(s) {
        return Validation::NotApplicable;
    }
    let u = s.to_ascii_uppercase();
    let b = u.as_bytes();
    let head = u[..13].replace("2A", "19").replace("2B", "18");
    let n = head
        .bytes()
        .fold(0u64, |acc, c| acc * 10 + u64::from(c - b'0'));
    let key = 97 - n % 97;
    let given = u64::from(b[13] - b'0') * 10 + u64::from(b[14] - b'0');
    if key == given {
        Validation::Valid
    } else {
        Validation::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_mainland_and_corsica() {
        assert_eq!(validate("185057512345673"), Validation::Valid);
        assert_eq!(validate("288022A12304505"), Validation::Valid);
        assert_eq!(validate("191102B05701239"), Validation::Valid);
        assert_eq!(validate("299062B99887723"), Validation::Valid);
    }

    #[test]
    fn lowercase_corsica_letter_is_accepted() {
        assert_eq!(validate("288022a12304505"), Validation::Valid);
    }

    #[test]
    fn wrong_key_or_digit_is_invalid() {
        assert_eq!(validate("185057512345683"), Validation::Invalid);
        assert_eq!(validate("186057512345673"), Validation::Invalid);
        assert_eq!(validate("288022A12304515"), Validation::Invalid);
        assert_eq!(validate("289022A12304505"), Validation::Invalid);
    }

    #[test]
    fn bad_shape_is_not_applicable() {
        assert_eq!(validate("18505751234567"), Validation::NotApplicable);
        assert_eq!(validate("1850575123456730"), Validation::NotApplicable);
        assert_eq!(validate("1850575A2345673"), Validation::NotApplicable);
        assert_eq!(validate("288022C12304505"), Validation::NotApplicable);
        assert_eq!(validate("18505751234567X"), Validation::NotApplicable);
    }
}
