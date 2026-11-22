//! What the LLM steps write to the database: one transaction per step, so a
//! failed step leaves nothing behind. Only values that are not the user's
//! (`manual`) are replaced. A value from a verified MRZ is never overwritten
//! by the model, a disagreement raises `mrz_mismatch` instead; the exception
//! are the names, which have no check digit: the printed reading replaces a
//! zone name it does not match exactly (see [`mrz::compare_name`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;

use serde_json::json;
use sqlx::SqliteConnection;

use crate::error::{Result, ServiceError};
use crate::ids::{CorrespondentId, DocumentId, PersonId, TagId};
use crate::llm::normalize;
use crate::llm::schemas::{Classification, FieldsAnswer, OrganizationAnswer, field_value};
use crate::matching;
use crate::model::document::{Provenance, Role};
use crate::model::fields::{FieldDef, FieldValue, Labels};
use crate::mrz::{self, NameAgreement};
use crate::pipeline::{checks, people};
use crate::service::{Actor, App, events};
use crate::time;

const MAX_TITLE_CHARS: usize = 300;
const MAX_SUMMARY_CHARS: usize = 2000;
const MAX_NAME_CHARS: usize = 200;
/// New tags one document may add. The archive's tags are a vocabulary the
/// reader browses, so a document adds one or two topics, not a description.
const MAX_NEW_TAGS: usize = 2;
const MAX_TAG_CHARS: usize = 40;
const MAX_TAG_WORDS: usize = 3;
const MAX_FACTS: usize = 20;
const MAX_FACT_KEY_CHARS: usize = 100;
const MAX_FACT_VALUE_CHARS: usize = 2000;

/// Error code when the document must not (or no longer) be sent to the LLM.
pub(crate) const NOT_ALLOWED: &str = "llm_not_allowed";

