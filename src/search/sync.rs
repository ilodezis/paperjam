//! Keeps the derived search index in step with SQLite.
//!
//! Services call [`refresh`] after committing a change. A failure to index is
//! logged, never returned: the index is rebuildable and the user's change
//! already succeeded (`paperjam reindex` repairs a stale index).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::SqliteConnection;

use super::{IndexDoc, SearchPage, SearchRequest, internal, type_names};
use crate::error::Result;
use crate::files::FileStore;
use crate::ids::{CorrespondentId, DocumentId, PersonId, TagId, TypeId};
use crate::model::document::Role;
use crate::model::fields::{FieldDef, FieldKind, FieldValue};
use crate::service::documents::{Layer, best_layer};
use crate::service::{App, Ctx};

/// Taxonomy entities whose edits change how their documents are indexed.
#[derive(Debug, Clone, Copy)]
pub enum Related {
    Person(PersonId),
    Correspondent(CorrespondentId),
    Tag(TagId),
    Type(TypeId),
}

#[derive(sqlx::FromRow)]
struct Row {
    title: Option<String>,
    original_name: String,
    document_date: Option<String>,
    summary: Option<String>,
    type_key: Option<String>,
    type_labels: Option<String>,
    type_fields: Option<String>,
    type_summary_fields: Option<String>,
    local_only: bool,
    attention: String,
    trashed_at: Option<String>,
    created_at: String,
}

fn names_with_aliases(name: String, aliases: Option<&str>) -> Vec<String> {
    let mut all = vec![name];
    all.extend(
        aliases
            .and_then(|a| serde_json::from_str::<Vec<String>>(a).ok())
            .unwrap_or_default(),
    );
    all
}

