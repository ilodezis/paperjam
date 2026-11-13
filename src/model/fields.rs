//! Typed field definitions (per document type) and field values.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::error::{Result, ServiceError};

/// Display names by language code: `{"en": "...", "ru": "..."}`.
pub type Labels = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Date,
    Money,
    Number,
    Identifier,
    List,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Date => "date",
            FieldKind::Money => "money",
            FieldKind::Number => "number",
            FieldKind::Identifier => "identifier",
            FieldKind::List => "list",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub key: String,
    pub labels: Labels,
    pub kind: FieldKind,
    /// Name in the validator registry (`idnp`, `inn`, `personal_number`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// The field names the organization that issued the document (an issuing
    /// authority, a registry office, a bank). Text fields only. When
    /// classification found no organization, the model's issuer fields link
    /// one (see `llm::write`); stored types that predate the flag read as
    /// `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub issuer: bool,
}

/// Accepts a decimal as a string or a JSON number; always writes a string.
mod lenient_decimal {
    use std::fmt;
    use std::str::FromStr;

    use rust_decimal::Decimal;
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Decimal;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal number or numeric string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Decimal, E> {
                Decimal::from_str(v.trim()).map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Decimal, E> {
                Ok(Decimal::from(v))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Decimal, E> {
                Ok(Decimal::from(v))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Decimal, E> {
                // Shortest round-trip text of the float, so 12.5 stays 12.5.
                Decimal::from_str(&v.to_string()).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

/// A stored or submitted field value, serialized as `{"kind": …, "value": …}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    Text(String),
    Date(NaiveDate),
    Money {
        #[serde(with = "lenient_decimal")]
        amount: Decimal,
        /// ISO 4217 code, e.g. `EUR`.
        currency: String,
    },
    Number(#[serde(with = "lenient_decimal")] Decimal),
    Identifier(String),
    List(Vec<String>),
}

impl FieldValue {
    pub fn kind(&self) -> FieldKind {
        match self {
            FieldValue::Text(_) => FieldKind::Text,
            FieldValue::Date(_) => FieldKind::Date,
            FieldValue::Money { .. } => FieldKind::Money,
            FieldValue::Number(_) => FieldKind::Number,
            FieldValue::Identifier(_) => FieldKind::Identifier,
            FieldValue::List(_) => FieldKind::List,
        }
    }

    /// Checks that the value fits the field's kind and basic shape.
    pub fn validate_against(&self, def: &FieldDef) -> Result<()> {
        if self.kind() != def.kind {
            return Err(ServiceError::invalid(
                "field_kind_mismatch",
                format!(
                    "field {} expects a {} value, got {}",
                    def.key,
                    def.kind.as_str(),
                    self.kind().as_str()
                ),
            )
            .with_details(serde_json::json!({
                "field": def.key,
                "expected": def.kind.as_str(),
                "got": self.kind().as_str(),
            })));
        }
        match self {
            FieldValue::Text(s) | FieldValue::Identifier(s) if s.trim().is_empty() => {
                Err(ServiceError::invalid(
                    "empty_value",
                    format!("field {} is empty; send null to clear it", def.key),
                ))
            }
            FieldValue::Money { currency, .. }
                if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) =>
            {
                Err(ServiceError::invalid(
                    "invalid_currency",
                    format!("{currency:?} is not an ISO 4217 currency code"),
                ))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use serde_json::json;

    fn def(key: &str, kind: FieldKind) -> FieldDef {
        FieldDef {
            key: key.to_string(),
            labels: Labels::from([("en".to_string(), key.to_string())]),
            kind,
            validator: None,
            required: false,
            issuer: false,
        }
    }

    #[test]
    fn field_value_kind_mismatch_is_invalid() {
        let err = FieldValue::Text("x".into())
            .validate_against(&def("expiry_date", FieldKind::Date))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Invalid);
        assert_eq!(err.code, "field_kind_mismatch");
        let details = err.details.unwrap();
        assert_eq!(details["field"], "expiry_date");
        assert_eq!(details["expected"], "date");
        assert_eq!(details["got"], "text");
    }

    #[test]
    fn matching_kinds_validate() {
        let ok = |v: FieldValue, k: FieldKind| v.validate_against(&def("f", k)).is_ok();
        assert!(ok(FieldValue::Text("x".into()), FieldKind::Text));
        assert!(ok(
            FieldValue::Date(NaiveDate::from_ymd_opt(2030, 1, 2).unwrap()),
            FieldKind::Date
        ));
        assert!(ok(
            FieldValue::Money {
                amount: Decimal::new(1250, 2),
                currency: "EUR".into()
            },
            FieldKind::Money
        ));
        assert!(ok(
            FieldValue::Number(Decimal::from(2024)),
            FieldKind::Number
        ));
        assert!(ok(
            FieldValue::Identifier("AB123".into()),
            FieldKind::Identifier
        ));
        assert!(ok(FieldValue::List(vec!["a".into()]), FieldKind::List));
    }

    #[test]
    fn money_needs_an_iso_currency_code() {
        for bad in ["eur", "EU", "EURO", "E1R", ""] {
            let v = FieldValue::Money {
                amount: Decimal::ONE,
                currency: bad.into(),
            };
            let err = v
                .validate_against(&def("total", FieldKind::Money))
                .unwrap_err();
            assert_eq!(err.code, "invalid_currency", "{bad}");
        }
    }

    #[test]
    fn blank_text_is_rejected_use_null_to_clear() {
        for v in [
            FieldValue::Text("  ".into()),
            FieldValue::Identifier(String::new()),
        ] {
            let kind = v.kind();
            let err = v.validate_against(&def("f", kind)).unwrap_err();
            assert_eq!(err.code, "empty_value");
        }
    }

    #[test]
    fn serde_uses_kind_and_value() {
        let v = FieldValue::Money {
            amount: Decimal::new(1250, 2),
            currency: "EUR".into(),
        };
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!({"kind": "money", "value": {"amount": "12.50", "currency": "EUR"}})
        );
        assert_eq!(
            serde_json::to_value(FieldValue::Date(
                NaiveDate::from_ymd_opt(2030, 1, 2).unwrap()
            ))
            .unwrap(),
            json!({"kind": "date", "value": "2030-01-02"})
        );
        let back: FieldValue =
            serde_json::from_value(json!({"kind": "list", "value": ["a", "b"]})).unwrap();
        assert_eq!(back, FieldValue::List(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn decimals_accept_strings_and_numbers() {
        for raw in [
            json!({"kind": "number", "value": "12.5"}),
            json!({"kind": "number", "value": 12.5}),
        ] {
            let v: FieldValue = serde_json::from_value(raw).unwrap();
            assert_eq!(v, FieldValue::Number(Decimal::new(125, 1)));
        }
        let v: FieldValue = serde_json::from_value(
            json!({"kind": "money", "value": {"amount": 100, "currency": "MDL"}}),
        )
        .unwrap();
        assert_eq!(
            v,
            FieldValue::Money {
                amount: Decimal::from(100),
                currency: "MDL".into()
            }
        );
        assert!(
            serde_json::from_value::<FieldValue>(json!({"kind": "number", "value": "abc"}))
                .is_err()
        );
    }

    #[test]
    fn field_def_roundtrips_without_empty_validator() {
        let d = def("number", FieldKind::Identifier);
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("validator").is_none());
        let back: FieldDef = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
        let with: FieldDef = serde_json::from_value(
            json!({"key": "personal_number", "labels": {"en": "P"}, "kind": "identifier", "validator": "personal_number"}),
        )
        .unwrap();
        assert_eq!(with.validator.as_deref(), Some("personal_number"));
        assert!(!with.required);
    }

    #[test]
    fn the_issuer_flag_defaults_to_false_and_is_written_only_when_set() {
        let old: FieldDef = serde_json::from_value(
            json!({"key": "issuing_authority", "labels": {"en": "I"}, "kind": "text"}),
        )
        .unwrap();
        assert!(!old.issuer, "a type stored before the flag existed");
        assert!(serde_json::to_value(&old).unwrap().get("issuer").is_none());

        let flagged = FieldDef {
            issuer: true,
            ..old
        };
        let json = serde_json::to_value(&flagged).unwrap();
        assert_eq!(json["issuer"], json!(true));
        assert_eq!(serde_json::from_value::<FieldDef>(json).unwrap(), flagged);
    }
}
