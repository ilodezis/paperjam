//! Documents as seen by callers: metadata, people, tags, typed fields with
//! provenance, facts and (paged) text.

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::sqlite::{SqliteTypeInfo, SqliteValueRef};
use sqlx::{Database, Decode, Encode, Sqlite, Type};

use crate::ids::{CorrespondentId, DocumentId, PersonId, TagId, TypeId, UserId};
use crate::model::fields::FieldValue;

/// Where a value came from. `manual` values are never overwritten by
/// reprocessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Llm,
    Mrz,
    Manual,
    Local,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Llm => "llm",
            Provenance::Mrz => "mrz",
            Provenance::Manual => "manual",
            Provenance::Local => "local",
        }
    }
}

impl FromStr for Provenance {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "llm" => Ok(Provenance::Llm),
            "mrz" => Ok(Provenance::Mrz),
            "manual" => Ok(Provenance::Manual),
            "local" => Ok(Provenance::Local),
            other => Err(format!("unknown provenance {other:?}")),
        }
    }
}

impl Type<Sqlite> for Provenance {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl Encode<'_, Sqlite> for Provenance {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'_, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Provenance {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <&str as Decode<Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

/// The role an organization plays in a document.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    Default,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    IssuedBy,
    ReceivedFrom,
    Party,
    Employer,
    #[default]
    Unspecified,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::IssuedBy => "issued_by",
            Role::ReceivedFrom => "received_from",
            Role::Party => "party",
            Role::Employer => "employer",
            Role::Unspecified => "unspecified",
        }
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "issued_by" => Ok(Role::IssuedBy),
            "received_from" => Ok(Role::ReceivedFrom),
            "party" => Ok(Role::Party),
            "employer" => Ok(Role::Employer),
            "unspecified" => Ok(Role::Unspecified),
            other => Err(format!("unknown role {other:?}")),
        }
    }
}

impl Type<Sqlite> for Role {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl Encode<'_, Sqlite> for Role {
    fn encode_by_ref(
        &self,
        buf: &mut <Sqlite as Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <&str as Encode<'_, Sqlite>>::encode(self.as_str(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Role {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <&str as Decode<Sqlite>>::decode(value)?;
        Ok(text.parse()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Queued,
    Processing,
    Ready,
    Failed,
}

impl DocumentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DocumentStatus::Queued => "queued",
            DocumentStatus::Processing => "processing",
            DocumentStatus::Ready => "ready",
            DocumentStatus::Failed => "failed",
        }
    }
}

