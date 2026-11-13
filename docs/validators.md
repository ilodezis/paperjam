# Identifier validators

Some identifiers carry a checksum, and paperjam uses it. When a value fails its check, the document gets the `checksum_failed` attention flag, so a misread digit does not sit unnoticed in the archive. This page is for anyone who wonders why a document was flagged, which identifiers are covered, or how to add one. The algorithms are in `src/validators/` and `src/mrz.rs`, and each file names the source it was implemented from.

## What a check does

A field definition can name a validator, and only identifier fields can. Every time a document is processed or edited, paperjam runs the validators over its field values. Spaces, hyphens and non-breaking spaces are ignored first. A check has three possible answers:

- **valid**: the value has the right format and the checksum matches;
- **invalid**: the format is right and the checksum is wrong, which raises `checksum_failed`;
- **not applicable**: no algorithm fits the value, or the checksum of that format is not publicly documented. The value is stored, and nothing is flagged.

The flag is derived, not remembered. It disappears as soon as the value is corrected. It is also never raised for a value you typed yourself: manual edits are trusted, because you decide what the document says. If a document really does carry a number that fails its checksum (issuers make typos too), type the value in and the flag clears.

## What is covered

| Name | Identifier | Checked |
|---|---|---|
| `mrz` | The machine-readable zone of an ICAO 9303 document (TD1, TD2, TD3) and of the Russian internal passport | Every check digit: document number, date of birth, expiry date, the optional data where the format has a check digit for it, and the composite digit |
| `iban` | International bank account number (ISO 13616), 15 to 34 characters | The ISO 7064 MOD 97-10 check. The length per country is not checked |
| `nir` | French social security number, 13 characters (Corsican departments 2A and 2B allowed) and a 2-digit key | The key, 97 minus the number modulo 97 |
| `steuer_id` | German tax identification number, 11 digits | The ISO 7064 MOD 11,10 check digit and a non-zero first digit |
| `dni_nie` | Spanish DNI (8 digits and a letter) and NIE (X, Y or Z, 7 digits and a letter) | The control letter |
| `pesel` | Polish personal identification number, 11 digits | The check digit. The embedded date is not checked |
| `bsn` | Dutch citizen service number, 9 digits | The 11-proef |
| `nhs` | UK NHS number, 10 digits | The modulus 11 check digit. A number whose check works out to 10 is invalid |
| `cpf` | Brazilian taxpayer number, 11 digits | Both check digits; eleven equal digits are invalid |
| `aadhaar` | Indian Aadhaar number, 12 digits starting with 2 to 9 | The Verhoeff check digit |
| `inn` | Russian taxpayer number, 10 digits for organizations and 12 for individuals | One check digit (10 digits) or two (12 digits) |
| `snils` | Russian pension insurance number, 11 digits | The two-digit check value. Numbers up to 001-001-998 carry none and get no verdict |
| `cnp` | Romanian personal numeric code, 13 digits starting with 1 to 8 | The control digit, plus ranges: month 01 to 12, day 01 to 31, county 01 to 52. A value outside a range is invalid |
| `idnp` | Moldovan personal identification number, 13 digits | **No.** See below |
| `personal_number` | One of the national numbers above, picked by format | See below |

The built-in types passport, internal passport, ID card, residence permit and driver's license all have a `personal_number` field that uses the `personal_number` validator, and the bank statement's account number uses `iban`. Other national numbers are chosen by name on your own types (see below), because their plain digit shapes overlap.

The machine-readable zone is checked while the page is read, not as a field. A zone whose check digits all pass fills the name, number, dates and nationality fields. A zone that fails any of them fills nothing, and the document is flagged. A damaged zone therefore cannot put plausible but wrong values into the fields.

## `personal_number` format detection

The validator `personal_number` picks an algorithm from the shape of the value, with spaces and dashes ignored:

- 10 or 12 digits: INN;
- 15 characters, 13 digits or Corsican `2A`/`2B` in the department place, then 2 digits: NIR;
- 8 digits and a letter, or X, Y or Z, 7 digits and a letter: DNI or NIE;
- 13 digits starting with 1 or 3 to 8: CNP;
- 13 digits starting with 2: both a CNP and an IDNP fit. Only a passing CNP check is reported as valid. A failing one is not applicable, because the number may be a Moldovan IDNP whose checksum is unknown;
- 13 digits starting with 0: IDNP, not checked;
- anything else: not applicable. In particular a Polish, German, Brazilian or Dutch number is not picked up here: 9 to 12 plain digits fit several countries at once, so give the field the validator by name instead.

## Moldovan IDNP

The IDNP has a check digit, but no official publication of its algorithm could be found. Government Decision 333/2002 defines the format only. Secondary sources claim "weights 7-3-1, modulo 10", and none of them cites a primary source. An unverified algorithm would flag good numbers or bless bad ones, so the validator answers not applicable and the value is stored without a checksum verdict.

If you can point to an official document that states the algorithm, please open an issue with the reference. To implement it, put it in `src/validators/idnp.rs`, add test vectors from that document, and remove this section.

## Using a validator on your own type

The validators are available to custom document types. Give an identifier field the `validator` property when you create or update the type, through `POST /api/v1/types` or `manage_taxonomy` with `create_type` or `update_type`:

```bash
curl -X POST https://paperjam.example.org/api/v1/types \
  -H "Authorization: Bearer pj_…" \
  -H "Content-Type: application/json" \
  -d '{
    "key": "tax_notice",
    "labels": {"en": "Tax notice"},
    "fields": [
      {"key": "taxpayer_id", "labels": {"en": "Taxpayer ID"}, "kind": "identifier", "validator": "inn"}
    ]
  }'
```

A validator on a field that is not an identifier, or a name not in the table above, is refused with `invalid_field`.

## Adding a validator

A new algorithm is a file in `src/validators/` that starts with a comment naming its authoritative source, a branch in `validate` in `src/validators/mod.rs`, an entry in the list of known names in `src/service/taxonomy.rs`, and unit tests with vectors that come from the source or are made up. Never use a real person's number as a test vector. If the algorithm cannot be confirmed from a primary source, follow the IDNP precedent and answer not applicable.
