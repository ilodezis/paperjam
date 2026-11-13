//! Identifier validators: a small registry keyed by the `validator` name on a
//! field definition. Each algorithm lives in its own file, headed by the
//! authoritative source it was implemented from.

pub mod aadhaar;
pub mod bsn;
pub mod cnp;
pub mod cpf;
pub mod dni_nie;
pub mod iban;
pub mod idnp;
pub mod inn;
pub mod nhs;
pub mod nir;
pub mod pesel;
pub mod snils;
pub mod steuer_id;

use crate::mrz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validation {
    /// The value matches the format and its checksum is correct.
    Valid,
    /// The value matches the format but its checksum is wrong.
    Invalid,
    /// No algorithm applies to this value (unknown validator, unknown format,
    /// or a format whose checksum is not publicly documented).
    NotApplicable,
}

/// Validate `value` with the validator registered under `name`
/// (see `docs/validators.md` for the list; `personal_number` picks one by the
/// shape of the value).
pub fn validate(name: &str, value: &str) -> Validation {
    if name == "mrz" {
        return match mrz::find(value) {
            Some(zone) if zone.checks_ok => Validation::Valid,
            Some(_) => Validation::Invalid,
            None => Validation::NotApplicable,
        };
    }
    let digits: String = value
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '\u{a0}'))
        .collect();
    match name {
        "idnp" => idnp::validate(&digits),
        "cnp" => cnp::validate(&digits),
        "inn" => inn::validate(&digits),
        "snils" => snils::validate(&digits),
        "iban" => iban::validate(&digits),
        "pesel" => pesel::validate(&digits),
        "bsn" => bsn::validate(&digits),
        "nir" => nir::validate(&digits),
        "steuer_id" => steuer_id::validate(&digits),
        "dni_nie" => dni_nie::validate(&digits),
        "cpf" => cpf::validate(&digits),
        "aadhaar" => aadhaar::validate(&digits),
        "nhs" => nhs::validate(&digits),
        "personal_number" => personal_number(&digits),
        _ => Validation::NotApplicable,
    }
}

