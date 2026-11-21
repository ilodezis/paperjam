//! Document endpoints: upload, read, edit, download, reprocess, trash.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Multipart, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderName,
    IF_NONE_MATCH,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::error::ServiceError;
use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiQuery, ApiResult, HttpState};
use crate::ids::{DocumentId, PersonId, TagId, TypeId};
use crate::jobs::queue::ProcessOptions;
use crate::model::document::{DocumentPatch, DocumentView, GetOptions};
use crate::service::documents;
use crate::service::upload::{UploadInput, UploadOutcome, upload};

#[derive(utoipa::ToSchema)]
pub struct UploadForm {
    /// The file to upload.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    /// Skip LLM enrichment and keep processing strictly local.
    pub local_only: Option<bool>,
    /// Tag IDs to assign immediately.
    pub tags: Option<Vec<String>>,
    /// Person IDs to assign immediately.
    pub people: Option<Vec<String>>,
    /// Document type ID to assign immediately.
    pub type_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Uploaded {
    #[schema(value_type = String)]
    pub id: DocumentId,
    pub status: &'static str,
    /// True when the same bytes were in the trash and that document came back.
    pub restored: bool,
}

fn bad_form(what: &str) -> ServiceError {
    ServiceError::invalid("invalid_form", what.to_string())
}

fn multipart_error(e: axum::extract::multipart::MultipartError) -> ServiceError {
    ServiceError::invalid("invalid_upload", format!("could not read the upload: {e}"))
}

fn parse_flag(value: &str) -> Result<bool, ServiceError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" => Ok(true),
        "false" | "0" | "off" | "no" | "" => Ok(false),
        _ => Err(bad_form("local_only must be true or false")),
    }
}

/// Upload a file (multipart). Fields `local_only`, `tags[]`, `people[]` and
/// `type_id` must come before the `file` part, which is streamed to disk.
#[utoipa::path(
    post,
    path = "/documents",
    tag = "documents",
    summary = "Upload a file (multipart).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body(content = UploadForm, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Document uploaded and queued", body = Uploaded),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 409, description = "Duplicate document exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid multipart payload", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
    mut form: Multipart,
) -> ApiResult<(StatusCode, Json<Uploaded>)> {
    let mut input = UploadInput::default();
    while let Some(field) = form.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                input.original_name = field.file_name().unwrap_or("upload").to_string();
                input.declared_mime = field.content_type().map(str::to_string);
                let stream = field.map_err(std::io::Error::other);
                let outcome = upload(
                    &state.app.ctx(principal.actor),
                    input,
                    StreamReader::new(stream),
                )
                .await?;
                let (id, restored) = match outcome {
                    UploadOutcome::Created(id) => (id, false),
                    UploadOutcome::Restored(id) => (id, true),
                };
                return Ok((
                    StatusCode::CREATED,
                    Json(Uploaded {
                        id,
                        status: "queued",
                        restored,
                    }),
                ));
            }
            "local_only" => {
                input.local_only = parse_flag(&field.text().await.map_err(multipart_error)?)?;
            }
            "tags" | "tags[]" => {
                let text = field.text().await.map_err(multipart_error)?;
                input.tags.push(
                    text.trim()
                        .parse::<TagId>()
                        .map_err(|_| bad_form("invalid tag id"))?,
                );
            }
            "people" | "people[]" => {
                let text = field.text().await.map_err(multipart_error)?;
                input.people.push(
                    text.trim()
                        .parse::<PersonId>()
                        .map_err(|_| bad_form("invalid person id"))?,
                );
            }
            "type_id" => {
                let text = field.text().await.map_err(multipart_error)?;
                if !text.trim().is_empty() {
                    input.type_id = Some(
                        text.trim()
                            .parse::<TypeId>()
                            .map_err(|_| bad_form("invalid type id"))?,
                    );
                }
            }
            other => return Err(bad_form(&format!("unknown form field {other:?}")).into()),
        }
    }
    Err(bad_form("the form has no `file` part (options go before it)").into())
}

#[derive(Deserialize)]
pub struct GetQuery {
    pub text_page: Option<u32>,
}

/// One document with its fields; `text_page` adds the text of that page.
#[utoipa::path(
    get,
    path = "/documents/{id}",
    tag = "documents",
    summary = "One document with its fields; text_page adds the text of that page.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
        ("text_page" = Option<u32>, Query, description = "Page number to include text for"),
    ),
    responses(
        (status = 200, description = "Document details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn get(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    ApiQuery(query): ApiQuery<GetQuery>,
) -> ApiResult<Json<DocumentView>> {
    let ctx = state.app.ctx(principal.actor);
    let view = documents::get_document(
        &ctx,
        id,
        GetOptions {
            text_page: query.text_page,
        },
    )
    .await?;
    Ok(Json(view))
}

/// Edit metadata. Every value set here gets provenance `manual`.
#[utoipa::path(
    patch,
    path = "/documents/{id}",
    tag = "documents",
    summary = "Edit metadata. Every value set here gets provenance manual.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Updated document details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid metadata patch", body = crate::http::ErrorResponse),
    )
)]
pub async fn patch(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(patch): ApiJson<DocumentPatch>,
) -> ApiResult<Json<DocumentView>> {
    let ctx = state.app.ctx(principal.actor);
    Ok(Json(documents::update_document(&ctx, id, patch).await?))
}

