//! The JSON schemas the LLM steps ask for and the structs their answers are
//! read into. Every schema is closed (`additionalProperties: false`) and every
//! property is required, as strict structured output demands; absent values
//! are `null`.
//!
//! Candidate lists (types, correspondents, people, tags) become `enum`s, so a
//! model can only pick what exists: an invented person id fails validation
//! (`llm_schema_mismatch`) instead of reaching the database. A candidate list
//! that is empty drops its property, because an empty `enum` is not a valid
//! schema.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::LazyLock;

use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::normalize;
use crate::model::document::Role;
use crate::model::fields::{FieldDef, FieldKind, FieldValue};

pub const TRANSCRIPTION: &str = "transcription";
pub const CLASSIFICATION: &str = "classification";
pub const FIELD_EXTRACTION: &str = "field_extraction";

fn closed_object(properties: Map<String, Value>) -> Value {
    let required: Vec<&String> = properties.keys().collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

// --- step A: transcription -----------------------------------------------

static TRANSCRIPTION_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    let page = closed_object(Map::from_iter([
        ("page".to_string(), json!({"type": "integer"})),
        ("markdown".to_string(), json!({"type": "string"})),
        ("illegible".to_string(), json!({"type": "boolean"})),
        ("handwritten".to_string(), json!({"type": "boolean"})),
    ]));
    closed_object(Map::from_iter([(
        "pages".to_string(),
        json!({"type": "array", "items": page}),
    )]))
});

pub fn transcription_schema() -> Value {
    TRANSCRIPTION_SCHEMA.clone()
}

pub fn transcription_schema_ref() -> &'static Value {
    &TRANSCRIPTION_SCHEMA
}

#[derive(Debug, Deserialize)]
pub struct Transcription {
    pub pages: Vec<TranscribedPage>,
}

#[derive(Debug, Deserialize)]
pub struct TranscribedPage {
    pub page: u32,
    pub markdown: String,
    /// The page has words the model could not read; the document is flagged
    /// so the reader compares the scan with the text's [illegible] markers.
    #[allow(dead_code)]
    pub illegible: bool,
    /// Values on the page are written by hand (a bare signature does not
    /// count): such a page gets a second reading by a stronger model.
    #[serde(default)]
    pub handwritten: bool,
}

// --- step B: classification ----------------------------------------------

/// What the model may choose from, as strings (ids are UUIDs).
#[derive(Debug, Default)]
pub struct ClassifyCandidates {
    pub type_keys: Vec<String>,
    pub correspondent_ids: Vec<String>,
    pub person_ids: Vec<String>,
    pub tag_names: Vec<String>,
    /// Whether the model may propose tags that do not exist yet
    /// (`PAPERJAM_LLM_CREATE_TAGS`).
    pub allow_new_tags: bool,
}

fn string_enum(values: &[String]) -> Value {
    json!({"type": "string", "enum": values})
}

pub fn classification_schema(c: &ClassifyCandidates) -> Value {
    let mut properties = Map::new();
    if !c.type_keys.is_empty() {
        properties.insert("type_key".to_string(), string_enum(&c.type_keys));
    }
    properties.insert("title".to_string(), json!({"type": "string"}));
    properties.insert(
        "document_date".to_string(),
        json!({"type": ["string", "null"]}),
    );
    let correspondent_id = if c.correspondent_ids.is_empty() {
        json!({"type": "null"})
    } else {
        // `anyOf`, not `type: [string, null]` plus `enum`: Anthropic's
        // structured outputs refuse the latter.
        json!({"anyOf": [string_enum(&c.correspondent_ids), {"type": "null"}]})
    };
    let org_item = closed_object(Map::from_iter([
        ("id".to_string(), correspondent_id),
        ("new_name".to_string(), json!({"type": ["string", "null"]})),
        (
            "role".to_string(),
            string_enum(&[
                "issued_by".into(),
                "received_from".into(),
                "party".into(),
                "employer".into(),
                "unspecified".into(),
            ]),
        ),
        ("confident".to_string(), json!({"type": "boolean"})),
    ]));
    properties.insert(
        "organizations".to_string(),
        json!({
            "type": "array",
            "maxItems": 4,
            "items": org_item,
        }),
    );
    if !c.person_ids.is_empty() {
        properties.insert(
            "people".to_string(),
            json!({"type": "array", "items": string_enum(&c.person_ids)}),
        );
    }
    if !c.tag_names.is_empty() {
        properties.insert(
            "tags".to_string(),
            json!({"type": "array", "items": string_enum(&c.tag_names)}),
        );
    }
    if c.allow_new_tags {
        properties.insert(
            "new_tags".to_string(),
            json!({"type": "array", "maxItems": 2, "items": {"type": "string"}}),
        );
    }
    properties.insert("summary".to_string(), json!({"type": "string"}));
    properties.insert("language".to_string(), json!({"type": "string"}));
    closed_object(properties)
}

