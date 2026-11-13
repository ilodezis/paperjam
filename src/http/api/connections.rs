//! OAuth clients the signed-in user has connected, such as Claude.ai.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::http::auth::Principal;
use crate::http::{ApiPath, ApiResult, HttpState};
use crate::oauth::{self, GrantInfo};

/// List the connected OAuth clients of the signed-in user.
#[utoipa::path(
    get,
    path = "/connections",
    tag = "tokens",
    summary = "List the connected OAuth clients of the signed-in user.",
    description = "Every live grant that has been used to obtain tokens: the client's name, when it was connected and when it last refreshed its tokens.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Connected clients", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<Vec<GrantInfo>>> {
    Ok(Json(
        oauth::list_grants(&state.app.ctx(principal.actor)).await?,
    ))
}

/// Disconnect an OAuth client.
#[utoipa::path(
    delete,
    path = "/connections/{id}",
    tag = "tokens",
    summary = "Disconnect an OAuth client.",
    description = "Ends the grant: its access and refresh tokens stop working at once. The client has to ask for consent again to reconnect.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Grant ID"),
    ),
    responses(
        (status = 204, description = "Client disconnected"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Grant not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<StatusCode> {
    oauth::revoke_grant(&state.app.ctx(principal.actor), &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