#[derive(Deserialize, Default)]
pub struct DownloadQuery {
    /// Show PDFs and images in the browser instead of saving them.
    #[serde(default)]
    pub inline: bool,
}

/// RFC 5987 `Content-Disposition`: an ASCII fallback plus the UTF-8 name.
fn content_disposition(kind: &str, name: &str) -> String {
    fn encode(name: &str) -> String {
        let mut out = String::new();
        for byte in name.bytes() {
            match byte {
                b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'!'
                | b'#'
                | b'$'
                | b'&'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~' => out.push(byte as char),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        encode(name)
    )
}

fn header_value(value: String) -> HeaderValue {
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// The original file, byte for byte, named after the document's title.
#[utoipa::path(
    get,
    path = "/documents/{id}/original",
    tag = "documents",
    summary = "The original file, byte for byte, named after the document's title.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
        ("inline" = Option<bool>, Query, description = "Show PDFs and images in the browser instead of saving them"),
    ),
    responses(
        (status = 200, description = "Original file contents", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document or file not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn original(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    ApiQuery(query): ApiQuery<DownloadQuery>,
) -> ApiResult<Response> {
    let file = documents::original_file(&state.app.ctx(principal.actor), id).await?;
    file_response(&state.app, file.into(), query.inline)
}

/// The document as a person downloads it: the clean copy of a scan (pages
/// upright, cut out and straightened, as a PDF) when there is one, else the
/// original; named after the title.
#[utoipa::path(
    get,
    path = "/documents/{id}/download",
    tag = "documents",
    summary = "The document as a person downloads it: the clean copy of a scan, else the original.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
        ("inline" = Option<bool>, Query, description = "Show PDFs and images in the browser instead of saving them"),
    ),
    responses(
        (status = 200, description = "Clean copy or original file contents", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document or file not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn download(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    ApiQuery(query): ApiQuery<DownloadQuery>,
) -> ApiResult<Response> {
    let file = documents::download_file(&state.app.ctx(principal.actor), id).await?;
    file_response(&state.app, file, query.inline)
}

/// A download (or inline preview) of `file`, byte for byte.
pub fn file_response(
    app: &crate::service::App,
    file: documents::DownloadFile,
    inline: bool,
) -> ApiResult<Response> {
    let handle = file
        .open(app)
        .map_err(|_| ServiceError::not_found("file_missing", "the file is missing from storage"))?;
    let showable = file.mime == "application/pdf" || file.mime.starts_with("image/");
    let kind = if inline && showable {
        "inline"
    } else {
        "attachment"
    };
    let body = Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(handle)));
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, header_value(file.mime));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(file.size));
    headers.insert(
        CONTENT_DISPOSITION,
        header_value(content_disposition(kind, &file.filename)),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// A cached picture. It is kept by the browser but checked on every use
/// (`no-cache` with an `ETag` of its content): a reprocess shows up at once,
/// and an unchanged picture costs a header-only 304, not a download.
pub(crate) async fn derived_image(
    state: &HttpState,
    id: DocumentId,
    name: &str,
    request: &HeaderMap,
) -> ApiResult<Response> {
    let path = state.app.files.derived_path(id, name);
    let not_ready =
        || ServiceError::not_found("image_not_ready", "this image has not been rendered (yet)");
    // The tag comes from length and mtime, so a revalidation costs a stat, not
    // a read and a hash. A rewrite changes the mtime and so the tag.
    let meta = tokio::fs::metadata(&path).await.map_err(|_| not_ready())?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let etag = format!("\"{:x}-{mtime_ns:x}\"", meta.len());
    let unchanged = request
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == "*" || tag == etag);
    let mut response = if unchanged {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        not_modified
    } else {
        let bytes = tokio::fs::read(&path).await.map_err(|_| not_ready())?;
        let mut ok = Response::new(Body::from(bytes));
        ok.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("image/webp"));
        ok
    };
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    headers.insert(ETAG, header_value(etag));
    Ok(response)
}

/// Page `n` as a cached WebP render (longest edge 1600 px).
#[utoipa::path(
    get,
    path = "/documents/{id}/pages/{file}",
    tag = "documents",
    summary = "Page n as a cached WebP render (longest edge 1600 px).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
        ("file" = String, Path, description = "Page file name: 1.webp is the page as the document is downloaded (the clean copy of a scan, when there is one); 1-original.webp is the page as it was read, before cleaning"),
    ),
    responses(
        (status = 200, description = "Rendered WebP page image", body = Vec<u8>, content_type = "image/webp"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Page image not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn page_image(
    State(state): State<HttpState>,
    _principal: Principal,
    ApiPath((id, file)): ApiPath<(DocumentId, String)>,
    request: HeaderMap,
) -> ApiResult<Response> {
    let stem = file.strip_suffix(".webp");
    let (stem, original) = match stem.and_then(|s| s.strip_suffix("-original")) {
        Some(n) => (Some(n), true),
        None => (stem, false),
    };
    let n: u32 = stem
        .and_then(|n| n.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            ServiceError::not_found(
                "page_not_found",
                "pages are named <n>.webp or <n>-original.webp",
            )
        })?;
    // A document with a clean copy shows the clean page; the page as it was
    // read is kept beside it. Without one, both names are the same picture.
    if original
        && tokio::fs::try_exists(
            state
                .app
                .files
                .derived_path(id, &format!("original-{n}.webp")),
        )
        .await
        .unwrap_or(false)
    {
        return derived_image(&state, id, &format!("original-{n}.webp"), &request).await;
    }
    derived_image(&state, id, &format!("page-{n}.webp"), &request).await
}

/// WebP thumbnail of the first page (longest edge 320 px, margins cropped).
#[utoipa::path(
    get,
    path = "/documents/{id}/thumbnail.webp",
    tag = "documents",
    summary = "WebP thumbnail of the first page (longest edge 320 px, margins cropped).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 200, description = "WebP thumbnail image", body = Vec<u8>, content_type = "image/webp"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Thumbnail not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn thumbnail(
    State(state): State<HttpState>,
    _principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    request: HeaderMap,
) -> ApiResult<Response> {
    derived_image(&state, id, "thumbnail.webp", &request).await
}