#[derive(Debug, Deserialize)]
pub struct Classification {
    #[serde(default)]
    pub type_key: Option<String>,
    pub title: String,
    #[serde(default)]
    pub document_date: Option<String>,
    #[serde(default)]
    pub organizations: Vec<OrganizationAnswer>,
    #[serde(default)]
    pub people: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub new_tags: Vec<String>,
    pub summary: String,
    pub language: String,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationAnswer {
    pub id: Option<String>,
    pub new_name: Option<String>,
    #[serde(default)]
    pub role: Role,
    /// Whether the model could read the organization and did not have to infer it.
    /// Answers without the flag (older ones) count as sure.
    #[serde(default = "sure")]
    pub confident: bool,
}

fn sure() -> bool {
    true
}

// --- step C: fields --------------------------------------------------------

/// A field the document does not state is an empty string, an empty list or
/// a money value with empty parts, and an unknown page is `0`: Anthropic's
/// structured outputs allow only 16 nullable (union-typed) parameters per
/// schema, which a document type with a dozen fields would exceed.
fn value_schema(kind: FieldKind) -> Value {
    match kind {
        FieldKind::Text | FieldKind::Identifier | FieldKind::Date | FieldKind::Number => {
            json!({"type": "string"})
        }
        FieldKind::Money => json!({
            "type": "object",
            "properties": {
                "amount": {"type": "string"},
                "currency": {"type": "string"},
            },
            "required": ["amount", "currency"],
            "additionalProperties": false,
        }),
        FieldKind::List => json!({"type": "array", "items": {"type": "string"}}),
    }
}

pub fn fields_schema(defs: &[FieldDef]) -> Value {
    let fields = defs
        .iter()
        .map(|def| {
            let mut entry = Map::from_iter([
                ("value".to_string(), value_schema(def.kind)),
                ("page".to_string(), json!({"type": "integer"})),
            ]);
            if def.issuer {
                // Whether the value is an organization or only a place decides
                // whether it becomes a correspondent (`write::link_issuer`).
                entry.insert("names_organization".to_string(), json!({"type": "boolean"}));
            }
            (def.key.clone(), closed_object(entry))
        })
        .collect();
    let fact = closed_object(Map::from_iter([
        ("key".to_string(), json!({"type": "string"})),
        ("label".to_string(), json!({"type": "string"})),
        ("value".to_string(), json!({"type": "string"})),
        ("page".to_string(), json!({"type": "integer"})),
    ]));
    closed_object(Map::from_iter([
        ("fields".to_string(), closed_object(fields)),
        ("facts".to_string(), json!({"type": "array", "items": fact})),
    ]))
}

#[derive(Debug, Deserialize)]
pub struct FieldsAnswer {
    pub fields: BTreeMap<String, FieldAnswer>,
    pub facts: Vec<FactAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct FieldAnswer {
    pub value: Value,
    pub page: Option<i64>,
    /// Asked for issuer fields only: the value is the name of an organization
    /// or office (even one word, a company name), not just a place or a code.
    /// Absent reads as `false`.
    #[serde(default)]
    pub names_organization: bool,
}

#[derive(Debug, Deserialize)]
pub struct FactAnswer {
    pub key: String,
    /// What the fact is, in the uploader's language, for the UI.
    #[serde(default)]
    pub label: String,
    pub value: String,
    pub page: Option<i64>,
}

/// The field value the model gave, shaped for the field's kind; `None` for
/// `null` and for anything that does not fit (a malformed single value never
/// fails the step).
pub fn field_value(def: &FieldDef, raw: &Value) -> Option<FieldValue> {
    fn text(v: &Value) -> Option<&str> {
        v.as_str().map(str::trim).filter(|s| !s.is_empty())
    }
    // Printed labels are stripped and enumerable values brought to one form
    // (`normalize`); a value that was only a label is no value, and neither
    // is one that only says it cannot be read ("[illegible]",
    // "неразборчиво"), which the transcription marks instead of guessing.
    let cleaned = |raw: &Value| -> Option<String> {
        let value = normalize::field_text(&def.key, &def.labels, text(raw)?);
        (!value.is_empty() && !normalize::is_placeholder(&value)).then_some(value)
    };
    let value = match def.kind {
        FieldKind::Text => FieldValue::Text(cleaned(raw)?),
        FieldKind::Identifier => FieldValue::Identifier(cleaned(raw)?),
        FieldKind::Date => FieldValue::Date(normalize::date(text(raw)?)?),
        FieldKind::Number => FieldValue::Number(Decimal::from_str(text(raw)?).ok()?),
        FieldKind::Money => FieldValue::Money {
            amount: Decimal::from_str(text(&raw["amount"])?).ok()?,
            currency: text(&raw["currency"])?.to_ascii_uppercase(),
        },
        FieldKind::List => {
            let items: Vec<String> = raw
                .as_array()?
                .iter()
                .filter_map(|item| text(item).map(str::to_string))
                .collect();
            if items.is_empty() {
                return None;
            }
            FieldValue::List(items)
        }
    };
    value.validate_against(def).ok()?;
    Some(value)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::model::fields::Labels;

    fn def(key: &str, kind: FieldKind) -> FieldDef {
        FieldDef {
            key: key.to_string(),
            labels: Labels::new(),
            kind,
            validator: None,
            required: false,
            issuer: false,
        }
    }

    fn compiles(schema: &Value) -> jsonschema::Validator {
        jsonschema::validator_for(schema).expect("the schema compiles")
    }

    #[test]
    fn classification_schema_limits_choices_to_the_candidates() {
        let schema = classification_schema(&ClassifyCandidates {
            type_keys: vec!["passport".into(), "other".into()],
            correspondent_ids: vec!["c1".into()],
            person_ids: vec!["p1".into()],
            tag_names: vec!["warranty".into()],
            allow_new_tags: false,
        });
        let validator = compiles(&schema);
        let answer = |overrides: Value| {
            let mut base = json!({
                "type_key": "passport",
                "title": "t",
                "document_date": null,
                "organizations": [{"id": null, "new_name": "Acme", "role": "issued_by", "confident": true}],
                "people": ["p1"],
                "tags": ["warranty"],
                "summary": "s",
                "language": "en",
            });
            for (k, v) in overrides.as_object().unwrap() {
                base[k] = v.clone();
            }
            base
        };
        assert!(validator.is_valid(&answer(json!({}))));
        assert!(!validator.is_valid(&answer(json!({"people": ["invented"]}))));
        assert!(!validator.is_valid(&answer(json!({"type_key": "nonsense"}))));
        assert!(!validator.is_valid(&answer(json!({"tags": ["new"]}))));
        assert!(!validator.is_valid(&answer(
            json!({"organizations": [{"id": "c9", "new_name": null, "role": "issued_by", "confident": true}]})
        )));
        assert!(validator.is_valid(&answer(
            json!({"organizations": [{"id": "c1", "new_name": null, "role": "issued_by", "confident": true}]})
        )));
        assert!(!validator.is_valid(&answer(
            json!({"organizations": [{"id": "c1", "new_name": null, "role": "invalid_role", "confident": true}]})
        )));
        assert!(!validator.is_valid(&answer(json!({"organizations": [
            {"id": "c1", "new_name": null, "role": "issued_by", "confident": true},
            {"id": "c1", "new_name": null, "role": "party", "confident": true},
            {"id": "c1", "new_name": null, "role": "employer", "confident": true},
            {"id": "c1", "new_name": null, "role": "received_from", "confident": true},
            {"id": "c1", "new_name": null, "role": "unspecified", "confident": true},
        ]}))));
        assert!(schema["properties"].get("new_tags").is_none());
    }

    /// Anthropic's structured-output validator rejects a node that lists
    /// `enum` values next to a `type` array (`["string", "null"]`): the union
    /// has to be spelled with `anyOf`.
    fn enum_beside_type_array(schema: &Value) -> bool {
        match schema {
            Value::Object(map) => {
                (map.get("enum").is_some() && map.get("type").is_some_and(Value::is_array))
                    || map.values().any(enum_beside_type_array)
            }
            Value::Array(items) => items.iter().any(enum_beside_type_array),
            _ => false,
        }
    }

    #[test]
    fn choices_never_sit_beside_a_type_union() {
        let schema = classification_schema(&ClassifyCandidates {
            type_keys: vec!["passport".into()],
            correspondent_ids: vec!["c1".into()],
            person_ids: vec!["p1".into()],
            tag_names: vec!["warranty".into()],
            allow_new_tags: true,
        });
        assert!(!enum_beside_type_array(&schema), "{schema}");
        // The helper does see the bad shape.
        assert!(enum_beside_type_array(
            &json!({"a": {"type": ["string", "null"], "enum": ["x", null]}})
        ));
    }

    #[test]
    fn empty_candidate_lists_drop_their_properties() {
        let schema = classification_schema(&ClassifyCandidates::default());
        compiles(&schema);
        for dropped in ["type_key", "people", "tags", "new_tags"] {
            assert!(schema["properties"].get(dropped).is_none(), "{dropped}");
        }
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().all(|r| r != "people"));
        // With no correspondents the id can only be null.
        assert!(compiles(&schema).is_valid(&json!({
            "title": "t", "document_date": null,
            "organizations": [{"id": null, "new_name": null, "role": "unspecified", "confident": false}],
            "summary": "s", "language": "en",
        })));
    }

