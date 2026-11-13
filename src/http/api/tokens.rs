//! API tokens of the signed-in user.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiResult, HttpState};
use crate::ids::TokenId;
use crate::model::user::TokenView;
use crate::service::tokens;

/// List API tokens of the signed-in user.
#[utoipa::path(
    get,
    path = "/tokens",
    tag = "tokens",
    summary = "List API tokens of the signed-in user.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "List of user tokens", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<Vec<TokenView>>> {
    Ok(Json(
        tokens::list_tokens(&state.app.ctx(principal.actor)).await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewToken {
    pub name: String,
}

/// Creates a token. The secret value is in this response only.
#[utoipa::path(
    post,
    path = "/tokens",
    tag = "tokens",
    summary = "Creates a token. The secret value is in this response only.",
    description = "Needs a signed-in web session: a token cannot issue further tokens.",
    security(
        ("session_cookie" = [])
    ),
    request_body = NewToken,
    responses(
        (status = 201, description = "Token created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Called with a token instead of a web session", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid token request", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewToken>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (view, secret) = tokens::create_token(&state.app.ctx(principal.actor), &new.name).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"token": view, "secret": secret})),
    ))
}

/// Revoke an API token.
#[utoipa::path(
    delete,
    path = "/tokens/{id}",
    tag = "tokens",
    summary = "Revoke an API token.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Token ID"),
    ),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Token not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<TokenId>,
) -> ApiResult<StatusCode> {
    tokens::revoke_token(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
