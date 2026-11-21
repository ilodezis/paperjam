//! Writing the pipeline's results to the database, in one transaction.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde_json::json;
use sqlx::SqliteConnection;

use crate::error::{Result, ServiceError};
use crate::ids::{CorrespondentId, DocumentId, TypeId};
use crate::jobs::queue::ProcessOptions;
use crate::matching;
use crate::model::document::Provenance;
use crate::model::fields::{FieldDef, FieldKind, FieldValue};
use crate::mrz::{self, Mrz, MrzFormat};
use crate::pipeline::checks;
use crate::pipeline::local::{LocalOutput, PageOut, SourceFile};
use crate::pipeline::people::{self, names_and_aliases};
use crate::search::sync::{self, Related};
use crate::service::documents::{self, Layer, best_layer};
use crate::service::{Actor, App, events};
use crate::time;

/// What the pipeline needs to know about a document before it starts.
pub struct DocInfo {
    pub title_provenance: Option<Provenance>,
    pub type_provenance: Option<Provenance>,
    pub mime: String,
    pub storage_key: String,
    pub original_name: String,
}

impl DocInfo {
    pub fn source(&self) -> SourceFile {
        SourceFile {
            mime: self.mime.clone(),
            storage_key: self.storage_key.clone(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct InfoRow {
    title_provenance: Option<Provenance>,
    type_provenance: Option<Provenance>,
    mime: String,
    storage_key: String,
    original_name: String,
}

pub async fn load_info(app: &App, id: DocumentId) -> Result<DocInfo> {
    let row: InfoRow = sqlx::query_as(
        "SELECT d.title_provenance, d.type_provenance, \
                f.mime, f.storage_key, f.original_name \
         FROM documents d JOIN files f ON f.document_id = d.id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&app.db.read)
    .await?
    .ok_or_else(|| ServiceError::not_found("document_not_found", "no such document"))?;
    Ok(DocInfo {
        title_provenance: row.title_provenance,
        type_provenance: row.type_provenance,
        mime: row.mime,
        storage_key: row.storage_key,
        original_name: row.original_name,
    })
}

pub async fn set_processing(app: &App, id: DocumentId) -> Result<()> {
    let done = sqlx::query(
        "UPDATE documents SET status = 'processing', status_reason = NULL WHERE id = ?",
    )
    .bind(id)
    .execute(&app.db.write)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ServiceError::not_found(
            "document_not_found",
            "no such document",
        ));
    }
    Ok(())
}

/// `status = failed`, with the error code as the reason and only the
/// `processing_failed` flag.
pub async fn mark_failed(app: &App, id: DocumentId, error: &ServiceError) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    sqlx::query(
        "UPDATE documents SET status = 'failed', status_reason = ?, attention = ? WHERE id = ?",
    )
    .bind(error.code)
    .bind(json!([checks::PROCESSING_FAILED]).to_string())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    events::record(
        &mut tx,
        &Actor::system(),
        "document.process_failed",
        Some(id),
        json!({"code": error.code}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Stores the result of a run and, with `finish`, marks the document `ready`.
/// A run whose LLM steps still have to follow stores the local results with
/// `finish = false` and calls this again (without output) when they are done.
/// `llm_follows`: an LLM-only job comes next, so no holder becomes a person
/// yet.
pub async fn commit(
    app: &App,
    id: DocumentId,
    info: &DocInfo,
    mut output: Option<LocalOutput>,
    opts: &ProcessOptions,
    finish: bool,
    llm_follows: bool,
) -> Result<()> {
    if output.is_none() && !finish {
        return Ok(());
    }
    let clean_size = if let Some(output) = &mut output {
        write_derived_files(app, id, output).await?
    } else {
        None
    };

    let mut tx = app.db.write.begin().await?;
    if let Some(output) = &output {
        // Reprocessing recomputes attention from scratch.
        sqlx::query("UPDATE documents SET attention = '[]' WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        store_local(&mut tx, id, info, output, clean_size).await?;
    }
    // The holder of an identity document is linked to the person of that
    // name when nobody else is; a new person is created only at the end of
    // the run, once the model has checked the names the zone brought.
    let end_of_run = finish && !llm_follows;
    people::link_holder_in(&mut tx, id, end_of_run).await?;
    let renamed = if end_of_run {
        people::prefer_holder_script(&mut tx, id).await?
    } else {
        None
    };
    let flags = checks::run_in(&mut tx, id, &[]).await?;
    checks::store_in(&mut tx, id, &flags).await?;
    if finish {
        sqlx::query(
            "UPDATE documents SET status = 'ready', status_reason = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(time::now_ts())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        events::record(
            &mut tx,
            &Actor::system(),
            "document.process",
            Some(id),
            json!({
                "local": opts.local,
                "llm": opts.llm,
                "pages": output.as_ref().map(|o| o.pages.len()),
                "attention": flags,
            }),
        )
        .await?;
    }
    tx.commit().await?;
    if let Some(person) = renamed {
        // The other documents of that person show the new name too.
        sync::refresh_related(app, Related::Person(person)).await;
    }
    Ok(())
}

/// Persists fast prepared local artifacts (renders, thumbnail, embedded text,
/// local title, and for OCR-free documents local matches) while leaving the
/// document in `processing` until LLM and/or background OCR complete.
pub async fn commit_prepared(
    app: &App,
    id: DocumentId,
    info: &DocInfo,
    output: &LocalOutput,
    has_pending_ocr: bool,
) -> Result<()> {
    app.files.delete_derived(id).await;
    let dir = app
        .files
        .derived_path(id, "_")
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    if let Some(thumbnail) = &output.thumbnail {
        writes.push((
            app.files.derived_path(id, "thumbnail.webp"),
            thumbnail.clone(),
        ));
    }
    for page in &output.pages {
        if let Some(render) = &page.render {
            writes.push((
                app.files.derived_path(id, &render_key(page.n)),
                render.clone(),
            ));
        }
    }
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        for (path, bytes) in writes {
            write_atomic(&path, &bytes)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| storage_error(std::io::Error::other(e)))?
    .map_err(storage_error)?;

    let mut tx = app.db.write.begin().await?;
    sqlx::query("UPDATE documents SET attention = '[]' WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let now = time::now_ts();
    store_pages(&mut tx, id, output, &now).await?;
    sqlx::query("UPDATE files SET clean_size = NULL WHERE document_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM field_values WHERE document_id = ? AND provenance IN ('mrz', 'local')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM facts WHERE document_id = ? AND provenance IN ('mrz', 'local')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    store_title(&mut tx, id, info).await?;
    if !has_pending_ocr {
        store_matches(&mut tx, id, output).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Applies background OCR results, straightened renders and clean PDF without
/// clobbering concurrent or already-completed LLM work.
pub async fn commit_ocr_backfill(
    app: &App,
    id: DocumentId,
    info: &DocInfo,
    mut output: LocalOutput,
) -> Result<()> {
    let clean_path = app.files.derived_path(id, documents::CLEAN_COPY);
    let dir = clean_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    if let Some(thumbnail) = output.thumbnail.take() {
        writes.push((app.files.derived_path(id, "thumbnail.webp"), thumbnail));
    }
    for page in &mut output.pages {
        if page.ocr.is_none() {
            continue;
        }
        if let Some(render) = page.render.take() {
            writes.push((app.files.derived_path(id, &render_key(page.n)), render));
        }
        if let Some(original) = page.original.take() {
            writes.push((app.files.derived_path(id, &original_key(page.n)), original));
        }
    }
    let clean_pages = output.clean.take();
    let clean_size = tokio::task::spawn_blocking(move || -> std::io::Result<Option<i64>> {
        std::fs::create_dir_all(&dir)?;
        for (path, bytes) in writes {
            write_atomic(&path, &bytes)?;
        }
        if let Some(pages) = clean_pages {
            let written = write_pdf_atomic(&clean_path, &pages)?;
            Ok(Some(i64::try_from(written).unwrap_or(i64::MAX)))
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|e| storage_error(std::io::Error::other(e)))?
    .map_err(storage_error)?;

    let mut tx = app.db.write.begin().await?;
    let doc_state: Option<(String, Option<Provenance>)> =
        sqlx::query_as("SELECT status, type_provenance FROM documents WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((status, current_type_provenance)) = doc_state else {
        return Ok(());
    };
    let now = time::now_ts();
    for page in &output.pages {
        if page.ocr.is_none() {
            continue;
        }
        if let Some((w, h)) = page.size {
            sqlx::query("UPDATE pages SET width = ?, height = ? WHERE document_id = ? AND n = ?")
                .bind(i64::from(w))
                .bind(i64::from(h))
                .bind(id)
                .bind(i64::from(page.n))
                .execute(&mut *tx)
                .await?;
        }
        if let Some((_, score)) = &page.embedded {
            sqlx::query(
                "UPDATE text_layers SET quality_score = ? \
                 WHERE document_id = ? AND page = ? AND source = 'embedded'",
            )
            .bind(f64::from(*score))
            .bind(id)
            .bind(i64::from(page.n))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "DELETE FROM text_layers WHERE document_id = ? AND page = ? AND source = 'tesseract'",
        )
        .bind(id)
        .bind(i64::from(page.n))
        .execute(&mut *tx)
        .await?;
        if let Some(ocr) = &page.ocr
            && !ocr.text.trim().is_empty()
        {
            insert_layer(
                &mut tx,
                id,
                page.n,
                "tesseract",
                &ocr.text,
                None,
                f64::from(ocr.mean_conf / 100.0),
                &now,
            )
            .await?;
        }
    }
    if let Some(cs) = clean_size {
        sqlx::query("UPDATE files SET clean_size = ? WHERE document_id = ?")
            .bind(cs)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let has_llm_fields: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM field_values WHERE document_id = ? AND provenance = 'llm')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if status != "ready" && !has_llm_fields && current_type_provenance != Some(Provenance::Mrz) {
        let fresh_info = DocInfo {
            title_provenance: info.title_provenance,
            type_provenance: current_type_provenance,
            mime: info.mime.clone(),
            storage_key: info.storage_key.clone(),
            original_name: info.original_name.clone(),
        };
        store_mrz(&mut tx, id, &fresh_info, &output, &now).await?;
    }
    let has_llm_links: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM document_people WHERE document_id = ? AND provenance = 'llm') \
            OR EXISTS(SELECT 1 FROM document_organizations WHERE document_id = ? AND provenance = 'llm')",
    )
    .bind(id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if status != "ready" && !has_llm_links {
        store_matches(&mut tx, id, &output).await?;
    }
    let renamed = if status == "ready" {
        people::link_holder_in(&mut tx, id, true).await?;
        let renamed = people::prefer_holder_script(&mut tx, id).await?;
        checks::recompute_in(&mut tx, id, &[]).await?;
        renamed
    } else {
        None
    };
    tx.commit().await?;
    if status == "ready" {
        sync::refresh(app, id).await;
    }
    if let Some(person) = renamed {
        sync::refresh_related(app, Related::Person(person)).await;
    }
    Ok(())
}

fn storage_error(_: std::io::Error) -> ServiceError {
    ServiceError::internal("storage_error", "could not write the page renders")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("derived");
    let tmp = path.with_file_name(format!("{name}.tmp.{}", crate::random::hex(4)));
    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    rename_with_retry(&tmp, path)
}

fn write_pdf_atomic(
    final_path: &Path,
    clean_pages: &[crate::extract::clean::Page],
) -> std::io::Result<u64> {
    use std::io::Write;
    let tmp = final_path.with_file_name(format!("clean.pdf.tmp.{}", crate::random::hex(4)));
    let write_res = (|| -> std::io::Result<u64> {
        let file = std::fs::File::create(&tmp)?;
        let mut buf = std::io::BufWriter::with_capacity(64 * 1024, file);
        let written = crate::extract::clean::write_pdf(clean_pages, &mut buf)?;
        buf.flush()?;
        Ok(written)
    })();
    let written = match write_res {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    rename_with_retry(&tmp, final_path)?;
    Ok(written)
}

fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    for attempt in 0..5 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if attempt < 4
                    && (e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.kind() == std::io::ErrorKind::AlreadyExists
                        || e.raw_os_error() == Some(32)) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                let _ = std::fs::remove_file(from);
                return Err(e);
            }
        }
    }
    Ok(())
}

async fn write_derived_files(
    app: &App,
    id: DocumentId,
    output: &mut LocalOutput,
) -> Result<Option<i64>> {
    app.files.delete_derived(id).await;
    let dir = app.files.derived_path(id, "_");
    let dir = dir.parent().unwrap_or(Path::new(".")).to_path_buf();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(storage_error)?;
    if let Some(thumbnail) = &output.thumbnail {
        tokio::fs::write(app.files.derived_path(id, "thumbnail.webp"), thumbnail)
            .await
            .map_err(storage_error)?;
    }
    for page in &output.pages {
        if let Some(render) = &page.render {
            tokio::fs::write(app.files.derived_path(id, &render_key(page.n)), render)
                .await
                .map_err(storage_error)?;
        }
        if let Some(original) = &page.original {
            tokio::fs::write(app.files.derived_path(id, &original_key(page.n)), original)
                .await
                .map_err(storage_error)?;
        }
    }
    let clean_size = if let Some(clean_pages) = output.clean.take() {
        let final_path = app.files.derived_path(id, documents::CLEAN_COPY);
        let written =
            tokio::task::spawn_blocking(move || write_pdf_atomic(&final_path, &clean_pages))
                .await
                .map_err(|e| storage_error(std::io::Error::other(e)))?
                .map_err(storage_error)?;
        Some(i64::try_from(written).unwrap_or(i64::MAX))
    } else {
        None
    };
    Ok(clean_size)
}

fn render_key(n: u32) -> String {
    format!("page-{n}.webp")
}

/// Page `n` as it was read, kept beside the clean render that replaced it.
fn original_key(n: u32) -> String {
    format!("original-{n}.webp")
}

async fn store_local(
    tx: &mut SqliteConnection,
    id: DocumentId,
    info: &DocInfo,
    output: &LocalOutput,
    clean_size: Option<i64>,
) -> Result<()> {
    let now = time::now_ts();
    store_pages(tx, id, output, &now).await?;
    sqlx::query("UPDATE files SET clean_size = ? WHERE document_id = ?")
        .bind(clean_size)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "DELETE FROM field_values WHERE document_id = ? AND provenance IN ('mrz', 'local')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM facts WHERE document_id = ? AND provenance IN ('mrz', 'local')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    store_mrz(tx, id, info, output, &now).await?;
    store_title(tx, id, info).await?;
    store_matches(tx, id, output).await?;
    Ok(())
}

async fn store_pages(
    tx: &mut SqliteConnection,
    id: DocumentId,
    output: &LocalOutput,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM text_layers WHERE document_id = ? AND source IN ('embedded', 'tesseract')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM pages WHERE document_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for page in &output.pages {
        sqlx::query(
            "INSERT INTO pages (document_id, n, width, height, has_embedded_text, render_key) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(i64::from(page.n))
        .bind(page.size.map(|s| i64::from(s.0)))
        .bind(page.size.map(|s| i64::from(s.1)))
        .bind(page.embedded.is_some())
        .bind(page.render.as_ref().map(|_| render_key(page.n)))
        .execute(&mut *tx)
        .await?;
        if let Some((text, score)) = &page.embedded {
            insert_layer(
                tx,
                id,
                page.n,
                "embedded",
                text,
                None,
                f64::from(*score),
                now,
            )
            .await?;
        }
        if let Some(ocr) = &page.ocr
            && !ocr.text.trim().is_empty()
        {
            insert_layer(
                tx,
                id,
                page.n,
                "tesseract",
                &ocr.text,
                None,
                f64::from(ocr.mean_conf / 100.0),
                now,
            )
            .await?;
        }
    }
    sqlx::query("UPDATE files SET page_count = ? WHERE document_id = ?")
        .bind(output.pages.len() as i64)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_layer(
    tx: &mut SqliteConnection,
    id: DocumentId,
    page: u32,
    source: &str,
    text: &str,
    word_boxes: Option<&[u8]>,
    quality_score: f64,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO text_layers (document_id, page, source, text, word_boxes, quality_score, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(i64::from(page))
    .bind(source)
    .bind(text)
    .bind(word_boxes)
    .bind(quality_score)
    .bind(now)
    .execute(tx)
    .await?;
    Ok(())
}

/// The MRZ of the document: the first one whose check digits verify, else the
/// first one that parses.
fn find_mrz(output: &LocalOutput) -> Option<(u32, Mrz)> {
    let mut parsed = None;
    for page in &output.pages {
        // The dedicated MRZ reading first; the general text as a fallback.
        let zone = [
            page.mrz_text.as_deref(),
            page.ocr.as_ref().map(|o| o.text.as_str()),
        ]
        .into_iter()
        .flatten()
        .filter_map(mrz::find)
        .max_by_key(|z| z.checks_ok);
        let Some(zone) = zone else {
            continue;
        };
        if zone.checks_ok {
            return Some((page.n, zone));
        }
        parsed.get_or_insert((page.n, zone));
    }
    parsed
}

enum MrzValue {
    Text(String),
    Date(NaiveDate),
}

fn mrz_values(zone: &Mrz) -> Vec<(&'static str, MrzValue)> {
    let mut values = vec![
        ("number", MrzValue::Text(zone.document_number.clone())),
        ("nationality", MrzValue::Text(zone.nationality.clone())),
        ("birth_date", MrzValue::Date(zone.birth_date)),
    ];
    if let Some(expiry) = zone.expiry_date {
        values.push(("expiry_date", MrzValue::Date(expiry)));
    }
    if let Some(series) = &zone.series {
        values.push(("series", MrzValue::Text(series.clone())));
    }
    if let Some(issued) = zone.issue_date {
        values.push(("issue_date", MrzValue::Date(issued)));
    }
    // A zone whose names line was unreadable brings no names.
    for (key, name) in [
        ("surname", &zone.surname),
        ("given_names", &zone.given_names),
    ] {
        if !name.is_empty() {
            values.push((key, MrzValue::Text(name.clone())));
        }
    }
    if let Some(sex) = zone.sex {
        values.push(("sex", MrzValue::Text(sex.to_string())));
    }
    if let Some(number) = zone.personal_number.as_ref().filter(|n| !n.is_empty()) {
        values.push(("personal_number", MrzValue::Text(number.clone())));
    }
    values
}

/// A value shaped for the field's kind, or `None` when the MRZ value does not
/// fit it.
fn shape(value: MrzValue, def: &FieldDef) -> Option<FieldValue> {
    match (value, def.kind) {
        (MrzValue::Date(d), FieldKind::Date) => Some(FieldValue::Date(d)),
        (MrzValue::Text(t), FieldKind::Text) => Some(FieldValue::Text(t)),
        (MrzValue::Text(t), FieldKind::Identifier) => Some(FieldValue::Identifier(t)),
        _ => None,
    }
}

async fn type_defs(tx: &mut SqliteConnection, id: DocumentId) -> Result<Vec<FieldDef>> {
    let fields: Option<String> = sqlx::query_scalar(
        "SELECT t.fields FROM documents d JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(tx)
    .await?;
    Ok(fields
        .and_then(|f| serde_json::from_str(&f).ok())
        .unwrap_or_default())
}

async fn store_mrz(
    tx: &mut SqliteConnection,
    id: DocumentId,
    info: &DocInfo,
    output: &LocalOutput,
    now: &str,
) -> Result<()> {
    let Some((page, zone)) = find_mrz(output) else {
        return Ok(());
    };
    if !zone.checks_ok {
        // Unverified digits are not trusted to fill anything. The verdict is
        // kept as a fact so that the `checksum_failed` flag survives every
        // later recomputation of the flags (an LLM-only run, a field edit).
        sqlx::query(
            "INSERT INTO facts (document_id, key, value, provenance, page) VALUES (?, ?, 'failed', 'mrz', ?)",
        )
        .bind(id)
        .bind(checks::MRZ_CHECK_FACT)
        .bind(i64::from(page))
        .execute(&mut *tx)
        .await?;
        return Ok(());
    }
    store_verified_zone(tx, id, info.type_provenance, page, &zone, now).await
}

/// The type a verified zone implies (unless the user set one) and its values,
/// with `mrz` provenance.
async fn store_verified_zone(
    tx: &mut SqliteConnection,
    id: DocumentId,
    type_provenance: Option<Provenance>,
    page: u32,
    zone: &Mrz,
    now: &str,
) -> Result<()> {
    if type_provenance != Some(Provenance::Manual) {
        let key = match zone.format {
            MrzFormat::TD3 => "travel_passport",
            MrzFormat::RuInternal => "passport",
            MrzFormat::TD1 | MrzFormat::TD2 => "id_card",
        };
        let found: Option<(TypeId,)> =
            sqlx::query_as("SELECT id FROM document_types WHERE key = ?")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((type_id,)) = found {
            sqlx::query("UPDATE documents SET type_id = ?, type_provenance = 'mrz' WHERE id = ?")
                .bind(type_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }

    let defs = type_defs(tx, id).await?;
    for (key, value) in mrz_values(zone) {
        let Some(def) = defs.iter().find(|d| d.key == key) else {
            continue;
        };
        let Some(value) = shape(value, def) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_by, updated_at) \
             VALUES (?, ?, ?, 'mrz', ?, NULL, ?) \
             ON CONFLICT (document_id, field_key) DO UPDATE SET \
                 value = excluded.value, provenance = 'mrz', page = excluded.page, \
                 updated_by = NULL, updated_at = excluded.updated_at \
             WHERE field_values.provenance != 'manual'",
        )
        .bind(id)
        .bind(key)
        .bind(json!(value).to_string())
        .bind(i64::from(page))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// The machine-readable zone read from the model's transcription: Tesseract
/// often garbles the zone of a photographed spread (Cyrillic look-alikes, a
/// lost line, filler read as `K<X<`) while the model copies it cleanly. Only
/// a zone whose check digits all verify is used, so a misread digit cannot
/// slip in; when found, it replaces any local MRZ reading (fields and type,
/// keeping manual edits) and clears a broken-zone verdict. Returns whether a
/// zone was stored.
pub async fn mrz_from_transcription(app: &App, id: DocumentId) -> Result<bool> {
    let mut tx = app.db.write.begin().await?;
    let pages: Vec<(i64, String)> = sqlx::query_as(
        "SELECT page, text FROM text_layers WHERE document_id = ? AND source = 'llm' ORDER BY page",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let Some((page, zone)) = pages.iter().find_map(|(page, text)| {
        let zone = mrz::find(text).filter(|z| z.checks_ok)?;
        Some((u32::try_from(*page).ok()?, zone))
    }) else {
        return Ok(false);
    };
    let type_provenance: Option<Provenance> =
        sqlx::query_scalar("SELECT type_provenance FROM documents WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM field_values WHERE document_id = ? AND provenance = 'mrz'")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM facts WHERE document_id = ? AND key = ? AND provenance = 'mrz'")
        .bind(id)
        .bind(checks::MRZ_CHECK_FACT)
        .execute(&mut *tx)
        .await?;
    store_verified_zone(&mut tx, id, type_provenance, page, &zone, &time::now_ts()).await?;
    tx.commit().await?;
    Ok(true)
}

/// Without a title from the user or the LLM, the file name (no extension).
async fn store_title(tx: &mut SqliteConnection, id: DocumentId, info: &DocInfo) -> Result<()> {
    let replaceable = matches!(info.title_provenance, None | Some(Provenance::Local));
    if !replaceable {
        return Ok(());
    }
    let stem = Path::new(&info.original_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&info.original_name);
    sqlx::query("UPDATE documents SET title = ?, title_provenance = 'local' WHERE id = ?")
        .bind(stem)
        .bind(id)
        .execute(tx)
        .await?;
    Ok(())
}

/// Matches people and the correspondent against the document text, unless
/// they were set by hand.
async fn store_matches(
    tx: &mut SqliteConnection,
    id: DocumentId,
    output: &LocalOutput,
) -> Result<()> {
    let text = output
        .pages
        .iter()
        .filter_map(page_text)
        .collect::<Vec<_>>()
        .join("\n");

    people::match_document(tx, id, &text).await?;

    // Runs only when the document has no manual or llm organization row.
    let has_settled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM document_organizations WHERE document_id = ? AND provenance IN ('manual', 'llm'))",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    if !has_settled {
        let rows: Vec<(CorrespondentId, String, String)> =
            sqlx::query_as("SELECT id, name, aliases FROM correspondents")
                .fetch_all(&mut *tx)
                .await?;
        let candidates: Vec<(CorrespondentId, Vec<String>)> = rows
            .into_iter()
            .map(|(cid, name, aliases)| (cid, names_and_aliases(name, &aliases)))
            .collect();
        let found = matching::find_correspondent(&text, &candidates);
        sqlx::query(
            "DELETE FROM document_organizations WHERE document_id = ? AND provenance = 'local'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if let Some(cid) = found {
            sqlx::query(
                "INSERT OR IGNORE INTO document_organizations (document_id, correspondent_id, role, provenance) VALUES (?, ?, 'unspecified', 'local')",
            )
            .bind(id)
            .bind(cid)
            .execute(&mut *tx)
            .await?;
        }
    }
    Ok(())
}

/// The text shown for a page: good embedded text, else OCR, else poor embedded.
fn page_text(page: &PageOut) -> Option<String> {
    let mut layers: Vec<Layer> = Vec::new();
    if let Some((text, score)) = &page.embedded {
        layers.push(("embedded".into(), text.clone(), Some(f64::from(*score))));
    }
    if let Some(ocr) = &page.ocr {
        layers.push(("tesseract".into(), ocr.text.clone(), None));
    }
    best_layer(&layers).map(|l| l.1.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::quality;

    #[test]
    fn good_embedded_text_is_preferred_over_ocr() {
        let page = PageOut {
            n: 1,
            size: None,
            embedded: Some(("exact text".into(), quality::GOOD_THRESHOLD + 0.1)),
            mrz_text: None,
            ocr: Some(crate::extract::tesseract::OcrPage {
                text: "ocr text".into(),
                mean_conf: 90.0,
                reading: Default::default(),
                mrz_region: None,
            }),
            render: None,
            clean: None,
            clean_render: None,
            original: None,
        };
        assert_eq!(page_text(&page).as_deref(), Some("exact text"));
    }

    #[test]
    fn a_russian_internal_passport_zone_fills_series_and_number_not_personal_number() {
        let zone = mrz::find(
            "PNRUSTESTOV<<P2TR<SERGEEVI3<<<<<<<<<<<<<<<<<\n\
             4501234568RUS9001158M<<<<<<<9100201770001<42",
        )
        .expect("zone");
        let values = mrz_values(&zone);
        let text = |key: &str| {
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| match v {
                    MrzValue::Text(t) => t.clone(),
                    MrzValue::Date(d) => d.to_string(),
                })
        };
        assert_eq!(text("series").as_deref(), Some("45 09"));
        assert_eq!(text("number").as_deref(), Some("123456"));
        assert_eq!(text("issue_date").as_deref(), Some("2010-02-01"));
        assert_eq!(text("surname").as_deref(), Some("ТЕСТОВ"));
        assert_eq!(text("personal_number"), None);
        assert_eq!(text("expiry_date"), None);
    }
}
