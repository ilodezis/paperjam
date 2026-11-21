//! Reading and editing documents; trash, restore and purge.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::SqliteConnection;

use crate::error::{Result, ServiceError};
use crate::extract::quality;
use crate::ids::{CorrespondentId, DocumentId, PersonId, TagId, TypeId, UserId};
use crate::jobs::queue::{JobKind, ProcessOptions};
use crate::model::document::{
    DocumentOrganization, DocumentPatch, DocumentStatus, DocumentView, Fact, FieldEntry, FileInfo,
    GetOptions, OrganizationLink, PagedText, PersonRef, Provenance, Role, TagRef,
};
use crate::model::due;
use crate::model::fields::{FieldDef, FieldValue};
use crate::pipeline::checks;
use crate::search::sync;
use crate::service::{App, Ctx, Via, events, naming};
use crate::time;

fn document_not_found() -> ServiceError {
    ServiceError::not_found("document_not_found", "no such document")
}

#[derive(sqlx::FromRow)]
struct DocRow {
    id: DocumentId,
    title: Option<String>,
    title_provenance: Option<Provenance>,
    document_date: Option<String>,
    date_provenance: Option<Provenance>,
    type_id: Option<TypeId>,
    type_key: Option<String>,
    type_provenance: Option<Provenance>,
    type_fields: Option<String>,
    summary: Option<String>,
    summary_provenance: Option<Provenance>,
    language: Option<String>,
    local_only: bool,
    status: String,
    status_reason: Option<String>,
    attention: String,
    added_by: Option<UserId>,
    updated_by: Option<UserId>,
    created_at: String,
    updated_at: String,
    trashed_at: Option<String>,
    sha256: String,
    mime: String,
    size: i64,
    original_name: String,
    page_count: Option<i64>,
    clean_size: Option<i64>,
}

const DOC_SELECT: &str = "SELECT d.id, d.title, d.title_provenance, d.document_date, d.date_provenance, \
    d.type_id, t.key AS type_key, d.type_provenance, t.fields AS type_fields, \
    d.summary, d.summary_provenance, d.language, d.local_only, d.status, d.status_reason, \
    d.attention, d.added_by, d.updated_by, d.created_at, d.updated_at, d.trashed_at, \
    f.sha256, f.mime, f.size, f.original_name, f.page_count, f.clean_size \
    FROM documents d \
    JOIN files f ON f.document_id = d.id \
    LEFT JOIN document_types t ON t.id = d.type_id \
    WHERE d.id = ?";

async fn load_row(conn: &mut SqliteConnection, id: DocumentId) -> Result<DocRow> {
    sqlx::query_as(DOC_SELECT)
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(document_not_found)
}

fn type_field_defs(row: &DocRow) -> Vec<FieldDef> {
    row.type_fields
        .as_deref()
        .and_then(|f| serde_json::from_str(f).ok())
        .unwrap_or_default()
}

/// key, label, value, provenance, page
type FactRow = (String, Option<String>, String, Provenance, Option<i64>);

