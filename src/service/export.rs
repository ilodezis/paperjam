//! Export: every original plus its metadata in an open, documented layout
//! (the anti-lock-in guarantee, spec §14).

use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::{Result, ServiceError};
use crate::ids::DocumentId;
use crate::model::document::GetOptions;
use crate::service::{Actor, App, documents, taxonomy};

const README: &str = "paperjam export\n\
===============\n\
\n\
documents/<id>/original.<ext>   the file exactly as it was uploaded\n\
documents/<id>/metadata.json    title, date, type, organizations, people, tags,\n\
                                fields (with provenance: llm, mrz, manual or local),\n\
                                facts, status, attention flags, text layer index\n\
documents/<id>/text/<page>-<source>.txt\n\
                                text of a page from one source:\n\
                                embedded (the PDF's own text), tesseract (OCR) or llm\n\
taxonomy.json                   document types (with their fields), people,\n\
                                correspondents and tags\n\
\n\
Everything is plain JSON and text. <id> is the document id; dates are ISO 8601,\n\
timestamps are UTC.\n";

#[derive(Debug, Clone, Copy)]
pub struct ExportSummary {
    pub documents: usize,
}

fn zip_error(e: impl std::fmt::Display) -> ServiceError {
    tracing::error!(error = %e, "export failed");
    ServiceError::internal("export_failed", "writing the export failed")
}

fn extension(original_name: &str, mime: &str) -> String {
    let from_name = Path::new(original_name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    if let Some(ext) = from_name {
        return ext.to_ascii_lowercase();
    }
    match mime {
        "application/pdf" => "pdf",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/tiff" => "tif",
        "image/heic" | "image/heif" => "heic",
        "text/markdown" => "md",
        _ => "txt",
    }
    .to_string()
}

fn options(method: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true)
}

/// Writes every document, trashed ones included, to a zip at `target`.
pub async fn export(app: &App, target: &Path) -> Result<ExportSummary> {
    let ctx = app.ctx(Actor::cli());
    let ids: Vec<DocumentId> = sqlx::query_scalar("SELECT id FROM documents ORDER BY id")
        .fetch_all(&app.db.read)
        .await?;

    let file = std::fs::File::create(target).map_err(zip_error)?;
    let mut zip = ZipWriter::new(file);
    let text_options = options(CompressionMethod::Deflated);
    // PDFs and images are already compressed.
    let blob_options = options(CompressionMethod::Stored);

    zip.start_file("README.txt", text_options)
        .map_err(zip_error)?;
    zip.write_all(README.as_bytes()).map_err(zip_error)?;

    let taxonomy = taxonomy::list_taxonomy(&ctx).await?;
    zip.start_file("taxonomy.json", text_options)
        .map_err(zip_error)?;
    zip.write_all(&serde_json::to_vec_pretty(&taxonomy).unwrap_or_default())
        .map_err(zip_error)?;

    for id in &ids {
        let view = documents::get_document(&ctx, *id, GetOptions::default()).await?;
        let original = documents::original_file(&ctx, *id).await?;

        let ext = extension(&original.original_name, &original.mime);
        zip.start_file(format!("documents/{id}/original.{ext}"), blob_options)
            .map_err(zip_error)?;
        match app.files.open_original(&original.storage_key) {
            Ok(mut blob) => {
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    let n = blob.read(&mut buffer).map_err(zip_error)?;
                    if n == 0 {
                        break;
                    }
                    zip.write_all(&buffer[..n]).map_err(zip_error)?;
                }
            }
            Err(e) => {
                tracing::warn!(document = %id, error = %e, "an original is missing from storage");
            }
        }

        let layers: Vec<(i64, String, String, Option<f64>)> = sqlx::query_as(
            "SELECT page, source, text, quality_score FROM text_layers \
             WHERE document_id = ? ORDER BY page, source",
        )
        .bind(id)
        .fetch_all(&app.db.read)
        .await?;
        let mut index = Vec::with_capacity(layers.len());
        for (page, source, text, quality) in &layers {
            let path = format!("text/{page}-{source}.txt");
            zip.start_file(format!("documents/{id}/{path}"), text_options)
                .map_err(zip_error)?;
            zip.write_all(text.as_bytes()).map_err(zip_error)?;
            index.push(json!({"page": page, "source": source, "quality": quality, "path": path}));
        }

        let mut metadata: Value = serde_json::to_value(&view).map_err(zip_error)?;
        metadata["text_layers"] = Value::Array(index);
        zip.start_file(format!("documents/{id}/metadata.json"), text_options)
            .map_err(zip_error)?;
        zip.write_all(&serde_json::to_vec_pretty(&metadata).unwrap_or_default())
            .map_err(zip_error)?;
    }
    zip.finish().map_err(zip_error)?;
    Ok(ExportSummary {
        documents: ids.len(),
    })
}