/// A step's answer may arrive after the user switched the document to
/// `local_only`; checked inside the write transaction (writes are serialized),
/// so nothing from the cloud lands once that switch was made.
async fn ensure_allowed(tx: &mut SqliteConnection, doc: DocumentId) -> Result<()> {
    let local_only: Option<bool> =
        sqlx::query_scalar("SELECT local_only FROM documents WHERE id = ?")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    if local_only == Some(false) {
        Ok(())
    } else {
        Err(ServiceError::forbidden(
            NOT_ALLOWED,
            "this document must not be sent to the LLM",
        ))
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    text.trim().chars().take(max_chars).collect()
}

/// The user's own values are never touched.
fn open(provenance: Option<Provenance>) -> bool {
    provenance != Some(Provenance::Manual)
}

// --- step A -----------------------------------------------------------------

/// Replaces the `llm` text layers with the transcribed pages.
pub async fn transcription(
    app: &App,
    doc: DocumentId,
    pages: &BTreeMap<u32, String>,
) -> Result<()> {
    if pages.is_empty() {
        return Ok(());
    }
    let mut tx = app.db.write.begin().await?;
    ensure_allowed(&mut tx, doc).await?;
    sqlx::query("DELETE FROM text_layers WHERE document_id = ? AND source = 'llm'")
        .bind(doc)
        .execute(&mut *tx)
        .await?;
    let now = time::now_ts();
    for (page, text) in pages {
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, word_boxes, quality_score, created_at) \
             VALUES (?, ?, 'llm', ?, NULL, NULL, ?)",
        )
        .bind(doc)
        .bind(i64::from(*page))
        .bind(text)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    events::record(
        &mut tx,
        &Actor::system(),
        "document.llm.transcribe",
        Some(doc),
        json!({"pages": pages.keys().collect::<Vec<_>>()}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// --- step B -----------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Provenances {
    title_provenance: Option<Provenance>,
    date_provenance: Option<Provenance>,
    type_provenance: Option<Provenance>,
    summary_provenance: Option<Provenance>,
}

/// Title, date, type, organizations, people, tags, summary and language.
/// `text` is the document text the model saw.
pub async fn classification(
    app: &App,
    doc: DocumentId,
    answer: &Classification,
    create_tags: bool,
    text: &str,
) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    ensure_allowed(&mut tx, doc).await?;
    let p: Provenances = sqlx::query_as(
        "SELECT title_provenance, date_provenance, type_provenance, summary_provenance \
         FROM documents WHERE id = ?",
    )
    .bind(doc)
    .fetch_one(&mut *tx)
    .await?;
    let mut changed: Vec<&str> = Vec::new();

    let date = answer.document_date.as_deref().and_then(normalize::date);
    if open(p.date_provenance)
        && let Some(date) = date
    {
        sqlx::query("UPDATE documents SET document_date = ?, date_provenance = 'llm' WHERE id = ?")
            .bind(date.to_string())
            .bind(doc)
            .execute(&mut *tx)
            .await?;
        changed.push("document_date");
    }

    // An MRZ-verified type (passport, ID card) stays.
    if !matches!(
        p.type_provenance,
        Some(Provenance::Manual | Provenance::Mrz)
    ) && let Some(key) = answer.type_key.as_deref()
    {
        let type_id: Option<(String,)> =
            sqlx::query_as("SELECT id FROM document_types WHERE key = ?")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((type_id,)) = type_id {
            sqlx::query("UPDATE documents SET type_id = ?, type_provenance = 'llm' WHERE id = ?")
                .bind(type_id)
                .bind(doc)
                .execute(&mut *tx)
                .await?;
            changed.push("type");
        }
    }

    if write_organizations(&mut tx, doc, answer, text).await? {
        changed.push("organizations");
    }

    // After the type and the correspondent: the title is shaped by both.
    let title = clip(&answer.title, MAX_TITLE_CHARS);
    if open(p.title_provenance) && !title.is_empty() {
        let title = shape_title(&mut tx, &title, answer).await?;
        if !title.is_empty() {
            sqlx::query("UPDATE documents SET title = ?, title_provenance = 'llm' WHERE id = ?")
                .bind(title)
                .bind(doc)
                .execute(&mut *tx)
                .await?;
            changed.push("title");
        }
    }

    if write_people(&mut tx, doc, answer, text).await? {
        changed.push("people");
    }
    if write_tags(&mut tx, doc, answer, create_tags).await? {
        changed.push("tags");
    }

    let summary = clip(&answer.summary, MAX_SUMMARY_CHARS);
    if open(p.summary_provenance) && !summary.is_empty() {
        sqlx::query("UPDATE documents SET summary = ?, summary_provenance = 'llm' WHERE id = ?")
            .bind(summary)
            .bind(doc)
            .execute(&mut *tx)
            .await?;
        changed.push("summary");
    }
    let language = clip(&answer.language, 16).to_lowercase();
    if !language.is_empty() {
        sqlx::query("UPDATE documents SET language = ? WHERE id = ?")
            .bind(language)
            .bind(doc)
            .execute(&mut *tx)
            .await?;
    }

    events::record(
        &mut tx,
        &Actor::system(),
        "document.llm.classify",
        Some(doc),
        json!({"changed": changed}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Types whose title ends in the year the document was issued
/// ("Иван Тестов, 2021"): identity documents and personal certificates.
const ISSUE_YEAR_TITLES: [&str; 9] = [
    "passport",
    "id_card",
    "travel_passport",
    "driver_license",
    "residence_permit",
    "birth_certificate",
    "marriage_certificate",
    "diploma",
    "certificate_other",
];

/// Title, its provenance, document date and type key of one document.
type TitleRow = (String, Option<Provenance>, Option<String>, Option<String>);

/// Puts the year of issue at the end of the model's title of an identity
/// document or personal certificate: the `issue_date` field, else the
/// document date. The model mixes up issue and expiry years, and one list
/// must not mean the one in some titles and the other in the rest. Runs
/// after the fields step; a title the user set stays as it is.
pub async fn settle_title_year(app: &App, doc: DocumentId) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    let removed = people::prune_mismatched_holder_people(&mut tx, doc).await?;
    let row: Option<TitleRow> = sqlx::query_as(
        "SELECT d.title, d.title_provenance, d.document_date, t.key \
         FROM documents d LEFT JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(doc)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((title, provenance, document_date, type_key)) = row else {
        tx.commit().await?;
        return Ok(());
    };
    let is_holder_doc = type_key.as_deref().is_some_and(people::is_holder_type);
    if provenance != Some(Provenance::Llm)
        || !type_key
            .as_deref()
            .is_some_and(|k| ISSUE_YEAR_TITLES.contains(&k))
    {
        if !removed.is_empty() {
            tx.commit().await?;
        }
        return Ok(());
    }
    let (head, existing_year) = split_title_year(&title);
    let mut base_title = title.clone();
    if is_holder_doc
        && let Some(holder_head) =
            people::reconcile_holder_title(&mut tx, doc, head, &removed).await?
    {
        base_title = match existing_year {
            Some(year) => format!("{holder_head}, {year}"),
            None => holder_head,
        };
    }
    let issued: Option<String> = sqlx::query_scalar(
        "SELECT value FROM field_values WHERE document_id = ? AND field_key = 'issue_date'",
    )
    .bind(doc)
    .fetch_optional(&mut *tx)
    .await?;
    let issue_year = issued
        .and_then(|v| match serde_json::from_str::<FieldValue>(&v).ok()? {
            FieldValue::Date(d) => Some(d.format("%Y").to_string()),
            _ => None,
        })
        .or_else(|| document_date.and_then(|d| d.get(..4).map(str::to_string)));
    let next_title = match issue_year.as_deref() {
        Some(year) => with_title_year(&base_title, year).unwrap_or_else(|| {
            if base_title != title && existing_year.is_none() {
                format!("{base_title}, {year}")
            } else {
                base_title
            }
        }),
        None => base_title,
    };
    if next_title == title && removed.is_empty() {
        return Ok(());
    }
    if next_title != title {
        sqlx::query("UPDATE documents SET title = ? WHERE id = ?")
            .bind(&next_title)
            .bind(doc)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn split_title_year(title: &str) -> (&str, Option<&str>) {
    let Some((head, tail)) = title.rsplit_once(", ") else {
        return (title, None);
    };
    let is_year = tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit());
    if is_year && !head.trim().is_empty() {
        (head, Some(tail))
    } else {
        (title, None)
    }
}

/// `title` with its trailing year (", 2031") replaced by `year`, or `None`
/// when it ends in no year or already in this one.
fn with_title_year(title: &str, year: &str) -> Option<String> {
    let (head, tail) = split_title_year(title);
    let tail = tail?;
    (tail != year).then(|| format!("{head}, {year}"))
}

/// The model's title in the archive's format (see `normalize::title`): no type
/// label in front of it, given every label of every type and every
/// correspondent that must not stand in front of one.
async fn shape_title(
    tx: &mut SqliteConnection,
    title: &str,
    answer: &Classification,
) -> Result<String> {
    let mut correspondents: Vec<String> = answer
        .organizations
        .iter()
        .filter_map(|o| o.new_name.as_deref().map(|n| clip(n, MAX_NAME_CHARS)))
        .collect();
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, aliases FROM correspondents")
        .fetch_all(&mut *tx)
        .await?;
    for (name, aliases) in rows {
        correspondents.push(name);
        correspondents.extend(serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default());
    }
    let labels: Vec<String> = sqlx::query_scalar("SELECT labels FROM document_types")
        .fetch_all(&mut *tx)
        .await?;
    let type_labels: Vec<String> = labels
        .into_iter()
        .flat_map(|l| {
            serde_json::from_str::<Labels>(&l)
                .unwrap_or_default()
                .into_values()
        })
        .filter(|l| !l.trim().is_empty())
        .collect();
    let shaped = normalize::title(
        title,
        &normalize::TitleContext {
            correspondents: &correspondents,
            type_labels: &type_labels,
        },
    );
    Ok(clip(&shaped, MAX_TITLE_CHARS))
}

/// Write organizations from the classification answer.
/// Only runs when no `manual` row exists on the document. The answer replaces
/// the `llm` and `local` rows, also when it names no organization: the model
/// saw the whole text and the listed correspondents, while the local match
/// only found a known name somewhere in it (a lease got the passport office of
/// the landlord's passport copy attached to it). An answer whose organizations
/// are all unsure drops only the `llm` rows.
async fn write_organizations(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    answer: &Classification,
    text: &str,
) -> Result<bool> {
    let manual_exists: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM document_organizations WHERE document_id = ? AND provenance = 'manual' LIMIT 1",
    )
    .bind(doc)
    .fetch_optional(&mut *tx)
    .await?;
    if manual_exists.is_some() {
        return Ok(false);
    }

    let before = organization_rows(tx, doc).await?;

    let mut new_links: Vec<(CorrespondentId, Role)> = Vec::new();
    for org in &answer.organizations {
        if !org.confident {
            continue;
        }
        if let Some(id) = resolve_organization(tx, org, text).await?
            && !new_links
                .iter()
                .any(|(cid, r)| *cid == id && *r == org.role)
        {
            new_links.push((id, org.role));
        }
    }

    // A model unsure of the organizations it saw leaves the local match.
    let unsure = new_links.is_empty() && !answer.organizations.is_empty();
    let stale = if unsure {
        "DELETE FROM document_organizations WHERE document_id = ? AND provenance = 'llm'"
    } else {
        "DELETE FROM document_organizations WHERE document_id = ? AND provenance IN ('llm', 'local')"
    };
    sqlx::query(stale).bind(doc).execute(&mut *tx).await?;

    for (id, role) in &new_links {
        sqlx::query(
            "INSERT OR IGNORE INTO document_organizations (document_id, correspondent_id, role, provenance)              VALUES (?, ?, ?, 'llm')",
        )
        .bind(doc)
        .bind(id)
        .bind(role.as_str())
        .execute(&mut *tx)
        .await?;
    }

    Ok(before != organization_rows(tx, doc).await?)
}

async fn organization_rows(
    tx: &mut SqliteConnection,
    doc: DocumentId,
) -> Result<Vec<(CorrespondentId, Role, String)>> {
    Ok(sqlx::query_as(
        "SELECT correspondent_id, role, provenance FROM document_organizations          WHERE document_id = ? ORDER BY correspondent_id, role, provenance",
    )
    .bind(doc)
    .fetch_all(&mut *tx)
    .await?)
}

/// The existing correspondent the model picked, or one matching its
/// `new_name` (by normalized name or alias), or a newly created one. None
/// when the model was not sure of the issuer (`confident` false) or its name
/// is a placeholder.
///
/// A picked correspondent must be backed by the document `text` or by the
/// printed `new_name` (see [`matching::pick_backed`], which also states the
/// trade-off for a correspondent stored in another language): models pick a
/// known organization of the same kind too readily ("МВД 12345" for a
/// passport issued by another МВД office). An unbacked pick falls back to
/// `new_name`.
async fn resolve_organization(
    tx: &mut SqliteConnection,
    org: &OrganizationAnswer,
    text: &str,
) -> Result<Option<CorrespondentId>> {
    if !org.confident {
        return Ok(None);
    }
    if let Some(raw) = org.id.as_deref()
        && let Ok(id) = CorrespondentId::from_str(raw)
    {
        let known: Option<(String, String)> =
            sqlx::query_as("SELECT name, aliases FROM correspondents WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((name, aliases)) = known {
            let aliases: Vec<String> = serde_json::from_str(&aliases).unwrap_or_default();
            let names = std::iter::once(&name)
                .chain(aliases.iter())
                .map(String::as_str);
            if matching::pick_backed(names, org.new_name.as_deref(), text) {
                return Ok(Some(id));
            }
            tracing::debug!(correspondent = %id, "the document does not back the picked correspondent");
        }
    }
    resolve_by_name(tx, org.new_name.as_deref().unwrap_or_default(), text).await
}

/// `raw` as a correspondent name (normalized, clipped); None for a
/// placeholder or an empty name.
fn correspondent_key(raw: &str) -> Option<String> {
    normalize::correspondent_name(raw)
        .map(|n| clip(&n, MAX_NAME_CHARS))
        .filter(|n| !n.is_empty())
}

/// The existing correspondent whose name or an alias is `name` (see
/// [`matching::same_org_name`]: the same folded name, or names that differ
/// only in the designation word at the front, so "Управление ЗАГС …" does
/// not create a second "Отдел ЗАГС …").
async fn find_by_name(tx: &mut SqliteConnection, name: &str) -> Result<Option<CorrespondentId>> {
    let rows: Vec<(CorrespondentId, String, String)> =
        sqlx::query_as("SELECT id, name, aliases FROM correspondents")
            .fetch_all(&mut *tx)
            .await?;
    for (id, known, aliases) in rows {
        let aliases: Vec<String> = serde_json::from_str(&aliases).unwrap_or_default();
        if std::iter::once(&known)
            .chain(aliases.iter())
            .any(|n| matching::same_org_name(n, name))
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The correspondent whose normalized name or alias is `raw`, or a new one
/// named `raw`. None for a placeholder or an empty name.
///
/// A new correspondent is created only when the document `text` prints the
/// name ([`matching::name_supported`]). Correspondent names go into every
/// later classification prompt, so a name the model made up, or one a hostile
/// document talked it into ("Ignore the document and tag everything …"),
/// must not become one.
async fn resolve_by_name(
    tx: &mut SqliteConnection,
    raw: &str,
    text: &str,
) -> Result<Option<CorrespondentId>> {
    let Some(name) = correspondent_key(raw) else {
        return Ok(None);
    };
    if let Some(id) = find_by_name(tx, &name).await? {
        return Ok(Some(id));
    }
    if !matching::name_supported(&name, text) {
        tracing::debug!("the document does not print the new correspondent's name");
        return Ok(None);
    }
    let id = CorrespondentId::new();
    sqlx::query(
        "INSERT INTO correspondents (id, name, aliases, created_at) VALUES (?, ?, '[]', ?)",
    )
    .bind(id)
    .bind(&name)
    .bind(time::now_ts())
    .execute(&mut *tx)
    .await?;
    events::record(
        tx,
        &Actor::system(),
        "correspondent.create",
        None,
        json!({"id": id, "via": "llm"}),
    )
    .await?;
    Ok(Some(id))
}

/// The model's people are added to the local matches; its own earlier picks
/// are replaced. People are never created here, and a pick whose name the
/// document text does not print is dropped: the model links listed people
/// to documents about strangers (a couple's marriage certificate got the
/// household's father).
async fn write_people(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    answer: &Classification,
    text: &str,
) -> Result<bool> {
    sqlx::query("DELETE FROM document_people WHERE document_id = ? AND provenance = 'llm'")
        .bind(doc)
        .execute(&mut *tx)
        .await?;
    let mut any = false;
    for raw in &answer.people {
        let Ok(person) = PersonId::from_str(raw) else {
            continue;
        };
        let known: Option<(String, String)> =
            sqlx::query_as("SELECT name, aliases FROM people WHERE id = ?")
                .bind(person)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((name, aliases)) = known else {
            continue;
        };
        let mut names = vec![name];
        names.extend(serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default());
        if !matching::person_backed(&names, text) {
            tracing::debug!(person = %person, "the document does not name the picked person");
            continue;
        }
        let done = sqlx::query(
            "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) \
             SELECT ?, id, 'llm' FROM people WHERE id = ?",
        )
        .bind(doc)
        .bind(person)
        .execute(&mut *tx)
        .await?;
        any |= done.rows_affected() > 0;
    }
    Ok(any)
}

/// A tag name in the form tags are compared in: spaces collapsed, lower case.
fn tag_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A new tag as it is stored: a short topic word or two in lower case. The
/// model's `#tag` marks, edge punctuation and spacing are dropped; `None` for
/// what is not a tag (empty, a sentence).
fn new_tag_name(raw: &str) -> Option<String> {
    let name = tag_key(raw.trim().trim_start_matches('#').trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '"' | '\'' | '«' | '»')
    }));
    let words = name.split(' ').count();
    (!name.is_empty() && name.chars().count() <= MAX_TAG_CHARS && words <= MAX_TAG_WORDS)
        .then_some(name)
}

/// Names a new tag must not repeat: the document types (their keys and labels
/// in every language), the people and the organizations, aliases included. A
/// tag "passport" next to the type Passport, or the name of a person, tells
/// the reader nothing a filter on that type or person does not.
async fn reserved_tag_names(tx: &mut SqliteConnection) -> Result<HashSet<String>> {
    let mut reserved = HashSet::new();
    let types: Vec<(String, String)> = sqlx::query_as("SELECT key, labels FROM document_types")
        .fetch_all(&mut *tx)
        .await?;
    for (key, labels) in types {
        reserved.insert(matching::name_key(&key.replace('_', " ")));
        for label in serde_json::from_str::<Labels>(&labels)
            .unwrap_or_default()
            .into_values()
        {
            reserved.insert(matching::name_key(&label));
        }
    }
    for query in [
        "SELECT name, aliases FROM people",
        "SELECT name, aliases FROM correspondents",
    ] {
        let rows: Vec<(String, String)> = sqlx::query_as(query).fetch_all(&mut *tx).await?;
        for (name, aliases) in rows {
            reserved.insert(matching::name_key(&name));
            for alias in serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default() {
                reserved.insert(matching::name_key(&alias));
            }
        }
    }
    reserved.remove("");
    Ok(reserved)
}

async fn write_tags(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    answer: &Classification,
    create_tags: bool,
) -> Result<bool> {
    sqlx::query("DELETE FROM document_tags WHERE document_id = ? AND provenance = 'llm'")
        .bind(doc)
        .execute(&mut *tx)
        .await?;
    // Existing tags by their comparison form: the column's NOCASE collation
    // only folds ASCII, so "Здоровье" and "здоровье" are two rows to SQL.
    let rows: Vec<(TagId, String)> = sqlx::query_as("SELECT id, name FROM tags")
        .fetch_all(&mut *tx)
        .await?;
    let mut known: HashMap<String, TagId> = rows
        .into_iter()
        .map(|(id, name)| (tag_key(&name), id))
        .collect();

    let mut any = false;
    for name in answer.tags.iter().map(|t| clip(t, MAX_NAME_CHARS)) {
        if let Some(tag) = known.get(&tag_key(&name)) {
            any |= link_tag(tx, doc, *tag).await?;
        }
    }
    // New tags only when the operator allowed them: a few at most, an
    // existing tag of the same name preferred over a new one.
    if create_tags {
        let reserved = reserved_tag_names(tx).await?;
        let mut created = 0;
        for raw in &answer.new_tags {
            let Some(name) = new_tag_name(&clip(raw, MAX_NAME_CHARS)) else {
                continue;
            };
            if let Some(tag) = known.get(&name) {
                any |= link_tag(tx, doc, *tag).await?;
                continue;
            }
            if reserved.contains(&matching::name_key(&name)) {
                continue;
            }
            if created == MAX_NEW_TAGS {
                break;
            }
            let id = TagId::new();
            sqlx::query("INSERT INTO tags (id, name, color) VALUES (?, ?, NULL)")
                .bind(id)
                .bind(&name)
                .execute(&mut *tx)
                .await?;
            known.insert(name, id);
            created += 1;
            any |= link_tag(tx, doc, id).await?;
        }
    }
    Ok(any)
}

async fn link_tag(tx: &mut SqliteConnection, doc: DocumentId, tag: TagId) -> Result<bool> {
    let done = sqlx::query(
        "INSERT OR IGNORE INTO document_tags (document_id, tag_id, provenance) VALUES (?, ?, 'llm')",
    )
    .bind(doc)
    .bind(tag)
    .execute(tx)
    .await?;
    Ok(done.rows_affected() > 0)
}

// --- step C -----------------------------------------------------------------

/// Field values and facts. `page_count` bounds the page numbers the model
/// reports.
pub async fn fields(
    app: &App,
    doc: DocumentId,
    defs: &[FieldDef],
    answer: &FieldsAnswer,
    page_count: u32,
    text: &str,
) -> Result<()> {
    let page_of = |page: Option<i64>| {
        page.and_then(|p| u32::try_from(p).ok())
            .filter(|p| (1..=page_count).contains(p))
    };
    let mut tx = app.db.write.begin().await?;
    ensure_allowed(&mut tx, doc).await?;
    checks::clear_mrz_mismatch_fields(&mut tx, doc).await?;
    sqlx::query("DELETE FROM field_values WHERE document_id = ? AND provenance = 'llm'")
        .bind(doc)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM facts WHERE document_id = ? AND provenance = 'llm'")
        .bind(doc)
        .execute(&mut *tx)
        .await?;

    let now = time::now_ts();
    let mut stored: Vec<&str> = Vec::new();
    let mut mismatch: Vec<&str> = Vec::new();
    let number = document_number(&mut tx, doc, defs, answer).await?;
    for def in defs {
        let Some(entry) = answer.fields.get(&def.key) else {
            continue;
        };
        let Some(value) = field_value(def, &entry.value) else {
            continue;
        };
        if def.key != NUMBER_FIELD
            && let (FieldValue::Identifier(other), Some(number)) = (&value, number.as_deref())
            && carries_number(other, number)
        {
            // The document number (with its MRZ check digit, a series glued
            // on) read into another identifier, typically the personal number.
            continue;
        }
        let existing: Option<(String, Provenance)> = sqlx::query_as(
            "SELECT value, provenance FROM field_values WHERE document_id = ? AND field_key = ?",
        )
        .bind(doc)
        .bind(&def.key)
        .fetch_optional(&mut *tx)
        .await?;
        match existing {
            // The user's value, or a verified MRZ value, wins.
            Some((_, Provenance::Manual)) => continue,
            Some((current, Provenance::Mrz)) => {
                let Ok(current) = serde_json::from_str::<FieldValue>(&current) else {
                    continue;
                };
                match (&current, &value) {
                    // A name has no check digit: OCR noise in the names line
                    // verifies like the rest of the zone. The printed reading
                    // replaces a zone name it does not match exactly.
                    (FieldValue::Text(zone), FieldValue::Text(printed))
                        if NAME_FIELDS.contains(&def.key.as_str()) =>
                    {
                        match mrz::compare_name(zone, printed) {
                            NameAgreement::Same => continue,
                            NameAgreement::Misread => {}
                            NameAgreement::Different => mismatch.push(&def.key),
                        }
                    }
                    _ => {
                        if disagrees(&current, &value) {
                            mismatch.push(&def.key);
                        }
                        continue;
                    }
                }
            }
            _ => {}
        }
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_by, updated_at) \
             VALUES (?, ?, ?, 'llm', ?, NULL, ?) \
             ON CONFLICT (document_id, field_key) DO UPDATE SET \
                 value = excluded.value, provenance = 'llm', page = excluded.page, \
                 updated_by = NULL, updated_at = excluded.updated_at",
        )
        .bind(doc)
        .bind(&def.key)
        .bind(json!(value).to_string())
        .bind(page_of(entry.page).map(i64::from))
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        stored.push(&def.key);
    }

    let mut facts = 0;
    for fact in &answer.facts {
        let key = clip(&normalize::fact_key(&fact.key), MAX_FACT_KEY_CHARS);
        let value = clip(&fact.value, MAX_FACT_VALUE_CHARS);
        let label = clip(&fact.label, MAX_NAME_CHARS);
        if facts == MAX_FACTS
            || value.is_empty()
            || normalize::is_placeholder(&value)
            || defs.iter().any(|d| d.key == key)
        {
            continue;
        }
        sqlx::query(
            "INSERT INTO facts (document_id, key, value, provenance, page, label) VALUES (?, ?, ?, 'llm', ?, ?)",
        )
        .bind(doc)
        .bind(key)
        .bind(value)
        .bind(page_of(fact.page).map(i64::from))
        .bind((!label.is_empty()).then_some(label))
        .execute(&mut *tx)
        .await?;
        facts += 1;
    }

    let issuer = link_issuer(&mut tx, doc, defs, answer, text).await?;

    if !mismatch.is_empty() {
        for key in &mismatch {
            sqlx::query(
                "UPDATE field_values SET mrz_mismatch = 1 WHERE document_id = ? AND field_key = ?",
            )
            .bind(doc)
            .bind(key)
            .execute(&mut *tx)
            .await?;
        }
        checks::add_flag(&mut tx, doc, checks::MRZ_MISMATCH).await?;
    }
    events::record(
        &mut tx,
        &Actor::system(),
        "document.llm.fields",
        Some(doc),
        json!({"fields": stored, "facts": facts, "mrz_mismatch": mismatch, "issuer": issuer}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

const NUMBER_FIELD: &str = "number";
/// The fields a zone's names line fills.
const NAME_FIELDS: [&str; 2] = ["surname", "given_names"];
/// Shorter document numbers are too likely to turn up inside an unrelated
/// identifier by chance.
const MIN_GUARDED_NUMBER: usize = 6;

/// The document number to keep out of other identifier fields: the
/// MRZ-verified one, else the model's own.
async fn document_number(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    defs: &[FieldDef],
    answer: &FieldsAnswer,
) -> Result<Option<String>> {
    let Some(def) = defs.iter().find(|d| d.key == NUMBER_FIELD) else {
        return Ok(None);
    };
    let verified: Option<(String,)> = sqlx::query_as(
        "SELECT value FROM field_values WHERE document_id = ? AND field_key = ? AND provenance = 'mrz'",
    )
    .bind(doc)
    .bind(NUMBER_FIELD)
    .fetch_optional(&mut *tx)
    .await?;
    let value = match verified {
        Some((json,)) => serde_json::from_str::<FieldValue>(&json).ok(),
        None => answer
            .fields
            .get(NUMBER_FIELD)
            .and_then(|e| field_value(def, &e.value)),
    };
    Ok(match value {
        Some(FieldValue::Identifier(n) | FieldValue::Text(n)) => Some(n),
        _ => None,
    })
}

fn alphanumeric(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Whether identifier `other` contains the document number.
fn carries_number(other: &str, number: &str) -> bool {
    let number = alphanumeric(number);
    number.len() >= MIN_GUARDED_NUMBER && alphanumeric(other).contains(&number)
}

/// When classification linked no organization at all, the issuer named by an
/// issuer field (`FieldDef::issuer`) becomes the `issued_by` organization, but
/// only when the model says the value names an organization or office
/// (`names_organization`): a place ("BUCUREȘTI" on a passport) is not one,
/// and a one-word company is. The model, not the word count, makes the call.
/// Returns whether a link was made.
async fn link_issuer(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    defs: &[FieldDef],
    answer: &FieldsAnswer,
    text: &str,
) -> Result<bool> {
    let linked: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM document_organizations WHERE document_id = ? LIMIT 1")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    if linked.is_some() {
        return Ok(false);
    }
    let mut found = None;
    for def in defs.iter().filter(|d| d.issuer) {
        let Some(entry) = answer.fields.get(&def.key) else {
            continue;
        };
        let Some(FieldValue::Text(name)) = field_value(def, &entry.value) else {
            continue;
        };
        found = if entry.names_organization {
            resolve_by_name(tx, &name, text).await?
        } else {
            // The model's call varies between runs on the same issuer; a name
            // the directory already holds as an organization is one anyway.
            // A place is never created here, so it never matches.
            match correspondent_key(&name) {
                Some(key) => find_by_name(tx, &key).await?,
                None => None,
            }
        };
        if found.is_some() {
            break;
        }
    }
    let Some(id) = found else {
        return Ok(false);
    };
    sqlx::query(
        "INSERT OR IGNORE INTO document_organizations (document_id, correspondent_id, role, provenance) \
         VALUES (?, ?, 'issued_by', 'llm')",
    )
    .bind(doc)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    Ok(true)
}

/// Drops the sentences of an LLM summary that state a number the document
/// does not print (see [`normalize::grounded_summary`]). Runs after the field
/// step, so the stored field values count as printed too. A summary with
/// nothing left is cleared.
pub async fn ground_summary(app: &App, doc: DocumentId, text: &str) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    ensure_allowed(&mut tx, doc).await?;
    let summary: Option<(Option<String>, Option<Provenance>)> =
        sqlx::query_as("SELECT summary, summary_provenance FROM documents WHERE id = ?")
            .bind(doc)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((Some(summary), Some(Provenance::Llm))) = summary else {
        return Ok(());
    };
    let values: Vec<(String,)> =
        sqlx::query_as("SELECT value FROM field_values WHERE document_id = ?")
            .bind(doc)
            .fetch_all(&mut *tx)
            .await?;
    let mut source = text.to_string();
    for (value,) in values {
        source.push('\n');
        source.push_str(&value);
    }
    let grounded = normalize::grounded_summary(&summary, &source);
    if grounded == summary {
        return Ok(());
    }
    let update = if grounded.is_empty() {
        sqlx::query("UPDATE documents SET summary = NULL, summary_provenance = NULL WHERE id = ?")
    } else {
        sqlx::query("UPDATE documents SET summary = ?, summary_provenance = 'llm' WHERE id = ?")
            .bind(grounded)
    };
    update.bind(doc).execute(&mut *tx).await?;
    events::record(
        &mut tx,
        &Actor::system(),
        "document.llm.summary_grounded",
        Some(doc),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Whether an LLM value contradicts the MRZ value of the same field.
/// Comparison is forgiving where the two legitimately differ in form: names
/// (the MRZ is upper-case Latin, the document may be Cyrillic), identifiers
/// (spacing) and free text such as nationality or sex are not compared at all.
fn disagrees(mrz: &FieldValue, llm: &FieldValue) -> bool {
    fn squash(s: &str) -> String {
        matching::normalize(s)
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect()
    }
    match (mrz, llm) {
        (FieldValue::Date(a), FieldValue::Date(b)) => a != b,
        (FieldValue::Identifier(a), FieldValue::Identifier(b)) => squash(a) != squash(b),
        (FieldValue::Text(a), FieldValue::Text(b)) => {
            // Short codes (sex, nationality) differ in form; names are
            // compared by containment, in the spelling a passport zone uses
            // for Cyrillic (ICAO: Я = IA, Х = KH, Ц = TS, Щ = SHCH).
            let icao = |s: &str| {
                squash(s)
                    .replace("shch", "shh")
                    .replace("kh", "h")
                    .replace("ts", "c")
                    .replace('y', "i")
            };
            let (a, b) = (icao(a), icao(b));
            a.len() > 3 && b.len() > 3 && !a.contains(&b) && !b.contains(&a)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn id(s: &str) -> FieldValue {
        FieldValue::Identifier(s.into())
    }

    fn text(s: &str) -> FieldValue {
        FieldValue::Text(s.into())
    }

    #[test]
    fn the_title_year_becomes_the_year_of_issue() {
        assert_eq!(
            with_title_year("Ivan Testov, 2036", "2026").as_deref(),
            Some("Ivan Testov, 2026")
        );
        assert_eq!(with_title_year("Иван Тестов, 2021", "2021"), None);
        assert_eq!(with_title_year("Список клиник по ДМС", "2021"), None);
        assert_eq!(with_title_year("Отчёт, Q3", "2021"), None);
        assert_eq!(with_title_year(", 2031", "2021"), None);
    }

    #[test]
    fn identifiers_differ_only_in_spacing_agree() {
        assert!(!disagrees(&id("L898902C3"), &id("l898 902 c3")));
        assert!(disagrees(&id("L898902C3"), &id("L898902C8")));
    }

    #[test]
    fn dates_must_match_exactly() {
        let d = |y, m, day| FieldValue::Date(NaiveDate::from_ymd_opt(y, m, day).unwrap());
        assert!(!disagrees(&d(2012, 4, 15), &d(2012, 4, 15)));
        assert!(disagrees(&d(2012, 4, 15), &d(2012, 5, 15)));
    }

    #[test]
    fn names_are_compared_across_scripts_and_case() {
        assert!(!disagrees(&text("TESTOV"), &text("Тестов")));
        assert!(!disagrees(&text("IVAN MARIA"), &text("Ivan")));
        assert!(disagrees(&text("TESTOV"), &text("Petrov")));
        // The zone spells Cyrillic the ICAO way.
        assert!(!disagrees(&text("IAKOVLEVA"), &text("ЯКОВЛЕВА")));
        assert!(!disagrees(&text("IURII"), &text("Юрий")));
        assert!(!disagrees(&text("KHARITONOV"), &text("Харитонов")));
        assert!(!disagrees(&text("TSVETKOVA"), &text("Цветкова")));
        assert!(!disagrees(&text("SHCHUKIN"), &text("Щукин")));
        assert!(disagrees(&text("IAKOVLEVA"), &text("Яковенко")));
    }

    #[test]
    fn the_document_number_inside_another_identifier_is_caught() {
        // Series digits, number and MRZ check digit glued together.
        assert!(carries_number("4501234568", "123456"));
        assert!(carries_number("L898902C36", "L898 902 C3"));
        // A personal number that merely differs.
        assert!(!carries_number("1900115123456", "AB7654321"));
        // Too short to tell.
        assert!(!carries_number("1234567", "12345"));
    }

    #[test]
    fn short_codes_are_not_compared() {
        assert!(!disagrees(&text("UTO"), &text("Utopia")));
        assert!(!disagrees(&text("F"), &text("female")));
    }
}