impl FromStr for DocumentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(DocumentStatus::Queued),
            "processing" => Ok(DocumentStatus::Processing),
            "ready" => Ok(DocumentStatus::Ready),
            "failed" => Ok(DocumentStatus::Failed),
            other => Err(format!("unknown status {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileInfo {
    pub sha256: String,
    pub mime: String,
    pub size: u64,
    /// The name the file had when uploaded. Display only; never a path.
    pub original_name: String,
    pub page_count: Option<u32>,
    /// The document has page images and a thumbnail to show. A plain-text
    /// file and a document that failed before its pages were rendered have
    /// none, and the image routes answer 404 for them.
    pub has_pages: bool,
    /// The name a download of the document gets: its title, with `.pdf` when
    /// the download is the clean copy. Display only; never a path.
    pub download_name: String,
    /// The name a download of the original gets.
    pub original_download_name: String,
    /// Size in bytes of the clean copy of a scan (its pages upright, cut out
    /// and straightened, as a PDF), when there is one. Downloads serve it.
    pub clean_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonRef {
    pub id: PersonId,
    pub name: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagRef {
    pub id: TagId,
    pub name: String,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DocumentOrganization {
    #[schema(value_type = String)]
    pub id: CorrespondentId,
    pub name: String,
    pub role: Role,
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct OrganizationLink {
    #[schema(value_type = String)]
    pub id: CorrespondentId,
    #[serde(default)]
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldEntry {
    pub key: String,
    pub value: FieldValue,
    pub provenance: Provenance,
    pub page: Option<u32>,
    /// Attention flags this value raises (`checksum_failed`,
    /// `ocr_llm_disagree`). Saving the value by hand clears them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// A value the model found important that fits no field of the type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fact {
    pub key: String,
    /// What the fact is, in the uploader's language; `None` when the model
    /// gave no label (the UI then names it by its key).
    #[serde(default)]
    pub label: Option<String>,
    pub value: String,
    pub provenance: Provenance,
    pub page: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PagedText {
    pub page: u32,
    pub page_count: u32,
    /// Which layer is shown: `embedded`, `tesseract` or `llm`.
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentView {
    pub id: DocumentId,
    pub title: Option<String>,
    pub title_provenance: Option<Provenance>,
    pub document_date: Option<NaiveDate>,
    pub date_provenance: Option<Provenance>,
    /// When the document has to be renewed or replaced (see
    /// [`crate::model::due`]); derived from the fields, never stored.
    pub due_date: Option<NaiveDate>,
    pub type_id: Option<TypeId>,
    pub type_key: Option<String>,
    pub type_provenance: Option<Provenance>,
    pub organizations: Vec<DocumentOrganization>,
    pub summary: Option<String>,
    pub summary_provenance: Option<Provenance>,
    pub language: Option<String>,
    pub local_only: bool,
    pub status: DocumentStatus,
    pub status_reason: Option<String>,
    pub attention: Vec<String>,
    pub added_by: Option<UserId>,
    pub updated_by: Option<UserId>,
    pub created_at: String,
    pub updated_at: String,
    pub trashed_at: Option<String>,
    pub file: FileInfo,
    pub people: Vec<PersonRef>,
    pub tags: Vec<TagRef>,
    pub fields: Vec<FieldEntry>,
    pub facts: Vec<Fact>,
    pub text: Option<PagedText>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GetOptions {
    /// Include the text of this (1-based) page.
    pub text_page: Option<u32>,
}

/// Reads `null` as `Some(None)` and a missing key as `None` (with
/// `#[serde(default)]`), so a patch can tell "clear" from "leave alone".
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A partial update. Absent = unchanged, `null` = clear, a value = set.
/// Every value touched gets provenance `manual`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocumentPatch {
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub document_date: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "double_option")]
    pub type_id: Option<Option<TypeId>>,
    /// Replaces the whole list.
    #[serde(default)]
    pub organizations: Option<Vec<OrganizationLink>>,
    /// Replaces the whole list.
    #[serde(default)]
    pub people: Option<Vec<PersonId>>,
    /// Replaces the whole list.
    #[serde(default)]
    pub tags: Option<Vec<TagId>>,
    /// `null` clears the field.
    #[serde(default)]
    pub fields: BTreeMap<String, Option<FieldValue>>,
    #[serde(default, deserialize_with = "double_option")]
    pub summary: Option<Option<String>>,
    #[serde(default)]
    pub local_only: Option<bool>,
    /// Attention flags the user confirmed and wants to clear.
    #[serde(default)]
    pub dismiss_attention: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_tells_absent_from_null() {
        let p: DocumentPatch = serde_json::from_value(json!({
            "title": null,
            "summary": "kept",
            "fields": {"expiry_date": null, "number": {"kind": "identifier", "value": "AB1"}}
        }))
        .unwrap();
        assert_eq!(p.title, Some(None));
        assert_eq!(p.summary, Some(Some("kept".to_string())));
        assert_eq!(p.document_date, None);
        assert_eq!(p.type_id, None);
        assert_eq!(p.fields["expiry_date"], None);
        assert_eq!(
            p.fields["number"],
            Some(FieldValue::Identifier("AB1".into()))
        );
        assert!(p.people.is_none() && p.local_only.is_none());
    }

    #[test]
    fn empty_patch_changes_nothing() {
        let p: DocumentPatch = serde_json::from_value(json!({})).unwrap();
        assert!(p.title.is_none() && p.fields.is_empty() && p.tags.is_none());
    }

    #[test]
    fn provenance_roundtrips_as_text() {
        for p in [
            Provenance::Llm,
            Provenance::Mrz,
            Provenance::Manual,
            Provenance::Local,
        ] {
            assert_eq!(p.as_str().parse::<Provenance>().unwrap(), p);
            assert_eq!(serde_json::to_value(p).unwrap(), json!(p.as_str()));
        }
        assert!("robot".parse::<Provenance>().is_err());
    }

    #[test]
    fn role_roundtrips_and_orders() {
        for (r, s) in [
            (Role::IssuedBy, "issued_by"),
            (Role::ReceivedFrom, "received_from"),
            (Role::Party, "party"),
            (Role::Employer, "employer"),
            (Role::Unspecified, "unspecified"),
        ] {
            assert_eq!(r.as_str().parse::<Role>().unwrap(), r);
            assert_eq!(serde_json::to_value(r).unwrap(), json!(s));
        }
        assert!("unknown".parse::<Role>().is_err());
        assert_eq!(Role::default(), Role::Unspecified);
        assert!(
            Role::IssuedBy < Role::ReceivedFrom
                && Role::ReceivedFrom < Role::Party
                && Role::Party < Role::Employer
                && Role::Employer < Role::Unspecified
        );
    }

    #[test]
    fn patch_deserializes_organizations_with_default_role() {
        let cid = CorrespondentId::new();
        let p: DocumentPatch = serde_json::from_value(json!({
            "organizations": [
                {"id": cid},
                {"id": cid, "role": "party"}
            ]
        }))
        .unwrap();
        let orgs = p.organizations.unwrap();
        assert_eq!(orgs.len(), 2);
        assert_eq!(orgs[0].id, cid);
        assert_eq!(orgs[0].role, Role::Unspecified);
        assert_eq!(orgs[1].id, cid);
        assert_eq!(orgs[1].role, Role::Party);
    }
}
