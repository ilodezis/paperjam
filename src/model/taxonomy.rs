//! People, correspondents, tags and document types: the shared vocabulary
//! documents are filed under.

use serde::{Deserialize, Serialize};

use crate::ids::{CorrespondentId, PersonId, TagId, TypeId};
use crate::model::fields::{FieldDef, Labels};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonView {
    pub id: PersonId,
    pub name: String,
    pub aliases: Vec<String>,
    pub notes: String,
    pub created_at: String,
    /// Documents (not in the trash) that mention this person.
    pub document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorrespondentView {
    pub id: CorrespondentId,
    pub name: String,
    pub aliases: Vec<String>,
    pub created_at: String,
    pub document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagView {
    pub id: TagId,
    pub name: String,
    pub color: Option<String>,
    pub document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeView {
    pub id: TypeId,
    pub key: String,
    pub labels: Labels,
    pub fields: Vec<FieldDef>,
    /// Field keys shown on a search hit, in this order. Empty: the first
    /// fields of the type.
    pub summary_fields: Vec<String>,
    pub builtin: bool,
    pub created_at: String,
    pub document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Taxonomy {
    pub types: Vec<TypeView>,
    pub people: Vec<PersonView>,
    pub correspondents: Vec<CorrespondentView>,
    pub tags: Vec<TagView>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewPerson {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonPatch {
    pub name: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewCorrespondent {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorrespondentPatch {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewType {
    pub key: String,
    pub labels: Labels,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
}

/// `fields`, when present, replaces the whole list. Removing a field only
/// hides it: values already stored for it stay in the database and remain
/// visible as facts.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TypePatch {
    pub labels: Option<Labels>,
    pub fields: Option<Vec<FieldDef>>,
}