/// A national personal number of unknown country: pick the algorithm by
/// format.
///
/// 13 digits starting with `2` fit both a CNP and a Moldovan IDNP. The IDNP
/// checksum is unknown, so such a number is only ever reported `Valid` (the
/// CNP check passes) and never `Invalid`.
///
/// Only formats that no other format shares are detected: the Russian INN
/// (10 or 12 digits), the French NIR (15 characters) and the Spanish DNI/NIE
/// (letters in fixed places). The other national numbers have plain 9 to 12
/// digit shapes that collide with these, so they are chosen by name.
fn personal_number(digits: &str) -> Validation {
    if inn::is_format(digits) {
        return inn::validate(digits);
    }
    if nir::is_format(digits) {
        return nir::validate(digits);
    }
    if dni_nie::is_format(digits) {
        return dni_nie::validate(digits);
    }
    let cnp_fits = cnp::is_format(digits);
    let idnp_fits = idnp::is_format(digits);
    match (cnp_fits, idnp_fits) {
        (true, true) => match cnp::validate(digits) {
            Validation::Valid => Validation::Valid,
            _ => Validation::NotApplicable,
        },
        (true, false) => cnp::validate(digits),
        (false, true) => idnp::validate(digits),
        (false, false) => Validation::NotApplicable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_number_recognizes_the_french_and_spanish_shapes() {
        assert_eq!(
            validate("personal_number", "1 85 05 75 123 456 73"),
            Validation::Valid
        );
        assert_eq!(
            validate("personal_number", "1 85 05 75 123 456 74"),
            Validation::Invalid
        );
        assert_eq!(validate("personal_number", "12345678-Z"), Validation::Valid);
        assert_eq!(
            validate("personal_number", "12345678-A"),
            Validation::Invalid
        );
    }

    #[test]
    fn nine_to_twelve_plain_digits_are_chosen_by_name_not_guessed() {
        // A valid BSN is just nine digits; `personal_number` stays silent.
        assert_eq!(validate("bsn", "123456782"), Validation::Valid);
        assert_eq!(
            validate("personal_number", "123456782"),
            Validation::NotApplicable
        );
    }

    #[test]
    fn inn_10_valid() {
        assert_eq!(validate("inn", "7707083893"), Validation::Valid);
        assert_eq!(validate("inn", "7812345675"), Validation::Valid);
    }

    #[test]
    fn inn_10_invalid() {
        assert_eq!(validate("inn", "7707083894"), Validation::Invalid);
    }

    #[test]
    fn inn_12_valid() {
        assert_eq!(validate("inn", "500100732259"), Validation::Valid);
    }

    #[test]
    fn inn_12_invalid() {
        assert_eq!(validate("inn", "500100732250"), Validation::Invalid);
    }

    #[test]
    fn inn_wrong_length_is_not_applicable() {
        assert_eq!(validate("inn", "12345"), Validation::NotApplicable);
        assert_eq!(validate("inn", "77070838a3"), Validation::NotApplicable);
    }

    #[test]
    fn snils_valid() {
        assert_eq!(validate("snils", "11223344595"), Validation::Valid);
        assert_eq!(validate("snils", "112-233-445 95"), Validation::Valid);
    }

    #[test]
    fn snils_invalid() {
        assert_eq!(validate("snils", "11223344596"), Validation::Invalid);
    }

    #[test]
    fn cnp_valid() {
        assert_eq!(validate("cnp", "1800101221144"), Validation::Valid);
        assert_eq!(validate("cnp", "2900505123454"), Validation::Valid);
    }

    #[test]
    fn cnp_invalid() {
        assert_eq!(validate("cnp", "1800101221145"), Validation::Invalid);
        // month 13 does not exist
        assert_eq!(validate("cnp", "1801301221140"), Validation::Invalid);
    }

    #[test]
    fn idnp_checksum_is_not_checked() {
        // The check-digit algorithm is not publicly documented, see idnp.rs.
        assert_eq!(validate("idnp", "2002000000123"), Validation::NotApplicable);
        assert_eq!(validate("idnp", "20020"), Validation::NotApplicable);
    }

    #[test]
    fn personal_number_picks_validator_by_format() {
        assert_eq!(validate("personal_number", "7707083893"), Validation::Valid);
        assert_eq!(
            validate("personal_number", "7707083894"),
            Validation::Invalid
        );
        assert_eq!(
            validate("personal_number", "500100732259"),
            Validation::Valid
        );
        assert_eq!(
            validate("personal_number", "1800101221144"),
            Validation::Valid
        );
        assert_eq!(
            validate("personal_number", "1800101221145"),
            Validation::Invalid
        );
    }

    #[test]
    fn personal_number_ambiguous_leading_2_is_never_flagged() {
        // 13 digits starting with 2 may be a CNP or a Moldovan IDNP, and the
        // IDNP checksum is unknown, so a failing CNP check proves nothing.
        assert_eq!(
            validate("personal_number", "2900505123454"),
            Validation::Valid
        );
        assert_eq!(
            validate("personal_number", "2002000000123"),
            Validation::NotApplicable
        );
    }

    #[test]
    fn personal_number_unknown_format_is_not_applicable() {
        assert_eq!(
            validate("personal_number", "AB123"),
            Validation::NotApplicable
        );
        assert_eq!(validate("personal_number", ""), Validation::NotApplicable);
    }

    #[test]
    fn personal_number_ignores_spaces_and_dashes() {
        assert_eq!(
            validate("personal_number", "7707 083 893"),
            Validation::Valid
        );
    }

    #[test]
    fn unknown_validator_is_not_applicable() {
        assert_eq!(validate("nope", "123"), Validation::NotApplicable);
    }

    #[test]
    fn mrz_validator_checks_check_digits() {
        let good = "P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<\nL898902C36UTO7408122F1204159ZE184226B<<<<<10";
        let bad = good.replace("L898902C36", "L898902C37");
        assert_eq!(validate("mrz", good), Validation::Valid);
        assert_eq!(validate("mrz", &bad), Validation::Invalid);
        assert_eq!(validate("mrz", "not an mrz"), Validation::NotApplicable);
    }
}