#[derive(Deserialize, Default, utoipa::ToSchema)]
#[serde(default)]
pub struct ReprocessBody {
    pub local: Option<bool>,
    pub llm: Option<bool>,
    pub model: Option<String>,
}

/// Run the processing pipeline again (idempotent while a run is queued).
#[utoipa::path(
    post,
    path = "/documents/{id}/reprocess",
    tag = "documents",
    summary = "Run the processing pipeline again (idempotent while a run is queued).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    request_body = ReprocessBody,
    responses(
        (status = 202, description = "Reprocessing job queued", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid request body, or a model not in PAPERJAM_LLM_ALLOWED_MODELS", body = crate::http::ErrorResponse),
    )
)]
pub async fn reprocess(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // An empty body means the defaults (both steps).
    let body: ReprocessBody = if body.iter().all(u8::is_ascii_whitespace) {
        ReprocessBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ServiceError::invalid("invalid_body", e.to_string()))?
    };
    let opts = ProcessOptions {
        local: body.local.unwrap_or(true),
        llm: body.llm.unwrap_or(true),
        model_override: body.model,
    };
    documents::reprocess_document(&state.app.ctx(principal.actor), id, opts).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"id": id, "status": "queued"})),
    ))
}

/// Move a document to the trash (reversible).
#[utoipa::path(
    post,
    path = "/documents/{id}/trash",
    tag = "documents",
    summary = "Move a document to the trash (reversible).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 204, description = "Document moved to trash"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn trash(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
) -> ApiResult<StatusCode> {
    documents::trash_document(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Bring a document back from the trash.
#[utoipa::path(
    post,
    path = "/documents/{id}/restore",
    tag = "documents",
    summary = "Bring a document back from the trash.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 204, description = "Document restored from trash"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn restore(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
) -> ApiResult<StatusCode> {
    documents::restore_document(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a trashed document for good. Needs a web session.
#[utoipa::path(
    delete,
    path = "/documents/{id}",
    tag = "documents",
    summary = "Delete a trashed document for good. Needs a web session.",
    security(
        ("session_cookie" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 204, description = "Document permanently purged"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Action requires a web session", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn purge(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<DocumentId>,
) -> ApiResult<StatusCode> {
    documents::purge_document(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Purge everything in the trash. Needs a web session.
#[utoipa::path(
    delete,
    path = "/trash",
    tag = "documents",
    summary = "Purge everything in the trash. Needs a web session.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 200, description = "Trash emptied", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Action requires a web session", body = crate::http::ErrorResponse),
    )
)]
pub async fn empty_trash(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<Value>> {
    let removed = documents::empty_trash(&state.app.ctx(principal.actor)).await?;
    Ok(Json(json!({"purged": removed})))
}

impl IntoResponse for Uploaded {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}