pub async fn get_document(
    ctx: &Ctx<'_>,
    id: DocumentId,
    options: GetOptions,
) -> Result<DocumentView> {
    let mut conn = ctx.app.db.read.acquire().await?;
    let row = load_row(&mut conn, id).await?;
    let status = row
        .status
        .parse::<DocumentStatus>()
        .map_err(|_| ServiceError::internal("data_invalid", "unknown document status"))?;
    let attention: Vec<String> = serde_json::from_str(&row.attention).unwrap_or_default();
    let defs = type_field_defs(&row);

    let people: Vec<(PersonId, String, Provenance)> = sqlx::query_as(
        "SELECT p.id, p.name, dp.provenance FROM document_people dp \
         JOIN people p ON p.id = dp.person_id WHERE dp.document_id = ? ORDER BY p.name COLLATE NOCASE, p.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let tags: Vec<(TagId, String, Provenance)> = sqlx::query_as(
        "SELECT t.id, t.name, dt.provenance FROM document_tags dt \
         JOIN tags t ON t.id = dt.tag_id WHERE dt.document_id = ? ORDER BY t.name COLLATE NOCASE, t.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let field_rows: Vec<(String, String, Provenance, Option<i64>)> = sqlx::query_as(
        "SELECT field_key, value, provenance, page FROM field_values WHERE document_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let fact_rows: Vec<FactRow> = sqlx::query_as(
        "SELECT key, label, value, provenance, page FROM facts WHERE document_id = ? ORDER BY rowid",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;

    let mut fields: Vec<FieldEntry> = field_rows
        .into_iter()
        .filter_map(|(key, value, provenance, page)| {
            Some(FieldEntry {
                key,
                value: serde_json::from_str(&value).ok()?,
                provenance,
                page: page.map(|p| p as u32),
                issues: Vec::new(),
            })
        })
        .collect();
    if !fields.is_empty() {
        let issues = if status == DocumentStatus::Ready {
            checks::field_issues_for_attention(&mut conn, id, &attention).await?
        } else {
            checks::field_issues(&mut conn, id).await?
        };
        for (key, flag) in issues {
            if let Some(field) = fields.iter_mut().find(|f| f.key == key) {
                field.issues.push(flag.to_string());
            }
        }
    }
    // The type's own order first; values orphaned by a type edit last.
    fields.sort_by_key(|f| {
        (
            defs.iter()
                .position(|d| d.key == f.key)
                .unwrap_or(usize::MAX),
            f.key.clone(),
        )
    });

    let text = match options.text_page {
        Some(page) => Some(load_text(&mut conn, id, page, row.page_count).await?),
        None => None,
    };

    let org_rows: Vec<(CorrespondentId, String, Role, Option<Provenance>)> = sqlx::query_as(
        "SELECT c.id, c.name, do_org.role, do_org.provenance \
         FROM document_organizations do_org \
         JOIN correspondents c ON c.id = do_org.correspondent_id \
         WHERE do_org.document_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;

    let mut organizations: Vec<DocumentOrganization> = org_rows
        .into_iter()
        .map(|(id, name, role, provenance)| DocumentOrganization {
            id,
            name,
            role,
            provenance,
        })
        .collect();
    organizations.sort_by(|a, b| (a.role, &a.name).cmp(&(b.role, &b.name)));

    let original_download_name = naming::download_name(
        row.title.as_deref(),
        &row.original_name,
        &naming::extension_for(&row.mime, &row.original_name),
    );
    let download_name = match row.clean_size {
        Some(_) => naming::download_name(row.title.as_deref(), &row.original_name, "pdf"),
        None => original_download_name.clone(),
    };

    let due_date = due::due_date(row.type_key.as_deref(), |key| {
        fields.iter().find(|f| f.key == key).map(|f| &f.value)
    });
    Ok(DocumentView {
        id: row.id,
        title: row.title,
        title_provenance: row.title_provenance,
        document_date: row
            .document_date
            .as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        date_provenance: row.date_provenance,
        due_date,
        type_id: row.type_id,
        type_key: row.type_key,
        type_provenance: row.type_provenance,
        organizations,
        summary: row.summary,
        summary_provenance: row.summary_provenance,
        language: row.language,
        local_only: row.local_only,
        status,
        status_reason: row.status_reason,
        attention,
        added_by: row.added_by,
        updated_by: row.updated_by,
        created_at: row.created_at,
        updated_at: row.updated_at,
        trashed_at: row.trashed_at,
        file: FileInfo {
            download_name,
            original_download_name,
            clean_size: row.clean_size.map(|s| s.max(0) as u64),
            sha256: row.sha256,
            mime: row.mime,
            size: row.size as u64,
            original_name: row.original_name,
            page_count: row.page_count.map(|p| p as u32),
            has_pages: ctx.app.files.has_page_images(id).await,
        },
        people: people
            .into_iter()
            .map(|(id, name, provenance)| PersonRef {
                id,
                name,
                provenance,
            })
            .collect(),
        tags: tags
            .into_iter()
            .map(|(id, name, provenance)| TagRef {
                id,
                name,
                provenance,
            })
            .collect(),
        fields,
        facts: fact_rows
            .into_iter()
            .map(|(key, label, value, provenance, page)| Fact {
                key,
                label,
                value,
                provenance,
                page: page.map(|p| p as u32),
            })
            .collect(),
        text,
    })
}

/// One text layer of a page: `(source, text, quality score)`.
pub(crate) type Layer = (String, String, Option<f64>);

/// Picks the layer to show for one page: good embedded text (exact), else
/// LLM, else OCR, else whatever embedded text there is.
pub(crate) fn best_layer(layers: &[Layer]) -> Option<&Layer> {
    let find = |source: &str| layers.iter().find(|l| l.0 == source);
    let good_embedded =
        find("embedded").filter(|l| l.2.unwrap_or(0.0) >= f64::from(quality::GOOD_THRESHOLD));
    good_embedded
        .or_else(|| find("llm"))
        .or_else(|| find("tesseract"))
        .or_else(|| find("embedded"))
}

async fn load_text(
    conn: &mut SqliteConnection,
    id: DocumentId,
    page: u32,
    declared_pages: Option<i64>,
) -> Result<PagedText> {
    let known_pages = match declared_pages {
        Some(n) => n,
        None => {
            sqlx::query_scalar(
                "SELECT COALESCE(MAX(page), 0) FROM text_layers WHERE document_id = ?",
            )
            .bind(id)
            .fetch_one(&mut *conn)
            .await?
        }
    };
    if page == 0 || (known_pages > 0 && i64::from(page) > known_pages) {
        return Err(ServiceError::invalid(
            "page_out_of_range",
            format!("pages are numbered 1 to {known_pages}"),
        ));
    }
    let layers: Vec<(String, String, Option<f64>)> = sqlx::query_as(
        "SELECT source, text, quality_score FROM text_layers WHERE document_id = ? AND page = ?",
    )
    .bind(id)
    .bind(i64::from(page))
    .fetch_all(&mut *conn)
    .await?;

    let (source, text) = best_layer(&layers).map_or(("none".to_string(), String::new()), |l| {
        (l.0.clone(), l.1.clone())
    });
    Ok(PagedText {
        page,
        page_count: known_pages as u32,
        source,
        text,
    })
}

// --- editing ------------------------------------------------------------------

fn unknown_field(key: &str) -> ServiceError {
    ServiceError::invalid(
        "unknown_field",
        format!("the document type has no field {key:?}"),
    )
    .with_details(json!({"field": key}))
}

async fn must_exist(
    tx: &mut SqliteConnection,
    sql: &'static str,
    id: String,
    code: &'static str,
    message: &'static str,
) -> Result<()> {
    let found: Option<(i64,)> = sqlx::query_as(sql).bind(id).fetch_optional(tx).await?;
    found
        .map(|_| ())
        .ok_or_else(|| ServiceError::not_found(code, message))
}

/// Removes everything the LLM produced, so a `local_only` document keeps
/// only locally derived data.
async fn drop_llm_data(tx: &mut SqliteConnection, id: DocumentId) -> Result<()> {
    for sql in [
        "DELETE FROM text_layers WHERE document_id = ?1 AND source = 'llm'",
        "DELETE FROM field_values WHERE document_id = ?1 AND provenance = 'llm'",
        "DELETE FROM facts WHERE document_id = ?1 AND provenance = 'llm'",
        "DELETE FROM document_people WHERE document_id = ?1 AND provenance = 'llm'",
        "DELETE FROM document_tags WHERE document_id = ?1 AND provenance = 'llm'",
        "DELETE FROM document_organizations WHERE document_id = ?1 AND provenance = 'llm'",
        "UPDATE documents SET title = NULL, title_provenance = NULL WHERE id = ?1 AND title_provenance = 'llm'",
        "UPDATE documents SET document_date = NULL, date_provenance = NULL WHERE id = ?1 AND date_provenance = 'llm'",
        "UPDATE documents SET type_id = NULL, type_provenance = NULL WHERE id = ?1 AND type_provenance = 'llm'",
        "UPDATE documents SET summary = NULL, summary_provenance = NULL WHERE id = ?1 AND summary_provenance = 'llm'",
    ] {
        sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    }
    Ok(())
}

pub async fn update_document(
    ctx: &Ctx<'_>,
    id: DocumentId,
    patch: DocumentPatch,
) -> Result<DocumentView> {
    for flag in &patch.dismiss_attention {
        if !checks::is_known_flag(flag) {
            return Err(ServiceError::invalid(
                "invalid_attention_flag",
                format!("unknown attention flag {flag:?}"),
            ));
        }
    }
    let mut tx = ctx.app.db.write.begin().await?;
    let row = load_row(&mut tx, id).await?;
    let mut diff = serde_json::Map::new();
    let mut requeue_local = false;

    if let Some(title) = &patch.title {
        let new = match title {
            Some(t) if t.trim().is_empty() => {
                return Err(ServiceError::invalid(
                    "invalid_title",
                    "the title cannot be blank",
                ));
            }
            Some(t) => Some(t.trim().to_string()),
            None => None,
        };
        sqlx::query("UPDATE documents SET title = ?, title_provenance = ? WHERE id = ?")
            .bind(&new)
            .bind(new.as_ref().map(|_| "manual"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        diff.insert("title".into(), json!({"from": row.title, "to": new}));
    }
    if let Some(date) = &patch.document_date {
        sqlx::query("UPDATE documents SET document_date = ?, date_provenance = ? WHERE id = ?")
            .bind(date.map(|d| d.to_string()))
            .bind(date.as_ref().map(|_| "manual"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        diff.insert(
            "document_date".into(),
            json!({"from": row.document_date, "to": date}),
        );
    }
    let mut effective_defs = type_field_defs(&row);
    if let Some(new_type) = patch.type_id {
        if let Some(t) = new_type {
            must_exist(
                &mut tx,
                "SELECT 1 FROM document_types WHERE id = ?",
                t.to_string(),
                "type_not_found",
                "no such document type",
            )
            .await?;
        }
        sqlx::query("UPDATE documents SET type_id = ?, type_provenance = ? WHERE id = ?")
            .bind(new_type)
            .bind(new_type.map(|_| "manual"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        diff.insert(
            "type_id".into(),
            json!({"from": row.type_id, "to": new_type}),
        );
        let fields_json: Option<String> = match new_type {
            Some(t) => {
                sqlx::query_scalar("SELECT fields FROM document_types WHERE id = ?")
                    .bind(t)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            None => None,
        };
        effective_defs = fields_json
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default();
    }
    if let Some(orgs) = &patch.organizations {
        for org in orgs {
            must_exist(
                &mut tx,
                "SELECT 1 FROM correspondents WHERE id = ?",
                org.id.to_string(),
                "correspondent_not_found",
                "no such correspondent",
            )
            .await?;
        }
        let old_orgs: Vec<(CorrespondentId, Role)> = sqlx::query_as(
            "SELECT correspondent_id, role FROM document_organizations WHERE document_id = ? ORDER BY role, correspondent_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM document_organizations WHERE document_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        let mut deduped: Vec<OrganizationLink> = Vec::new();
        for org in orgs {
            if !deduped.iter().any(|o| o.id == org.id && o.role == org.role) {
                deduped.push(org.clone());
            }
        }

        for org in &deduped {
            sqlx::query(
                "INSERT OR IGNORE INTO document_organizations (document_id, correspondent_id, role, provenance) VALUES (?, ?, ?, 'manual')",
            )
            .bind(id)
            .bind(org.id)
            .bind(org.role)
            .execute(&mut *tx)
            .await?;
        }

        let from_json: Vec<Value> = old_orgs
            .into_iter()
            .map(|(cid, role)| json!({"id": cid, "role": role}))
            .collect();
        let to_json: Vec<Value> = deduped
            .iter()
            .map(|org| json!({"id": org.id, "role": org.role}))
            .collect();
        diff.insert(
            "organizations".into(),
            json!({"from": from_json, "to": to_json}),
        );
    }
    if let Some(summary) = &patch.summary {
        let new = summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        sqlx::query("UPDATE documents SET summary = ?, summary_provenance = ? WHERE id = ?")
            .bind(&new)
            .bind(new.as_ref().map(|_| "manual"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        diff.insert("summary".into(), json!({"from": row.summary, "to": new}));
    }
    if let Some(people) = &patch.people {
        for p in people {
            must_exist(
                &mut tx,
                "SELECT 1 FROM people WHERE id = ?",
                p.to_string(),
                "person_not_found",
                "no such person",
            )
            .await?;
        }
        sqlx::query("DELETE FROM document_people WHERE document_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Remember the choice even when the list is empty, so matching by
        // name never brings back a person the user removed.
        sqlx::query("UPDATE documents SET people_provenance = 'manual' WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for p in people {
            sqlx::query(
                "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) VALUES (?, ?, 'manual')",
            )
            .bind(id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
        }
        diff.insert("people".into(), json!(people));
    }
    if let Some(tags) = &patch.tags {
        for t in tags {
            must_exist(
                &mut tx,
                "SELECT 1 FROM tags WHERE id = ?",
                t.to_string(),
                "tag_not_found",
                "no such tag",
            )
            .await?;
        }
        sqlx::query("DELETE FROM document_tags WHERE document_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for t in tags {
            sqlx::query(
                "INSERT OR IGNORE INTO document_tags (document_id, tag_id, provenance) VALUES (?, ?, 'manual')",
            )
            .bind(id)
            .bind(t)
            .execute(&mut *tx)
            .await?;
        }
        diff.insert("tags".into(), json!(tags));
    }
    if !patch.fields.is_empty() {
        let had_tracked_mrz = !checks::mrz_mismatch_fields(&mut tx, id).await?.is_empty();
        let mut field_diff = BTreeMap::new();
        for (key, value) in &patch.fields {
            let def = effective_defs
                .iter()
                .find(|d| &d.key == key)
                .ok_or_else(|| unknown_field(key))?;
            let old: Option<String> = sqlx::query_scalar(
                "SELECT value FROM field_values WHERE document_id = ? AND field_key = ?",
            )
            .bind(id)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;
            match value {
                Some(v) => {
                    v.validate_against(def)?;
                    sqlx::query(
                        "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_by, updated_at, mrz_mismatch) \
                         VALUES (?, ?, ?, 'manual', NULL, ?, ?, 0) \
                         ON CONFLICT (document_id, field_key) DO UPDATE SET \
                             value = excluded.value, provenance = 'manual', page = NULL, \
                             updated_by = excluded.updated_by, updated_at = excluded.updated_at, \
                             mrz_mismatch = 0",
                    )
                    .bind(id)
                    .bind(key)
                    .bind(json!(v).to_string())
                    .bind(ctx.actor.user_id)
                    .bind(time::now_ts())
                    .execute(&mut *tx)
                    .await?;
                }
                None => {
                    sqlx::query("DELETE FROM field_values WHERE document_id = ? AND field_key = ?")
                        .bind(id)
                        .bind(key)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            let from: Value = old
                .and_then(|o| serde_json::from_str::<FieldValue>(&o).ok())
                .map_or(Value::Null, |v| json!(v));
            field_diff.insert(key.clone(), json!({"from": from, "to": value}));
        }
        diff.insert("fields".into(), json!(field_diff));
        if had_tracked_mrz && checks::mrz_mismatch_fields(&mut tx, id).await?.is_empty() {
            checks::remove_flags(&mut tx, id, &[checks::MRZ_MISMATCH]).await?;
        }
    }
    if !patch.fields.is_empty() || patch.type_id.is_some() {
        // Flags tied to the type's validators or edited fields clear (or appear) at once.
        checks::recompute_in(&mut tx, id, &[]).await?;
    }
    if !patch.dismiss_attention.is_empty() {
        let dismissed = checks::dismiss_in(&mut tx, id, &patch.dismiss_attention).await?;
        if !dismissed.is_empty() {
            diff.insert("dismiss_attention".into(), json!(dismissed));
        }
    }
    if let Some(local_only) = patch.local_only
        && local_only != row.local_only
    {
        if !local_only {
            require_session(ctx)?;
        }
        sqlx::query("UPDATE documents SET local_only = ? WHERE id = ?")
            .bind(local_only)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if local_only {
            drop_llm_data(&mut tx, id).await?;
            sqlx::query(
                "UPDATE documents SET status = 'queued', status_reason = NULL WHERE id = ?",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            ctx.app
                .jobs
                .enqueue(
                    &mut tx,
                    JobKind::Process,
                    id,
                    ProcessOptions {
                        local: true,
                        llm: false,
                        model_override: None,
                    },
                )
                .await?;
            requeue_local = true;
        }
        diff.insert("local_only".into(), json!({"to": local_only}));
    }

    if !diff.is_empty() {
        sqlx::query("UPDATE documents SET updated_at = ?, updated_by = ? WHERE id = ?")
            .bind(time::now_ts())
            .bind(ctx.actor.user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        events::record(
            &mut tx,
            &ctx.actor,
            "document.update",
            Some(id),
            Value::Object(diff),
        )
        .await?;
    }
    tx.commit().await?;
    sync::refresh(ctx.app, id).await;
    if requeue_local {
        ctx.app.jobs.wake();
    }
    get_document(ctx, id, GetOptions::default()).await
}

/// Queues another processing run. Idempotent: a queued run for the same
/// document is reused (see `JobQueue::enqueue`).
pub async fn reprocess_document(ctx: &Ctx<'_>, id: DocumentId, opts: ProcessOptions) -> Result<()> {
    if !opts.local && !opts.llm {
        return Err(ServiceError::invalid(
            "nothing_to_do",
            "choose the local steps, the LLM steps or both",
        ));
    }
    if let Some(ref model) = opts.model_override
        && !ctx.app.config.is_model_allowed(model)
    {
        return Err(ServiceError::invalid(
            "model_not_allowed",
            format!("model {model} is not allowed"),
        ));
    }
    let mut tx = ctx.app.db.write.begin().await?;
    let updated =
        sqlx::query("UPDATE documents SET status = 'queued', status_reason = NULL WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() == 0 {
        return Err(document_not_found());
    }
    ctx.app
        .jobs
        .enqueue(&mut tx, JobKind::Process, id, opts.clone())
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "document.reprocess",
        Some(id),
        json!({"local": opts.local, "llm": opts.llm, "model": opts.model_override}),
    )
    .await?;
    tx.commit().await?;
    ctx.app.jobs.wake();
    Ok(())
}

/// Where the original of a document lives and how to present it.
#[derive(Debug, Clone)]
pub struct OriginalFile {
    pub storage_key: String,
    pub mime: String,
    pub original_name: String,
    pub size: u64,
    /// The document's title, which names the file on its way out.
    pub title: Option<String>,
    /// Size of the clean copy (see [`CLEAN_COPY`]), when there is one.
    pub clean_size: Option<u64>,
}

/// Name of a scanned document's clean copy among its derived files.
pub const CLEAN_COPY: &str = "clean.pdf";

impl OriginalFile {
    /// The original's name on download: the title, with the original's extension.
    pub fn download_name(&self) -> String {
        naming::download_name(
            self.title.as_deref(),
            &self.original_name,
            &naming::extension_for(&self.mime, &self.original_name),
        )
    }
}

pub async fn original_file(ctx: &Ctx<'_>, id: DocumentId) -> Result<OriginalFile> {
    original_file_in(&ctx.app.db.read, id).await
}

/// storage_key, mime, original_name, size, title, clean_size.
type OriginalRow = (String, String, String, i64, Option<String>, Option<i64>);

async fn original_file_in(pool: &sqlx::SqlitePool, id: DocumentId) -> Result<OriginalFile> {
    let row: Option<OriginalRow> = sqlx::query_as(
        "SELECT f.storage_key, f.mime, f.original_name, f.size, d.title, f.clean_size \
         FROM files f JOIN documents d ON d.id = f.document_id WHERE f.document_id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let (storage_key, mime, original_name, size, title, clean_size) =
        row.ok_or_else(document_not_found)?;
    Ok(OriginalFile {
        storage_key,
        mime,
        original_name,
        size: size.max(0) as u64,
        title,
        clean_size: clean_size.map(|s| s.max(0) as u64),
    })
}

/// Where the bytes of a [`DownloadFile`] are.
#[derive(Debug, Clone)]
pub enum DownloadSource {
    /// The original blob, by storage key.
    Original(String),
    /// A derived file of the document.
    Derived(std::path::PathBuf),
}

/// What a person gets when they download a document: the clean copy of a scan
/// when there is one, else the original; either way named after the title.
#[derive(Debug, Clone)]
pub struct DownloadFile {
    pub source: DownloadSource,
    pub mime: String,
    pub size: u64,
    pub filename: String,
    /// True when this is the clean copy, not the original.
    pub clean: bool,
}

impl DownloadFile {
    pub fn open(&self, app: &App) -> std::io::Result<std::fs::File> {
        match &self.source {
            DownloadSource::Original(key) => app.files.open_original(key),
            DownloadSource::Derived(path) => std::fs::File::open(path),
        }
    }
}

impl From<OriginalFile> for DownloadFile {
    fn from(file: OriginalFile) -> Self {
        DownloadFile {
            filename: file.download_name(),
            source: DownloadSource::Original(file.storage_key),
            mime: file.mime,
            size: file.size,
            clean: false,
        }
    }
}

/// The file a download of `id` serves (see [`DownloadFile`]).
pub async fn download_file(ctx: &Ctx<'_>, id: DocumentId) -> Result<DownloadFile> {
    let file = original_file(ctx, id).await?;
    Ok(download_of(ctx.app, id, file))
}

/// The download for `file`, the original of `id`.
pub fn download_of(app: &App, id: DocumentId, file: OriginalFile) -> DownloadFile {
    let clean = app.files.derived_path(id, CLEAN_COPY);
    match file.clean_size {
        Some(size) if clean.is_file() => DownloadFile {
            filename: naming::download_name(file.title.as_deref(), &file.original_name, "pdf"),
            source: DownloadSource::Derived(clean),
            mime: "application/pdf".to_string(),
            size,
            clean: true,
        },
        _ => file.into(),
    }
}

// --- trash, restore, purge ---------------------------------------------------------

async fn set_trashed(ctx: &Ctx<'_>, id: DocumentId, trashed: bool) -> Result<()> {
    let mut tx = ctx.app.db.write.begin().await?;
    let current: Option<(Option<String>,)> =
        sqlx::query_as("SELECT trashed_at FROM documents WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (current,) = current.ok_or_else(document_not_found)?;
    if current.is_some() == trashed {
        return Ok(());
    }
    let now = time::now_ts();
    sqlx::query("UPDATE documents SET trashed_at = ?, updated_at = ?, updated_by = ? WHERE id = ?")
        .bind(trashed.then(|| now.clone()))
        .bind(&now)
        .bind(ctx.actor.user_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let action = if trashed {
        "document.trash"
    } else {
        "document.restore"
    };
    events::record(&mut tx, &ctx.actor, action, Some(id), json!({})).await?;
    tx.commit().await?;
    sync::refresh(ctx.app, id).await;
    Ok(())
}

pub async fn trash_document(ctx: &Ctx<'_>, id: DocumentId) -> Result<()> {
    set_trashed(ctx, id, true).await
}

pub async fn restore_document(ctx: &Ctx<'_>, id: DocumentId) -> Result<()> {
    set_trashed(ctx, id, false).await
}

fn require_session(ctx: &Ctx<'_>) -> Result<()> {
    if ctx.actor.via == Via::Web {
        Ok(())
    } else {
        Err(ServiceError::forbidden(
            "purge_requires_session",
            "permanent deletion is only available in the web interface",
        ))
    }
}

/// Permanently deletes a trashed document and its original. Only a web
/// session may do this; agents and tokens never can.
pub async fn purge_document(ctx: &Ctx<'_>, id: DocumentId) -> Result<()> {
    require_session(ctx)?;
    let app = ctx.app;
    let found: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT f.sha256, f.storage_key, d.trashed_at FROM documents d \
         JOIN files f ON f.document_id = d.id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&app.db.write)
    .await?;
    let (sha256, storage_key, trashed_at) = found.ok_or_else(document_not_found)?;
    if trashed_at.is_none() {
        return Err(ServiceError::invalid(
            "purge_requires_trash",
            "move the document to the trash before deleting it permanently",
        ));
    }

    // Serialized with uploads of the same bytes: the blob is removed only
    // after the rows are gone, and nothing can re-reference it meanwhile.
    let _guard = app.locks.lock(&sha256).await;
    let mut tx = app.db.write.begin().await?;
    let deleted = sqlx::query("DELETE FROM documents WHERE id = ? AND trashed_at IS NOT NULL")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(document_not_found());
    }
    events::record(&mut tx, &ctx.actor, "document.purge", Some(id), json!({})).await?;
    tx.commit().await?;
    sync::forget(app, id);

    app.files.delete_original(&storage_key).await;
    app.files.delete_derived(id).await;
    Ok(())
}

/// Purges every document in the trash (web session only). Returns how many.
pub async fn empty_trash(ctx: &Ctx<'_>) -> Result<u64> {
    require_session(ctx)?;
    let ids: Vec<DocumentId> =
        sqlx::query_scalar("SELECT id FROM documents WHERE trashed_at IS NOT NULL ORDER BY id")
            .fetch_all(&ctx.app.db.write)
            .await?;
    let mut purged = 0;
    for id in ids {
        match purge_document(ctx, id).await {
            Ok(()) => purged += 1,
            // Raced with a restore or another purge: skip it.
            Err(e) if matches!(e.code, "document_not_found" | "purge_requires_trash") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::ids::{CorrespondentId, PersonId, TagId};
    use crate::jobs::queue::JobKind;
    use crate::model::document::{DocumentStatus, Provenance};
    use crate::model::fields::{FieldDef, FieldKind, FieldValue, Labels};
    use crate::model::taxonomy::{NewCorrespondent, NewPerson, NewType};
    use crate::service::testing::test_app;
    use crate::service::upload::{UploadInput, UploadOutcome, upload};
    use crate::service::{Actor, App, Via, taxonomy};
    use chrono::NaiveDate;
    use serde_json::json;

    fn pdf(tag: &str) -> Vec<u8> {
        format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
    }

    fn ctx(app: &App) -> Ctx<'_> {
        app.ctx(Actor::cli())
    }

    fn web(app: &App) -> Ctx<'_> {
        app.ctx(Actor {
            user_id: None,
            via: Via::Web,
        })
    }

    async fn make_doc(app: &App, tag: &str) -> DocumentId {
        match upload(
            &ctx(app),
            UploadInput {
                original_name: format!("{tag}.pdf"),
                ..UploadInput::default()
            },
            &pdf(tag)[..],
        )
        .await
        .unwrap()
        {
            UploadOutcome::Created(id) | UploadOutcome::Restored(id) => id,
        }
    }

    fn field(key: &str, kind: FieldKind) -> FieldDef {
        FieldDef {
            key: key.into(),
            labels: Labels::from([("en".into(), key.into())]),
            kind,
            validator: None,
            required: false,
            issuer: false,
        }
    }

    async fn passport_type(app: &App) -> crate::ids::TypeId {
        taxonomy::create_type(
            &ctx(app),
            NewType {
                key: "passport".into(),
                labels: Labels::from([("en".into(), "Passport".into())]),
                fields: vec![
                    field("number", FieldKind::Identifier),
                    field("expiry_date", FieldKind::Date),
                    field("total", FieldKind::Money),
                ],
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn scalar<T>(app: &App, sql: &'static str, id: DocumentId) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
    {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap()
    }

    // --- get ---------------------------------------------------------------

    #[tokio::test]
    async fn get_document_returns_metadata_and_file() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "get").await;
        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        assert_eq!(view.id, id);
        assert_eq!(view.status, DocumentStatus::Queued);
        assert_eq!(view.file.mime, "application/pdf");
        assert_eq!(view.file.original_name, "get.pdf");
        assert_eq!(view.file.size as usize, pdf("get").len());
        assert!(view.people.is_empty() && view.tags.is_empty() && view.fields.is_empty());
        assert!(view.attention.is_empty());
        assert!(view.text.is_none());
        assert!(!view.local_only);
    }

    #[tokio::test]
    async fn get_missing_document_is_not_found() {
        let (_dir, app) = test_app().await;
        let err = get_document(&ctx(&app), DocumentId::new(), GetOptions::default())
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::NotFound, "document_not_found")
        );
    }

    #[tokio::test]
    async fn text_page_shows_the_best_layer() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "text").await;
        sqlx::query("UPDATE files SET page_count = 4 WHERE document_id = ?")
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        // (page, source, text, quality)
        for (page, source, text, quality) in [
            (1, "embedded", "exact embedded", 0.9),
            (1, "llm", "llm version", 0.5),
            (1, "tesseract", "ocr version", 0.5),
            (2, "tesseract", "ocr page two", 0.7),
            (2, "llm", "llm page two", 0.5),
            (3, "tesseract", "ocr only", 0.7),
            (4, "embedded", "garbled \u{FFFD}\u{FFFD}", 0.1),
            (4, "tesseract", "ocr page four", 0.7),
        ] {
            sqlx::query(
                "INSERT INTO text_layers (document_id, page, source, text, quality_score, created_at) \
                 VALUES (?, ?, ?, ?, ?, '2026-01-01T00:00:00.000Z')",
            )
            .bind(id)
            .bind(page)
            .bind(source)
            .bind(text)
            .bind(quality)
            .execute(&app.db.write)
            .await
            .unwrap();
        }
        let text_of = |page: u32| {
            let app = &app;
            async move {
                get_document(
                    &ctx(app),
                    id,
                    GetOptions {
                        text_page: Some(page),
                    },
                )
                .await
                .unwrap()
                .text
                .unwrap()
            }
        };
        // Good embedded text is exact: it wins.
        let t = text_of(1).await;
        assert_eq!(
            (t.source.as_str(), t.text.as_str(), t.page_count),
            ("embedded", "exact embedded", 4)
        );
        // Otherwise llm > tesseract.
        assert_eq!(text_of(2).await.source, "llm");
        assert_eq!(text_of(3).await.source, "tesseract");
        // Poor embedded text does not beat OCR.
        assert_eq!(text_of(4).await.text, "ocr page four");

        let err = get_document(&ctx(&app), id, GetOptions { text_page: Some(5) })
            .await
            .unwrap_err();
        assert_eq!(err.code, "page_out_of_range");
        let err = get_document(&ctx(&app), id, GetOptions { text_page: Some(0) })
            .await
            .unwrap_err();
        assert_eq!(err.code, "page_out_of_range");
    }

    #[tokio::test]
    async fn text_page_before_processing_is_empty() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "pending").await;
        let t = get_document(&ctx(&app), id, GetOptions { text_page: Some(1) })
            .await
            .unwrap()
            .text
            .unwrap();
        assert_eq!((t.text.as_str(), t.source.as_str()), ("", "none"));
    }

    // --- patch -------------------------------------------------------------

    #[tokio::test]
    async fn manual_patch_sets_manual_provenance() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "patch").await;
        let ty = passport_type(&app).await;
        let person = taxonomy::create_person(
            &ctx(&app),
            NewPerson {
                name: "Marik".into(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap();
        let tag = taxonomy::create_tag(&ctx(&app), "id", None).await.unwrap();
        let corr = taxonomy::create_correspondent(
            &ctx(&app),
            NewCorrespondent {
                name: "Registry".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let expiry = NaiveDate::from_ymd_opt(2031, 5, 6).unwrap();

        let patch: DocumentPatch = serde_json::from_value(json!({
            "title": "  My passport ",
            "document_date": "2021-05-06",
            "type_id": ty,
            "organizations": [{"id": corr.id, "role": "issued_by"}],
            "summary": "Travel document",
            "people": [person.id],
            "tags": [tag.id],
            "fields": {
                "number": {"kind": "identifier", "value": "AB1234567"},
                "expiry_date": {"kind": "date", "value": "2031-05-06"}
            }
        }))
        .unwrap();
        let view = update_document(&ctx(&app), id, patch).await.unwrap();

        assert_eq!(view.title.as_deref(), Some("My passport"));
        assert_eq!(view.title_provenance, Some(Provenance::Manual));
        assert_eq!(view.document_date, NaiveDate::from_ymd_opt(2021, 5, 6));
        assert_eq!(view.type_key.as_deref(), Some("passport"));
        assert_eq!(view.type_provenance, Some(Provenance::Manual));
        assert_eq!(view.organizations.len(), 1);
        assert_eq!(view.organizations[0].id, corr.id);
        assert_eq!(view.organizations[0].name, "Registry");
        assert_eq!(view.organizations[0].role, Role::IssuedBy);
        assert_eq!(view.organizations[0].provenance, Some(Provenance::Manual));
        assert_eq!(view.summary_provenance, Some(Provenance::Manual));
        assert_eq!(view.people.len(), 1);
        assert_eq!(view.people[0].provenance, Provenance::Manual);
        assert_eq!(view.tags[0].provenance, Provenance::Manual);
        assert_eq!(view.fields.len(), 2);
        assert!(
            view.fields
                .iter()
                .all(|f| f.provenance == Provenance::Manual)
        );
        let number = view.fields.iter().find(|f| f.key == "number").unwrap();
        assert_eq!(number.value, FieldValue::Identifier("AB1234567".into()));
        let exp = view.fields.iter().find(|f| f.key == "expiry_date").unwrap();
        assert_eq!(exp.value, FieldValue::Date(expiry));

        // Audited with before/after for the edited field.
        let diff: String = sqlx::query_scalar(
            "SELECT diff FROM events WHERE action = 'document.update' AND document_id = ?",
        )
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        let diff: serde_json::Value = serde_json::from_str(&diff).unwrap();
        assert_eq!(diff["fields"]["number"]["to"]["value"], "AB1234567");
        assert!(diff["fields"]["number"]["from"].is_null());
    }

    #[tokio::test]
    async fn patch_null_clears_and_absent_leaves() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "clear").await;
        let ty = passport_type(&app).await;
        let set: DocumentPatch = serde_json::from_value(json!({
            "title": "T", "summary": "S", "type_id": ty,
            "fields": {"number": {"kind": "identifier", "value": "X1"}}
        }))
        .unwrap();
        update_document(&ctx(&app), id, set).await.unwrap();

        // Clear the title and the field; leave summary and type alone.
        let clear: DocumentPatch =
            serde_json::from_value(json!({"title": null, "fields": {"number": null}})).unwrap();
        let view = update_document(&ctx(&app), id, clear).await.unwrap();
        assert_eq!(view.title, None);
        assert_eq!(
            view.title_provenance, None,
            "a cleared value can be refilled by reprocessing"
        );
        assert!(view.fields.is_empty());
        assert_eq!(view.summary.as_deref(), Some("S"));
        assert_eq!(view.type_key.as_deref(), Some("passport"));

        // An empty patch is a no-op.
        let before = view.clone();
        let after = update_document(&ctx(&app), id, DocumentPatch::default())
            .await
            .unwrap();
        assert_eq!(before.title, after.title);
        assert_eq!(before.summary, after.summary);
        assert_eq!(before.fields, after.fields);
    }

    #[tokio::test]
    async fn field_issues_name_the_value_and_saving_it_by_hand_clears_them() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "issues").await;
        let ty = passport_type(&app).await;
        let typed: DocumentPatch = serde_json::from_value(json!({"type_id": ty})).unwrap();
        update_document(&ctx(&app), id, typed).await.unwrap();
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_at)              VALUES (?, 'number', '{\"kind\":\"identifier\",\"value\":\"AB 1234567\"}', 'llm', 1, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, created_at)              VALUES (?, 1, 'tesseract', 'No AB 1234561', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();

        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        let number = view.fields.iter().find(|f| f.key == "number").unwrap();
        assert_eq!(number.issues, vec![checks::OCR_LLM_DISAGREE.to_string()]);

        // Accepting the value as it is: the same value saved by hand.
        let accept: DocumentPatch = serde_json::from_value(
            json!({"fields": {"number": {"kind": "identifier", "value": "AB 1234567"}}}),
        )
        .unwrap();
        let view = update_document(&ctx(&app), id, accept).await.unwrap();
        let number = view.fields.iter().find(|f| f.key == "number").unwrap();
        assert!(number.issues.is_empty());
        assert!(!view.attention.iter().any(|f| f == checks::OCR_LLM_DISAGREE));
    }

    #[tokio::test]
    async fn confirming_mrz_mismatch_field_or_dismissing_attention_clears_flag() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "mrz_dismiss").await;
        let ty = passport_type(&app).await;
        let typed: DocumentPatch = serde_json::from_value(json!({"type_id": ty})).unwrap();
        update_document(&ctx(&app), id, typed).await.unwrap();

        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_at, mrz_mismatch) \
             VALUES (?, 'number', '{\"kind\":\"identifier\",\"value\":\"AB123456\"}', 'llm', 1, '2026-01-01T00:00:00Z', 1)",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
        {
            let mut conn = app.db.write.acquire().await.unwrap();
            checks::recompute_in(&mut conn, id, &[]).await.unwrap();
        }

        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        assert_eq!(view.attention, vec![checks::MRZ_MISMATCH]);
        let number = view.fields.iter().find(|f| f.key == "number").unwrap();
        assert_eq!(number.issues, vec![checks::MRZ_MISMATCH]);

        // Saving the field manually clears both the field issue and the attention flag.
        let confirm: DocumentPatch = serde_json::from_value(
            json!({"fields": {"number": {"kind": "identifier", "value": "AB123456"}}}),
        )
        .unwrap();
        let view = update_document(&ctx(&app), id, confirm).await.unwrap();
        assert!(view.attention.is_empty());
        assert!(
            view.fields
                .iter()
                .find(|f| f.key == "number")
                .unwrap()
                .issues
                .is_empty()
        );

        // Legacy carried mrz_mismatch without per-field marker can still be dismissed.
        {
            let mut conn = app.db.write.acquire().await.unwrap();
            checks::add_flag(&mut conn, id, checks::MRZ_MISMATCH)
                .await
                .unwrap();
        }
        let dismiss: DocumentPatch =
            serde_json::from_value(json!({"dismiss_attention": ["mrz_mismatch"]})).unwrap();
        let view = update_document(&ctx(&app), id, dismiss).await.unwrap();
        assert!(view.attention.is_empty());

        let invalid: DocumentPatch =
            serde_json::from_value(json!({"dismiss_attention": ["bogus_flag"]})).unwrap();
        let err = update_document(&ctx(&app), id, invalid).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Invalid, "invalid_attention_flag")
        );
    }

    #[tokio::test]
    async fn patch_validates_input() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "validate").await;
        let ty = passport_type(&app).await;
        let patch = |v: serde_json::Value| serde_json::from_value::<DocumentPatch>(v).unwrap();

        // A field needs a type that defines it.
        let err = update_document(
            &ctx(&app),
            id,
            patch(json!({"fields": {"number": {"kind": "identifier", "value": "X"}}})),
        )
        .await
        .unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Invalid, "unknown_field"));

        update_document(&ctx(&app), id, patch(json!({"type_id": ty})))
            .await
            .unwrap();
        let err = update_document(
            &ctx(&app),
            id,
            patch(json!({"fields": {"no_such": {"kind": "text", "value": "X"}}})),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "unknown_field");
        let err = update_document(
            &ctx(&app),
            id,
            patch(json!({"fields": {"expiry_date": {"kind": "text", "value": "tomorrow"}}})),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "field_kind_mismatch");

        let err = update_document(&ctx(&app), id, patch(json!({"title": "   "})))
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_title");
        for (key, value) in [
            ("people", json!([PersonId::new()])),
            ("tags", json!([TagId::new()])),
        ] {
            let err = update_document(&ctx(&app), id, patch(json!({ key: value })))
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::NotFound, "{key}");
        }
        let err = update_document(
            &ctx(&app),
            id,
            patch(json!({"organizations": [{"id": CorrespondentId::new()}]})),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        assert_eq!(err.code, "correspondent_not_found");
        let err = update_document(&ctx(&app), DocumentId::new(), DocumentPatch::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, "document_not_found");

        // Nothing above changed the document.
        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        assert!(view.fields.is_empty() && view.people.is_empty() && view.tags.is_empty());
    }

    #[tokio::test]
    async fn organizations_patch_semantics_and_ordering() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "orgs").await;
        let c1 = taxonomy::create_correspondent(
            &ctx(&app),
            NewCorrespondent {
                name: "Bank A".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let c2 = taxonomy::create_correspondent(
            &ctx(&app),
            NewCorrespondent {
                name: "Ministry B".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();

        // Patch with two organizations, different roles.
        // Ministry B is issued_by, Bank A is party.
        let p: DocumentPatch = serde_json::from_value(json!({
            "organizations": [
                {"id": c1.id, "role": "party"},
                {"id": c2.id, "role": "issued_by"}
            ]
        }))
        .unwrap();
        let view = update_document(&ctx(&app), id, p).await.unwrap();
        assert_eq!(view.organizations.len(), 2);
        // Ordered by role: issued_by (c2) before party (c1)
        assert_eq!(view.organizations[0].id, c2.id);
        assert_eq!(view.organizations[0].role, Role::IssuedBy);
        assert_eq!(view.organizations[0].provenance, Some(Provenance::Manual));
        assert_eq!(view.organizations[1].id, c1.id);
        assert_eq!(view.organizations[1].role, Role::Party);
        assert_eq!(view.organizations[1].provenance, Some(Provenance::Manual));

        // Unknown id is rejected
        let unknown = CorrespondentId::new();
        let bad_patch: DocumentPatch = serde_json::from_value(json!({
            "organizations": [{"id": unknown}]
        }))
        .unwrap();
        let err = update_document(&ctx(&app), id, bad_patch)
            .await
            .unwrap_err();
        assert_eq!(err.code, "correspondent_not_found");

        // Duplicate (id, role) pairs are de-duplicated; missing role defaults to unspecified
        let dup_patch: DocumentPatch = serde_json::from_value(json!({
            "organizations": [
                {"id": c1.id},
                {"id": c1.id, "role": "unspecified"},
                {"id": c1.id, "role": "party"}
            ]
        }))
        .unwrap();
        let view = update_document(&ctx(&app), id, dup_patch).await.unwrap();
        assert_eq!(view.organizations.len(), 2);
        // party before unspecified
        assert_eq!(view.organizations[0].role, Role::Party);
        assert_eq!(view.organizations[1].role, Role::Unspecified);

        // Empty list clears
        let empty_patch: DocumentPatch = serde_json::from_value(json!({
            "organizations": []
        }))
        .unwrap();
        let view = update_document(&ctx(&app), id, empty_patch).await.unwrap();
        assert!(view.organizations.is_empty());

        // Verify audit event has "organizations" diff key with from/to
        let diffs: Vec<(String,)> = sqlx::query_as(
            "SELECT diff FROM events WHERE document_id = ? AND action = 'document.update' ORDER BY at DESC",
        )
        .bind(id)
        .fetch_all(&app.db.read)
        .await
        .unwrap();
        assert!(!diffs.is_empty());
        let last_diff: serde_json::Value = serde_json::from_str(&diffs[0].0).unwrap();
        assert!(last_diff.get("organizations").is_some());
    }

    #[tokio::test]
    async fn enabling_local_only_drops_llm_data() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "local").await;
        let ty = passport_type(&app).await;
        let llm_person = taxonomy::create_person(
            &ctx(&app),
            NewPerson {
                name: "L".into(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap();
        let manual_person = taxonomy::create_person(
            &ctx(&app),
            NewPerson {
                name: "M".into(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap();
        let llm_tag = taxonomy::create_tag(&ctx(&app), "llm-tag", None)
            .await
            .unwrap();
        let manual_tag = taxonomy::create_tag(&ctx(&app), "manual-tag", None)
            .await
            .unwrap();

        // Pretend processing ran: some values from the LLM, some from other sources.
        let now = "'2026-01-01T00:00:00.000Z'";
        for sql in [
            format!(
                "UPDATE documents SET status = 'ready', type_id = '{ty}', type_provenance = 'llm', title = 'LLM title', title_provenance = 'llm', summary = 'LLM summary', summary_provenance = 'llm', document_date = '2020-01-01', date_provenance = 'mrz' WHERE id = '{id}'"
            ),
            format!(
                "INSERT INTO text_layers (document_id, page, source, text, created_at) VALUES ('{id}', 1, 'llm', 'llm text', {now})"
            ),
            format!(
                "INSERT INTO text_layers (document_id, page, source, text, created_at) VALUES ('{id}', 1, 'tesseract', 'ocr text', {now})"
            ),
            format!(
                "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) VALUES ('{id}', 'number', '{{\"kind\":\"identifier\",\"value\":\"A1\"}}', 'llm', {now})"
            ),
            format!(
                "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) VALUES ('{id}', 'expiry_date', '{{\"kind\":\"date\",\"value\":\"2030-01-01\"}}', 'mrz', {now})"
            ),
            format!(
                "INSERT INTO facts (document_id, key, value, provenance) VALUES ('{id}', 'k', 'from llm', 'llm')"
            ),
            format!(
                "INSERT INTO facts (document_id, key, value, provenance) VALUES ('{id}', 'k2', 'by hand', 'manual')"
            ),
            format!(
                "INSERT INTO document_people (document_id, person_id, provenance) VALUES ('{id}', '{llm_person_id}', 'llm')",
                llm_person_id = llm_person.id
            ),
            format!(
                "INSERT INTO document_people (document_id, person_id, provenance) VALUES ('{id}', '{manual_person_id}', 'manual')",
                manual_person_id = manual_person.id
            ),
            format!(
                "INSERT INTO document_tags (document_id, tag_id, provenance) VALUES ('{id}', '{llm_tag_id}', 'llm')",
                llm_tag_id = llm_tag.id
            ),
            format!(
                "INSERT INTO document_tags (document_id, tag_id, provenance) VALUES ('{id}', '{manual_tag_id}', 'manual')",
                manual_tag_id = manual_tag.id
            ),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&app.db.write)
                .await
                .unwrap();
        }
        // The upload job was already consumed by a worker.
        sqlx::query("DELETE FROM jobs")
            .execute(&app.db.write)
            .await
            .unwrap();

        let view = update_document(
            &ctx(&app),
            id,
            serde_json::from_value(json!({"local_only": true})).unwrap(),
        )
        .await
        .unwrap();

        assert!(view.local_only);
        // LLM-derived values are gone...
        assert_eq!(view.title, None);
        assert_eq!(view.summary, None);
        assert_eq!(view.type_key, None);
        assert!(view.facts.iter().all(|f| f.provenance != Provenance::Llm));
        assert_eq!(view.facts.len(), 1);
        assert_eq!(
            view.people.iter().map(|p| p.id).collect::<Vec<_>>(),
            [manual_person.id]
        );
        assert_eq!(
            view.tags.iter().map(|t| t.id).collect::<Vec<_>>(),
            [manual_tag.id]
        );
        assert_eq!(view.fields.len(), 1);
        assert_eq!(view.fields[0].key, "expiry_date");
        let sources: Vec<String> =
            sqlx::query_scalar("SELECT source FROM text_layers WHERE document_id = ?")
                .bind(id)
                .fetch_all(&app.db.read)
                .await
                .unwrap();
        assert_eq!(sources, ["tesseract"]);
        // ...other sources stay, and a local-only reprocess is queued.
        assert_eq!(view.document_date, NaiveDate::from_ymd_opt(2020, 1, 1));
        assert_eq!(view.status, DocumentStatus::Queued);
        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        assert_eq!(job.document_id, Some(id));
        let opts = job.process_options();
        assert!(opts.local && !opts.llm);

        // Turning it off again does not re-run anything by itself.
        sqlx::query("DELETE FROM jobs")
            .execute(&app.db.write)
            .await
            .unwrap();
        let view = update_document(
            &web(&app),
            id,
            serde_json::from_value(json!({"local_only": false})).unwrap(),
        )
        .await
        .unwrap();
        assert!(!view.local_only);
        assert!(app.jobs.claim(&[JobKind::Process]).await.unwrap().is_none());
    }

    // --- trash / restore / purge --------------------------------------------

    #[tokio::test]
    async fn trash_and_restore_are_reversible() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "trash").await;
        trash_document(&ctx(&app), id).await.unwrap();
        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        assert!(view.trashed_at.is_some());
        // Trashing twice is harmless.
        trash_document(&ctx(&app), id).await.unwrap();
        restore_document(&ctx(&app), id).await.unwrap();
        let view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        assert!(view.trashed_at.is_none());
        assert_eq!(
            trash_document(&ctx(&app), DocumentId::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::NotFound
        );
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM events WHERE document_id = ? AND action LIKE 'document.%' ORDER BY id",
        )
        .bind(id)
        .fetch_all(&app.db.read)
        .await
        .unwrap();
        assert!(actions.contains(&"document.trash".to_string()));
        assert!(actions.contains(&"document.restore".to_string()));
    }

    #[tokio::test]
    async fn purge_via_token_is_forbidden() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "forbid").await;
        trash_document(&ctx(&app), id).await.unwrap();
        for via in [Via::Token, Via::OAuth, Via::Cli, Via::System] {
            let c = app.ctx(Actor { user_id: None, via });
            let err = purge_document(&c, id).await.unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Forbidden, "purge_requires_session"),
                "{via:?}"
            );
            assert_eq!(
                empty_trash(&c).await.unwrap_err().code,
                "purge_requires_session"
            );
        }
        // Nothing was deleted.
        assert!(
            get_document(&ctx(&app), id, GetOptions::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn local_only_permissions() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "local_only_test").await;

        let token_ctx = app.ctx(Actor {
            user_id: None,
            via: Via::Token,
        });
        let updated = update_document(
            &token_ctx,
            id,
            DocumentPatch {
                local_only: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(updated.local_only);

        let err = update_document(
            &token_ctx,
            id,
            DocumentPatch {
                local_only: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Forbidden, "purge_requires_session")
        );

        let mcp_ctx = app.ctx(Actor {
            user_id: None,
            via: Via::OAuth,
        });
        let err = update_document(
            &mcp_ctx,
            id,
            DocumentPatch {
                local_only: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Forbidden, "purge_requires_session")
        );

        let session_ctx = app.ctx(Actor {
            user_id: None,
            via: Via::Web,
        });
        let updated = update_document(
            &session_ctx,
            id,
            DocumentPatch {
                local_only: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(!updated.local_only);

        let updated = update_document(
            &session_ctx,
            id,
            DocumentPatch {
                local_only: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(updated.local_only);
    }

    #[tokio::test]
    async fn reprocess_model_whitelist() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "reprocess_model").await;
        let c = ctx(&app);

        let res = reprocess_document(
            &c,
            id,
            ProcessOptions {
                local: true,
                llm: true,
                model_override: Some("anthropic/claude-haiku-5.5".to_string()),
            },
        )
        .await;
        assert!(res.is_ok());

        let err = reprocess_document(
            &c,
            id,
            ProcessOptions {
                local: true,
                llm: true,
                model_override: Some("unauthorized/evil-model".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Invalid);
        assert_eq!(err.code, "model_not_allowed");
    }

    #[tokio::test]
    async fn purge_requires_the_document_to_be_in_the_trash() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "alive").await;
        let err = purge_document(&web(&app), id).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Invalid, "purge_requires_trash")
        );
        let err = purge_document(&web(&app), DocumentId::new())
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn purge_then_reupload_creates_new_document() {
        let (dir, app) = test_app().await;
        let id = make_doc(&app, "purge").await;
        let key: String = scalar(
            &app,
            "SELECT storage_key FROM files WHERE document_id = ?",
            id,
        )
        .await;
        // Derived files and dependent rows exist.
        let derived = app.files.derived_path(id, "thumb.webp");
        std::fs::create_dir_all(derived.parent().unwrap()).unwrap();
        std::fs::write(&derived, b"thumb").unwrap();
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, created_at) \
             VALUES (?, 1, 'tesseract', 'secret text', '2026-01-01T00:00:00.000Z')",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
        trash_document(&ctx(&app), id).await.unwrap();

        purge_document(&web(&app), id).await.unwrap();

        assert_eq!(
            get_document(&ctx(&app), id, GetOptions::default())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::NotFound
        );
        assert!(app.files.open_original(&key).is_err(), "the blob is gone");
        assert!(!derived.exists());
        for table in ["files", "text_layers", "jobs"] {
            let n: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                    .fetch_one(&app.db.read)
                    .await
                    .unwrap();
            assert_eq!(n, 0, "{table} should be empty");
        }
        // The audit trail survives the purge.
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM events WHERE document_id = ? AND action = 'document.purge'",
        )
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        assert_eq!(n, 1);
        assert!(
            std::fs::read_dir(dir.path().join("originals"))
                .unwrap()
                .all(|e| {
                    std::fs::read_dir(e.unwrap().path())
                        .unwrap()
                        .next()
                        .is_none()
                })
        );

        // The same bytes are a brand new document afterwards.
        let again = upload(
            &ctx(&app),
            UploadInput {
                original_name: "purge.pdf".into(),
                ..UploadInput::default()
            },
            &pdf("purge")[..],
        )
        .await
        .unwrap();
        let UploadOutcome::Created(new_id) = again else {
            panic!("expected Created, got {again:?}")
        };
        assert_ne!(new_id, id);
        assert!(
            get_document(&ctx(&app), new_id, GetOptions::default())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn empty_trash_purges_only_trashed_documents() {
        let (_dir, app) = test_app().await;
        let keep = make_doc(&app, "keep").await;
        let a = make_doc(&app, "a").await;
        let b = make_doc(&app, "b").await;
        trash_document(&ctx(&app), a).await.unwrap();
        trash_document(&ctx(&app), b).await.unwrap();
        assert_eq!(empty_trash(&web(&app)).await.unwrap(), 2);
        assert!(
            get_document(&ctx(&app), keep, GetOptions::default())
                .await
                .is_ok()
        );
        assert!(
            get_document(&ctx(&app), a, GetOptions::default())
                .await
                .is_err()
        );
        assert_eq!(empty_trash(&web(&app)).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn ready_clean_doc_skips_unflagged_field_checks_and_type_patch_recomputes() {
        let (_dir, app) = test_app().await;
        let id = make_doc(&app, "clean_checks").await;
        let passport = passport_type(&app).await;
        update_document(
            &ctx(&app),
            id,
            serde_json::from_value(json!({"type_id": passport})).unwrap(),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_at) \
             VALUES (?, 'number', '{\"kind\":\"identifier\",\"value\":\"AB 1234567\"}', 'llm', 1, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, created_at) \
             VALUES (?, 1, 'tesseract', 'No AB 1234561', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();

        // A ready document with no recorded attention skips the read-time OCR scan.
        sqlx::query("UPDATE documents SET status = 'ready', attention = '[]' WHERE id = ?")
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        let clean_view = get_document(&ctx(&app), id, GetOptions::default())
            .await
            .unwrap();
        let number = clean_view
            .fields
            .iter()
            .find(|f| f.key == "number")
            .unwrap();
        assert!(number.issues.is_empty(), "{:?}", number.issues);

        // Switching the document type recomputes attention without editing any fields.
        let flagged_view = update_document(
            &ctx(&app),
            id,
            serde_json::from_value(json!({"type_id": null})).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            flagged_view
                .attention
                .iter()
                .any(|f| f == checks::OCR_LLM_DISAGREE),
            "{:?}",
            flagged_view.attention
        );
        let number = flagged_view
            .fields
            .iter()
            .find(|f| f.key == "number")
            .unwrap();
        assert_eq!(number.issues, vec![checks::OCR_LLM_DISAGREE.to_string()]);
    }
}
