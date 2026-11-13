//! Search over the one query language shared with MCP.

use axum::Json;
use axum::extract::State;

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiResult, HttpState};
use crate::search::{SearchPage, SearchRequest, sync};

/// Full-text and structured search; results are paged with `cursor`.
#[utoipa::path(
    post,
    path = "/search",
    tag = "search",
    summary = "Full-text and structured search; results are paged with cursor.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = SearchRequest,
    responses(
        (status = 200, description = "Search results page", body = SearchPage),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid search query", body = crate::http::ErrorResponse),
    )
)]
pub async fn search(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(request): ApiJson<SearchRequest>,
) -> ApiResult<Json<SearchPage>> {
    Ok(Json(
        sync::search(&state.app.ctx(principal.actor), request).await?,
    ))
}