    #[test]
    fn new_tags_are_offered_only_when_allowed() {
        let schema = classification_schema(&ClassifyCandidates {
            allow_new_tags: true,
            ..Default::default()
        });
        assert!(schema["properties"].get("new_tags").is_some());
        // A document adds one or two topics, so the model cannot send more.
        assert_eq!(schema["properties"]["new_tags"]["maxItems"], 2);
    }

    /// Anthropic refuses a schema with more than 16 parameters that have a
    /// union type (`type` arrays or `anyOf`), however many fields a type has.
    fn union_parameters(schema: &Value) -> usize {
        match schema {
            Value::Object(map) => {
                usize::from(map.get("type").is_some_and(Value::is_array))
                    + usize::from(map.contains_key("anyOf"))
                    + map.values().map(union_parameters).sum::<usize>()
            }
            Value::Array(items) => items.iter().map(union_parameters).sum(),
            _ => 0,
        }
    }

    #[test]
    fn field_schemas_stay_under_the_providers_union_limit() {
        let kinds = [
            FieldKind::Text,
            FieldKind::Identifier,
            FieldKind::Date,
            FieldKind::Number,
            FieldKind::Money,
            FieldKind::List,
        ];
        let defs: Vec<FieldDef> = (0..40)
            .map(|i| def(&format!("field_{i}"), kinds[i % kinds.len()]))
            .collect();
        let found = union_parameters(&fields_schema(&defs));
        assert!(found <= 16, "{found} union parameters");
    }

