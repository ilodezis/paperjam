//! Links that carry their own authority: no cookie or token is needed.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::TryStreamExt;
use serde::Deserialize;
use tokio_util::io::StreamReader;

use crate::http::api::documents::{Uploaded, file_response};
use crate::http::auth::Principal;
use crate::http::{ApiPath, ApiQuery, ApiResult, HttpState};
use crate::ids::DocumentId;
use crate::service::links::{DownloadLink, UploadLink};
use crate::service::upload::{UploadInput, UploadOutcome, upload};
use crate::service::{Actor, App, Via, documents, links};

fn base_url(app: &App, headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    links::base_url(app, host)
}

/// A short-lived URL to the original file (the MCP tool `get_download_link`).
#[utoipa::path(
    post,
    path = "/documents/{id}/download-link",
    tag = "documents",
    summary = "A short-lived (10 minutes) URL to the original file.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 200, description = "The link", body = DownloadLink),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_download_link(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
    ApiPath(id): ApiPath<DocumentId>,
) -> ApiResult<Json<DownloadLink>> {
    let base = base_url(&state.app, &headers);
    let link = links::create_download_link(&state.app.ctx(principal.actor), &base, id).await?;
    Ok(Json(link))
}

/// A one-time upload URL for clients with a shell (the MCP tool `create_upload_link`).
#[utoipa::path(
    post,
    path = "/upload-links",
    tag = "documents",
    summary = "A one-time upload URL, valid for 15 minutes.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 201, description = "The link", body = UploadLink),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_upload_link(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<UploadLink>)> {
    let base = base_url(&state.app, &headers);
    let link = links::issue_upload_link(&state.app.ctx(principal.actor), &base).await?;
    Ok((StatusCode::CREATED, Json(link)))
}

/// The original file behind a short-lived signed link (see the MCP tool `get_download_link`).
#[utoipa::path(
    get,
    path = "/dl/{token}",
    tag = "documents",
    summary = "The original file behind a short-lived signed link.",
    params(
        ("token" = String, Path, description = "Signed download token"),
    ),
    responses(
        (status = 200, description = "Original file contents", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 401, description = "The link is invalid or has expired", body = crate::http::ErrorResponse),
        (status = 404, description = "Document or file not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn download(
    State(state): State<HttpState>,
    ApiPath(token): ApiPath<String>,
) -> ApiResult<Response> {
    let id = links::verify_download_token(&state.app, &token)?;
    let file = documents::original_file(&state.app.ctx(Actor::system()), id).await?;
    file_response(&state.app, file.into(), false)
}

#[derive(Deserialize, Default)]
pub struct UploadLinkQuery {
    /// Display name of the file; `upload` when omitted.
    pub filename: Option<String>,
    #[serde(default)]
    pub local_only: bool,
}

/// Upload one file with a one-time link (see the MCP tool `create_upload_link`).
/// The body is the raw file: `curl -T file.pdf "<url>?filename=file.pdf"`.
#[utoipa::path(
    put,
    path = "/ul/{token}",
    tag = "documents",
    summary = "Upload one file with a one-time link; the body is the raw file.",
    params(
        ("token" = String, Path, description = "One-time upload token"),
        ("filename" = Option<String>, Query, description = "Display name of the file"),
        ("local_only" = Option<bool>, Query, description = "Keep the document away from the LLM"),
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Document uploaded and queued", body = Uploaded),
        (status = 401, description = "The link is invalid, used or expired", body = crate::http::ErrorResponse),
        (status = 409, description = "Duplicate document exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Unsupported file", body = crate::http::ErrorResponse),
    )
)]
pub async fn upload_with_link(
    State(state): State<HttpState>,
    ApiPath(token): ApiPath<String>,
    ApiQuery(query): ApiQuery<UploadLinkQuery>,
    body: Body,
) -> ApiResult<(StatusCode, Json<Uploaded>)> {
    let user_id = links::claim_upload_link(&state.app, &token).await?;
    let input = UploadInput {
        original_name: query.filename.unwrap_or_else(|| "upload".to_string()),
        local_only: query.local_only,
        ..Default::default()
    };
    let reader = StreamReader::new(body.into_data_stream().map_err(std::io::Error::other));
    let ctx = state.app.ctx(Actor::user(user_id, Via::Token));
    let outcome = match upload(&ctx, input, reader).await {
        Ok(outcome) => outcome,
        Err(e) => {
            // A failed upload (wrong type, duplicate) does not burn the link.
            let _ = links::release_upload_link(&state.app, &token).await;
            return Err(e.into());
        }
    };
    let (id, restored) = match outcome {
        UploadOutcome::Created(id) => (id, false),
        UploadOutcome::Restored(id) => (id, true),
    };
    Ok((
        StatusCode::CREATED,
        Json(Uploaded {
            id,
            status: "queued",
            restored,
        }),
    ))
}
