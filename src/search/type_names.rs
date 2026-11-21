//! The words a document type is searched by: its labels in every language
//! plus, for the built-in types, the other names people use for it (a
//! "загранпаспорт" is also a "паспорт", a "buletin" is an ID card, a
//! "Führerschein" is a driver's license).

use crate::model::fields::Labels;

/// Built-in type key, extra names. Matching is by word stem and ignores case
/// and diacritics, so inflected forms need no listing. Languages: Russian,
/// Romanian, then German, French, Spanish, Italian, Polish.
const SYNONYMS: &[(&str, &[&str])] = &[
    (
        "travel_passport",
        &[
            "паспорт",
            "passport",
            "заграничный паспорт",
            "загранпаспорт",
            "международный паспорт",
            "international passport",
            "загран",
            "Reisepass",
            "passeport",
            "pasaporte",
            "passaporto",
            "paszport",
        ],
    ),
    (
        "id_card",
        &[
            "buletin",
            "булетин",
            "carte de identitate",
            "identity card",
            "удостоверение",
            "Personalausweis",
            "Ausweis",
            "carte d'identité",
            "carte nationale d'identité",
            "pièce d'identité",
            "documento de identidad",
            "DNI",
            "carta d'identità",
            "dowód osobisty",
        ],
    ),
    (
        "residence_permit",
        &[
            "ВНЖ",
            "permis de sedere",
            "residence card",
            "Aufenthaltstitel",
            "Aufenthaltserlaubnis",
            "titre de séjour",
            "carte de séjour",
            "permiso de residencia",
            "tarjeta de residencia",
            "permesso di soggiorno",
            "carta di soggiorno",
            "karta pobytu",
        ],
    ),
    (
        "driver_license",
        &[
            "водительские права",
            "права",
            "permis de conducere",
            "driving licence",
            "driver license",
            "Führerschein",
            "Fahrerlaubnis",
            "permis de conduire",
            "permiso de conducir",
            "carnet de conducir",
            "licencia de conducir",
            "patente",
            "patente di guida",
            "prawo jazdy",
        ],
    ),
    (
        "birth_certificate",
        &[
            "метрика",
            "certificat de nastere",
            "свидетельство о рождении",
            "Geburtsurkunde",
            "acte de naissance",
            "certificado de nacimiento",
            "partida de nacimiento",
            "certificato di nascita",
            "atto di nascita",
            "akt urodzenia",
        ],
    ),
    (
        "marriage_certificate",
        &[
            "certificat de casatorie",
            "свидетельство о браке",
            "Heiratsurkunde",
            "acte de mariage",
            "certificat de mariage",
            "certificado de matrimonio",
            "certificato di matrimonio",
            "atto di matrimonio",
            "akt małżeństwa",
        ],
    ),
    (
        "insurance_policy",
        &[
            "полис",
            "страховка",
            "asigurare",
            "Versicherungspolice",
            "Versicherungsschein",
            "police d'assurance",
            "assurance",
            "póliza de seguro",
            "seguro",
            "polizza assicurativa",
            "assicurazione",
            "polisa ubezpieczeniowa",
            "ubezpieczenie",
        ],
    ),
    (
        "bank_statement",
        &[
            "выписка",
            "extras de cont",
            "Kontoauszug",
            "relevé bancaire",
            "relevé de compte",
            "extracto bancario",
            "extracto de cuenta",
            "estratto conto",
            "wyciąg bankowy",
            "wyciąg z konta",
        ],
    ),
    (
        "invoice",
        &[
            "счёт",
            "инвойс",
            "factura",
            "Rechnung",
            "facture",
            "fattura",
            "faktura",
        ],
    ),
    (
        "receipt",
        &[
            "квитанция",
            "bon fiscal",
            "Quittung",
            "Kassenbon",
            "Kassenzettel",
            "reçu",
            "ticket de caisse",
            "recibo",
            "ricevuta",
            "scontrino",
            "paragon",
        ],
    ),
    (
        "warranty",
        &[
            "гарантия",
            "garantie",
            "Garantieschein",
            "garantía",
            "garanzia",
            "gwarancja",
        ],
    ),
    (
        "tax_document",
        &[
            "налоги",
            "налоговая",
            "impozit",
            "Steuerbescheid",
            "Steuererklärung",
            "avis d'imposition",
            "déclaration d'impôts",
            "declaración de la renta",
            "impuestos",
            "dichiarazione dei redditi",
            "imposte",
            "deklaracja podatkowa",
            "podatek",
        ],
    ),
    (
        "medical_document",
        &[
            "медицинская справка",
            "анализы",
            "Arztbrief",
            "Befund",
            "ärztliches Attest",
            "certificat médical",
            "document médical",
            "ordonnance",
            "certificado médico",
            "informe médico",
            "certificato medico",
            "documento medico",
            "referto",
            "zaświadczenie lekarskie",
            "dokument medyczny",
        ],
    ),
    (
        "diploma",
        &[
            "диплом",
            "аттестат",
            "diplomă",
            "Diplom",
            "Abschlusszeugnis",
            "diplôme",
            "diploma",
            "título universitario",
            "dyplom",
        ],
    ),
    (
        "contract",
        &[
            "договор",
            "контракт",
            "Vertrag",
            "contrat",
            "contrato",
            "contratto",
            "umowa",
            "kontrakt",
        ],
    ),
    (
        "rental_agreement",
        &[
            "аренда",
            "договор аренды",
            "договор найма",
            "contract de închiriere",
            "chirie",
            "Mietvertrag",
            "contrat de location",
            "bail",
            "contrato de alquiler",
            "contrato de arrendamiento",
            "contratto di locazione",
            "contratto d'affitto",
            "umowa najmu",
        ],
    ),
    (
        "power_of_attorney",
        &[
            "доверенность",
            "нотариальная доверенность",
            "procură",
            "procura",
            "Vollmacht",
            "procuration",
            "poder notarial",
            "pełnomocnictwo",
        ],
    ),
    (
        "payment_confirmation",
        &[
            "платёжка",
            "платёжное поручение",
            "подтверждение оплаты",
            "confirmare de plată",
            "Zahlungsbestätigung",
            "Überweisungsbeleg",
            "confirmation de paiement",
            "justificatif de paiement",
            "confirmación de pago",
            "comprobante de pago",
            "conferma di pagamento",
            "ricevuta di pagamento",
            "potwierdzenie płatności",
            "potwierdzenie przelewu",
        ],
    ),
    (
        "letter",
        &[
            "письмо",
            "scrisoare",
            "Brief",
            "Schreiben",
            "lettre",
            "courrier",
            "carta",
            "lettera",
            "pismo",
        ],
    ),
];

