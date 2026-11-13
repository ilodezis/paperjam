//! Share links of a document: create, list the live ones, revoke.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiResult, HttpState};
use crate::ids::DocumentId;
use crate::service::shares::{self, NewShare, Share};
use crate::service::{App, links};

fn base_url(app: &App, headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    links::base_url(app, host)
}

/// A link to a page that shows the document to anyone who has it, for 15
/// minutes, an hour or a day.
#[utoipa::path(
    post,
    path = "/documents/{id}/shares",
    tag = "shares",
    summary = "A share link: a page that shows the document to anyone who has it, for 15 minutes, an hour or a day.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    request_body = NewShare,
    responses(
        (status = 201, description = "The link", body = Share),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document not found", body = crate::http::ErrorResponse),
        (status = 409, description = "The document is in the trash", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(new): ApiJson<NewShare>,
) -> ApiResult<(StatusCode, Json<Share>)> {
    let base = base_url(&state.app, &headers);
    let share = shares::create_share(&state.app.ctx(principal.actor), &base, id, new).await?;
    Ok((StatusCode::CREATED, Json(share)))
}

/// The document's share links that still work, newest first.
#[utoipa::path(
    get,
    path = "/documents/{id}/shares",
    tag = "shares",
    summary = "The document's share links that still work, newest first.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Document ID"),
    ),
    responses(
        (status = 200, description = "Live share links", body = Vec<Share>),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
    ApiPath(id): ApiPath<DocumentId>,
) -> ApiResult<Json<Vec<Share>>> {
    let base = base_url(&state.app, &headers);
    let list = shares::list_shares(&state.app.ctx(principal.actor), &base, id).await?;
    Ok(Json(list))
}

/// Stops a share link from working.
#[utoipa::path(
    delete,
    path = "/shares/{id}",
    tag = "shares",
    summary = "Stops a share link from working.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Share ID"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "No such share link", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<StatusCode> {
    shares::revoke_share(&state.app.ctx(principal.actor), &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
