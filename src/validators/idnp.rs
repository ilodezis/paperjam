//! Moldovan IDNP (personal identification number), 13 digits `2TTTXXXYYYYYK`
//! (older numbers may start with `0`).
//!
//! The format is defined by Government Decision 333/2002 (State Population
//! Register), but no official publication of the check-digit algorithm could
//! be found while writing this. Secondary sources claim "7-3-1 weights, mod
//! 10", yet none of them cites the Decision. Following the project rule, the
//! checksum is therefore **not** checked: [`validate`] always answers
//! `NotApplicable`. See `docs/validators.md`.

use super::Validation;

pub fn is_format(digits: &str) -> bool {
    digits.len() == 13
        && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(digits.as_bytes()[0], b'0' | b'2')
}

pub fn validate(_digits: &str) -> Validation {
    Validation::NotApplicable
}