    #[test]
    fn fields_schema_has_one_entry_per_field_with_empty_for_unknown() {
        let defs = [
            def("number", FieldKind::Identifier),
            def("total", FieldKind::Money),
            def("items", FieldKind::List),
        ];
        let schema = fields_schema(&defs);
        let validator = compiles(&schema);
        assert!(validator.is_valid(&json!({
            "fields": {
                "number": {"value": "AB123", "page": 1},
                "total": {"value": {"amount": "10.00", "currency": "EUR"}, "page": 0},
                "items": {"value": [], "page": 0},
            },
            "facts": [{"key": "k", "label": "K", "value": "v", "page": 2}],
        })));
        // Unknown is empty, not null.
        assert!(!validator.is_valid(&json!({
            "fields": {
                "number": {"value": null, "page": 0},
                "total": {"value": {"amount": "", "currency": ""}, "page": 0},
                "items": {"value": [], "page": 0},
            },
            "facts": [],
        })));
        // A field the type does not have is refused.
        assert!(!validator.is_valid(&json!({
            "fields": {
                "number": {"value": "AB123", "page": 1},
                "total": {"value": {"amount": "", "currency": ""}, "page": 0},
                "items": {"value": [], "page": 0},
                "extra": {"value": "x", "page": 0},
            },
            "facts": [],
        })));
    }

    #[test]
    fn an_issuer_field_is_also_asked_whether_it_names_an_organization() {
        let issuer = FieldDef {
            issuer: true,
            ..def("issued_by_office", FieldKind::Text)
        };
        let schema = fields_schema(&[issuer, def("note", FieldKind::Text)]);
        let validator = compiles(&schema);
        assert!(validator.is_valid(&json!({
            "fields": {
                "issued_by_office": {"value": "Moldcell", "page": 1, "names_organization": true},
                "note": {"value": "", "page": 0},
            },
            "facts": [],
        })));
        // Required on the issuer field, absent on the others.
        assert!(!validator.is_valid(&json!({
            "fields": {
                "issued_by_office": {"value": "Moldcell", "page": 1},
                "note": {"value": "", "page": 0},
            },
            "facts": [],
        })));
        assert!(!validator.is_valid(&json!({
            "fields": {
                "issued_by_office": {"value": "", "page": 0, "names_organization": false},
                "note": {"value": "", "page": 0, "names_organization": false},
            },
            "facts": [],
        })));

        // An answer that leaves it out reads as "not an organization".
        let answer: FieldsAnswer = serde_json::from_value(json!({
            "fields": {"issued_by_office": {"value": "Testville", "page": 1}},
            "facts": [],
        }))
        .unwrap();
        assert!(!answer.fields["issued_by_office"].names_organization);
    }