/// Builds the index entry of one document, `None` if it does not exist.
pub async fn build_index_doc(
    conn: &mut SqliteConnection,
    files: &FileStore,
    id: DocumentId,
) -> Result<Option<IndexDoc>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT d.title, f.original_name, d.document_date, d.summary, t.key AS type_key, \
         t.labels AS type_labels, t.fields AS type_fields, \
         t.summary_fields AS type_summary_fields, d.local_only, d.attention, d.trashed_at, d.created_at \
         FROM documents d JOIN files f ON f.document_id = d.id \
         LEFT JOIN document_types t ON t.id = d.type_id \
         WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };

    let people: Vec<(PersonId, String, String)> = sqlx::query_as(
        "SELECT p.id, p.name, p.aliases FROM document_people dp \
         JOIN people p ON p.id = dp.person_id WHERE dp.document_id = ? ORDER BY p.name, p.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM document_tags dt JOIN tags t ON t.id = dt.tag_id \
         WHERE dt.document_id = ? ORDER BY t.name",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let field_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT field_key, value FROM field_values WHERE document_id = ?")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    let layers: Vec<(i64, String, String, Option<f64>)> = sqlx::query_as(
        "SELECT page, source, text, quality_score FROM text_layers \
         WHERE document_id = ? ORDER BY page, source",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;

    // The shown layer of each page goes to `texts`; every other layer is
    // searchable too (spec §6) but never displayed.
    let mut texts = Vec::new();
    let mut other_texts = Vec::new();
    let mut by_page: BTreeMap<i64, Vec<Layer>> = BTreeMap::new();
    for (page, source, text, quality) in layers {
        by_page
            .entry(page)
            .or_default()
            .push((source, text, quality));
    }
    for page_layers in by_page.values() {
        let shown = best_layer(page_layers);
        if let Some(shown) = shown {
            texts.push(shown.1.clone());
        }
        for layer in page_layers {
            if !shown.is_some_and(|s| std::ptr::eq(s, layer)) {
                other_texts.push(layer.1.clone());
            }
        }
    }

    let fields: BTreeMap<String, FieldValue> = field_rows
        .into_iter()
        .filter_map(|(key, value)| Some((key, serde_json::from_str(&value).ok()?)))
        .collect();
    let field_order = row
        .type_fields
        .as_deref()
        .and_then(|f| serde_json::from_str::<Vec<FieldDef>>(f).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.key)
        .collect();

    let summary_fields = row
        .type_summary_fields
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let type_names = row.type_key.as_deref().map_or_else(Vec::new, |key| {
        let labels = row
            .type_labels
            .as_deref()
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or_default();
        type_names::names(key, &labels)
    });

    let org_rows: Vec<(CorrespondentId, String, Option<String>, Role)> = sqlx::query_as(
        "SELECT do_org.correspondent_id, c.name, c.aliases, do_org.role \
         FROM document_organizations do_org \
         JOIN correspondents c ON c.id = do_org.correspondent_id \
         WHERE do_org.document_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;

    let mut orgs = org_rows;
    orgs.sort_by(|a, b| (a.3, &a.1).cmp(&(b.3, &b.1)));

    let mut correspondent_ids = Vec::new();
    let mut correspondent = Vec::new();
    let mut org_names = Vec::new();
    for (cid, name, aliases, _) in orgs {
        if !correspondent_ids.contains(&cid) {
            correspondent_ids.push(cid);
        }
        if !org_names.contains(&name) {
            org_names.push(name.clone());
        }
        for alias in names_with_aliases(name, aliases.as_deref()) {
            if !correspondent.contains(&alias) {
                correspondent.push(alias);
            }
        }
    }

    Ok(Some(IndexDoc {
        id,
        title: row.title.unwrap_or_else(|| {
            row.original_name
                .rsplit_once('.')
                .map_or(row.original_name.clone(), |(stem, _)| stem.to_string())
        }),
        texts,
        other_texts,
        summary: row.summary,
        type_key: row.type_key,
        correspondent_ids,
        correspondent,
        organization_names: org_names,
        people: people
            .into_iter()
            .map(|(pid, name, aliases)| (pid, names_with_aliases(name, Some(&aliases))))
            .collect(),
        tags,
        fields,
        field_order,
        summary_fields,
        type_names,
        document_date: row
            .document_date
            .as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        added_at: DateTime::parse_from_rfc3339(&row.created_at)
            .map_or_else(|_| Utc::now(), |d| d.with_timezone(&Utc)),
        local_only: row.local_only,
        attention: serde_json::from_str(&row.attention).unwrap_or_default(),
        trashed: row.trashed_at.is_some(),
        has_pages: files.has_page_images(id).await,
    }))
}

/// Re-indexes one document from the database (removes it if it is gone).
pub async fn index_one(app: &App, id: DocumentId) -> Result<()> {
    let mut conn = app.db.read.acquire().await?;
    match build_index_doc(&mut conn, &app.files, id).await? {
        Some(doc) => app.index.index_document(&doc),
        None => app.index.delete(id),
    }
}

/// [`index_one`] for services: failures are logged, not returned.
pub async fn refresh(app: &App, id: DocumentId) {
    if let Err(e) = index_one(app, id).await {
        tracing::error!(error = %e, "could not update the search index");
    }
}

pub async fn refresh_many(app: &App, ids: &[DocumentId]) {
    for id in ids {
        refresh(app, *id).await;
    }
}

/// Removes a purged document from the index.
pub fn forget(app: &App, id: DocumentId) {
    if let Err(e) = app.index.delete(id) {
        tracing::error!(error = %e, "could not remove a document from the search index");
    }
}

/// Documents affected by an edit of a taxonomy entity.
pub async fn documents_of(app: &App, related: Related) -> Result<Vec<DocumentId>> {
    let ids = match related {
        Related::Person(id) => {
            sqlx::query_scalar(
                "SELECT DISTINCT document_id FROM document_people WHERE person_id = ?",
            )
            .bind(id)
            .fetch_all(&app.db.read)
            .await?
        }
        Related::Correspondent(id) => sqlx::query_scalar(
            "SELECT DISTINCT document_id FROM document_organizations WHERE correspondent_id = ?",
        )
        .bind(id)
        .fetch_all(&app.db.read)
        .await?,
        Related::Tag(id) => {
            sqlx::query_scalar("SELECT DISTINCT document_id FROM document_tags WHERE tag_id = ?")
                .bind(id)
                .fetch_all(&app.db.read)
                .await?
        }
        Related::Type(id) => {
            sqlx::query_scalar("SELECT id FROM documents WHERE type_id = ?")
                .bind(id)
                .fetch_all(&app.db.read)
                .await?
        }
    };
    Ok(ids)
}

/// Re-indexes every document touched by `related`.
pub async fn refresh_related(app: &App, related: Related) {
    match documents_of(app, related).await {
        Ok(ids) => refresh_many(app, &ids).await,
        Err(e) => tracing::error!(error = %e, "could not find documents to re-index"),
    }
}

/// Drops the index content and rebuilds it from the database. Returns the
/// number of documents indexed.
pub async fn rebuild(app: &App) -> Result<usize> {
    let ids: Vec<DocumentId> = sqlx::query_scalar("SELECT id FROM documents ORDER BY id")
        .fetch_all(&app.db.read)
        .await?;
    app.index.clear()?;
    for id in &ids {
        index_one(app, *id).await?;
    }
    commit(app).await?;
    tracing::info!(documents = ids.len(), "search index rebuilt");
    Ok(ids.len())
}

/// Commits pending index changes off the async threads.
pub async fn commit(app: &App) -> Result<()> {
    let index = app.index.clone();
    tokio::task::spawn_blocking(move || index.commit())
        .await
        .map_err(internal)?
}

/// Rebuilds the index when it holds a different number of documents than
/// the database (first start, deleted `index/`, schema change).
pub async fn ensure_current(app: &App) -> Result<()> {
    let expected: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM documents")
        .fetch_one(&app.db.read)
        .await?;
    if app.index.num_docs() != expected as u64 {
        rebuild(app).await?;
    }
    Ok(())
}

/// The service-layer entry point of `/search` and `search_documents`.
pub async fn search(ctx: &Ctx<'_>, request: SearchRequest) -> Result<SearchPage> {
    let index = ctx.app.index.clone();
    let date_fields = date_fields(ctx, &request).await?;
    // A search sees every change made before it, not only committed ones.
    tokio::task::spawn_blocking(move || {
        index.commit_if_dirty()?;
        index.search_with(&request, &date_fields)
    })
    .await
    .map_err(internal)?
}

/// Keys of the fields that hold dates, but only when the request compares a
/// field at all (the document types are read from the database for nothing
/// otherwise). A key is a date only if every type that defines it says so.
async fn date_fields(ctx: &Ctx<'_>, request: &SearchRequest) -> Result<BTreeSet<String>> {
    let compares_fields = !request.filters.fields.is_empty()
        || request
            .query
            .as_deref()
            .is_some_and(|q| q.to_lowercase().contains("field."));
    if !compares_fields {
        return Ok(BTreeSet::new());
    }
    let rows: Vec<String> = sqlx::query_scalar("SELECT fields FROM document_types")
        .fetch_all(&ctx.app.db.read)
        .await?;
    let (mut dates, mut other) = (BTreeSet::new(), BTreeSet::new());
    for row in rows {
        let defs: Vec<FieldDef> = serde_json::from_str(&row).map_err(internal)?;
        for def in defs {
            if def.kind == FieldKind::Date {
                dates.insert(def.key);
            } else {
                other.insert(def.key);
            }
        }
    }
    dates.retain(|key| !other.contains(key));
    Ok(dates)
}