/// Labels in every language, the key spelled out and the built-in synonyms.
pub fn names(key: &str, labels: &Labels) -> Vec<String> {
    let mut names: Vec<String> = labels.values().cloned().collect();
    names.push(key.replace('_', " "));
    if let Some((_, extra)) = SYNONYMS.iter().find(|(k, _)| *k == key) {
        names.extend(extra.iter().map(|s| (*s).to_string()));
    }
    names.retain(|n| !n.trim().is_empty());
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(en: &str, ru: &str) -> Labels {
        Labels::from([("en".into(), en.into()), ("ru".into(), ru.into())])
    }

    #[test]
    fn every_label_and_the_key_are_names() {
        let n = names("pet_passport", &labels("Pet passport", "Паспорт питомца"));
        assert_eq!(n, vec!["Pet passport", "Паспорт питомца", "pet passport"]);
    }

    #[test]
    fn a_travel_passport_is_also_a_passport() {
        let n = names(
            "travel_passport",
            &labels("Travel passport", "Загранпаспорт"),
        );
        for expected in [
            "Загранпаспорт",
            "паспорт",
            "passport",
            "заграничный паспорт",
        ] {
            assert!(n.iter().any(|x| x == expected), "{expected}");
        }
    }

    /// Per language, the everyday word on or around the document for every
    /// built-in type that has one (German, French, Spanish, Italian, Polish).
    #[test]
    fn european_languages_have_their_everyday_names() {
        let cases: [(&str, [&str; 5]); 19] = [
            (
                "travel_passport",
                [
                    "Reisepass",
                    "passeport",
                    "pasaporte",
                    "passaporto",
                    "paszport",
                ],
            ),
            (
                "id_card",
                [
                    "Personalausweis",
                    "carte d'identité",
                    "documento de identidad",
                    "carta d'identità",
                    "dowód osobisty",
                ],
            ),
            (
                "residence_permit",
                [
                    "Aufenthaltstitel",
                    "titre de séjour",
                    "permiso de residencia",
                    "permesso di soggiorno",
                    "karta pobytu",
                ],
            ),
            (
                "driver_license",
                [
                    "Führerschein",
                    "permis de conduire",
                    "permiso de conducir",
                    "patente",
                    "prawo jazdy",
                ],
            ),
            (
                "birth_certificate",
                [
                    "Geburtsurkunde",
                    "acte de naissance",
                    "certificado de nacimiento",
                    "certificato di nascita",
                    "akt urodzenia",
                ],
            ),
            (
                "marriage_certificate",
                [
                    "Heiratsurkunde",
                    "acte de mariage",
                    "certificado de matrimonio",
                    "certificato di matrimonio",
                    "akt małżeństwa",
                ],
            ),
            (
                "insurance_policy",
                [
                    "Versicherungspolice",
                    "police d'assurance",
                    "póliza de seguro",
                    "polizza assicurativa",
                    "polisa ubezpieczeniowa",
                ],
            ),
            (
                "bank_statement",
                [
                    "Kontoauszug",
                    "relevé bancaire",
                    "extracto bancario",
                    "estratto conto",
                    "wyciąg bankowy",
                ],
            ),
            (
                "invoice",
                ["Rechnung", "facture", "factura", "fattura", "faktura"],
            ),
            (
                "receipt",
                [
                    "Quittung",
                    "ticket de caisse",
                    "recibo",
                    "scontrino",
                    "paragon",
                ],
            ),
            (
                "warranty",
                [
                    "Garantieschein",
                    "garantie",
                    "garantía",
                    "garanzia",
                    "gwarancja",
                ],
            ),
            (
                "tax_document",
                [
                    "Steuerbescheid",
                    "avis d'imposition",
                    "declaración de la renta",
                    "dichiarazione dei redditi",
                    "deklaracja podatkowa",
                ],
            ),
            (
                "medical_document",
                [
                    "Arztbrief",
                    "certificat médical",
                    "informe médico",
                    "certificato medico",
                    "zaświadczenie lekarskie",
                ],
            ),
            (
                "diploma",
                [
                    "Diplom",
                    "diplôme",
                    "título universitario",
                    "diploma",
                    "dyplom",
                ],
            ),
            (
                "contract",
                ["Vertrag", "contrat", "contrato", "contratto", "umowa"],
            ),
            (
                "rental_agreement",
                [
                    "Mietvertrag",
                    "contrat de location",
                    "contrato de alquiler",
                    "contratto di locazione",
                    "umowa najmu",
                ],
            ),
            (
                "power_of_attorney",
                [
                    "Vollmacht",
                    "procuration",
                    "poder notarial",
                    "procura",
                    "pełnomocnictwo",
                ],
            ),
            (
                "payment_confirmation",
                [
                    "Zahlungsbestätigung",
                    "confirmation de paiement",
                    "comprobante de pago",
                    "ricevuta di pagamento",
                    "potwierdzenie płatności",
                ],
            ),
            (
                "letter",
                ["Schreiben", "lettre", "carta", "lettera", "pismo"],
            ),
        ];
        for (key, expected) in cases {
            let n = names(key, &labels("x", "y"));
            for name in expected {
                assert!(n.iter().any(|x| x == name), "{key}: {name}");
            }
        }
    }

    #[test]
    fn the_internal_passport_is_not_a_travel_passport() {
        let n = names("passport", &labels("Passport", "Паспорт"));
        assert!(!n.iter().any(|x| x.contains("загран")), "{n:?}");
    }
}