    #[test]
    fn values_are_shaped_by_kind_and_bad_ones_are_dropped() {
        let value = |kind, raw: Value| field_value(&def("f", kind), &raw);
        assert_eq!(
            value(FieldKind::Identifier, json!(" L898902C3 ")),
            Some(FieldValue::Identifier("L898902C3".into()))
        );
        assert_eq!(
            value(FieldKind::Date, json!("2012-04-15")),
            Some(FieldValue::Date(
                NaiveDate::from_ymd_opt(2012, 4, 15).unwrap()
            ))
        );
        assert_eq!(value(FieldKind::Date, json!("2012-13-45")), None);
        // A day-first date is salvaged, a month-first one is never guessed.
        assert_eq!(
            value(FieldKind::Date, json!("15.04.2012")),
            Some(FieldValue::Date(
                NaiveDate::from_ymd_opt(2012, 4, 15).unwrap()
            ))
        );
        assert_eq!(value(FieldKind::Date, json!("04/15/2012")), None);
        assert_eq!(
            value(
                FieldKind::Money,
                json!({"amount": "1250.00", "currency": "eur"})
            ),
            Some(FieldValue::Money {
                amount: Decimal::new(125000, 2),
                currency: "EUR".into()
            })
        );
        assert_eq!(
            value(
                FieldKind::Money,
                json!({"amount": "lots", "currency": "EUR"})
            ),
            None
        );
        assert_eq!(
            value(FieldKind::Money, json!({"amount": "5", "currency": "EURO"})),
            None
        );
        assert_eq!(
            value(FieldKind::Number, json!("12.5")),
            Some(FieldValue::Number(Decimal::new(125, 1)))
        );
        assert_eq!(
            value(FieldKind::List, json!(["a", " ", "b"])),
            Some(FieldValue::List(vec!["a".into(), "b".into()]))
        );
        assert_eq!(value(FieldKind::List, json!([])), None);
        assert_eq!(value(FieldKind::Text, Value::Null), None);
        assert_eq!(value(FieldKind::Text, json!("   ")), None);
    }

    #[test]
    fn the_organization_must_say_whether_it_is_sure() {
        let schema = classification_schema(&ClassifyCandidates {
            correspondent_ids: vec!["c1".into()],
            ..Default::default()
        });
        let org_items = &schema["properties"]["organizations"]["items"];
        assert_eq!(org_items["properties"]["confident"]["type"], "boolean");
        let required = org_items["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "confident"));
        let validator = compiles(&schema);
        let without = json!({
            "title": "t", "document_date": null,
            "organizations": [{"id": null, "new_name": "Acme", "role": "issued_by"}],
            "summary": "s", "language": "en",
        });
        assert!(!validator.is_valid(&without));
    }

    #[test]
    fn a_missing_confidence_reads_as_sure_so_older_answers_still_parse() {
        let answer: Classification = serde_json::from_value(json!({
            "title": "t", "document_date": null,
            "organizations": [{"id": null, "new_name": "Acme"}],
            "summary": "s", "language": "en",
        }))
        .unwrap();
        assert!(answer.organizations[0].confident);
        assert_eq!(answer.organizations[0].role, Role::Unspecified);
    }

    #[test]
    fn enumerable_and_place_values_are_normalized_on_the_way_in() {
        let mut sex = def("sex", FieldKind::Text);
        sex.labels = Labels::from([("en".to_string(), "Sex".to_string())]);
        let birth_place = def("birth_place", FieldKind::Text);
        assert_eq!(
            field_value(&sex, &json!("МУЖ.")),
            Some(FieldValue::Text("M".into()))
        );
        assert_eq!(
            field_value(&sex, &json!("Sex: female")),
            Some(FieldValue::Text("F".into()))
        );
        assert_eq!(
            field_value(&birth_place, &json!("Place of birth: SPRINGFIELD")),
            Some(FieldValue::Text("Springfield".into()))
        );
        // Nothing but a label is nothing.
        assert_eq!(field_value(&birth_place, &json!("Place of birth:")), None);
        // Other text is left alone.
        assert_eq!(
            field_value(&def("surname", FieldKind::Text), &json!("TESTOV")),
            Some(FieldValue::Text("TESTOV".into()))
        );
    }
}
