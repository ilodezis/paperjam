//! User administration (administrators only).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use secrecy::SecretString;
use serde::Deserialize;

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiResult, HttpState};
use crate::ids::UserId;
use crate::model::user::{NewUser, User};
use crate::service::users::{self, UserPatch};

/// List all users (administrators only).
#[utoipa::path(
    get,
    path = "/users",
    tag = "users",
    summary = "List all users (administrators only).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "List of users", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges required", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<Vec<User>>> {
    principal.require_admin()?;
    Ok(Json(
        users::list_users(&state.app.ctx(principal.actor)).await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateUser {
    pub username: String,
    pub display_name: Option<String>,
    pub password: String,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default)]
    pub is_admin: bool,
}

fn default_locale() -> String {
    "en".to_string()
}

/// Create a new user (administrators only).
#[utoipa::path(
    post,
    path = "/users",
    tag = "users",
    summary = "Create a new user (administrators only).",
    description = "Needs a signed-in web session: a token cannot create accounts.",
    security(
        ("session_cookie" = [])
    ),
    request_body = CreateUser,
    responses(
        (status = 201, description = "User created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
        (status = 409, description = "Username already taken", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid user data or weak password", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(body): ApiJson<CreateUser>,
) -> ApiResult<(StatusCode, Json<User>)> {
    principal.require_admin()?;
    let user = users::create_user(
        &state.app.ctx(principal.actor),
        NewUser {
            display_name: body.display_name.unwrap_or_else(|| body.username.clone()),
            username: body.username,
            password: SecretString::from(body.password),
            locale: body.locale,
            is_admin: body.is_admin,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(user)))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateUser {
    pub disabled: Option<bool>,
    pub is_admin: Option<bool>,
    pub display_name: Option<String>,
    pub password: Option<String>,
}

/// Update a user account (administrators only, web session only).
#[utoipa::path(
    patch,
    path = "/users/{id}",
    tag = "users",
    summary = "Update a user account (administrators only).",
    description = "Needs a signed-in web session: a token cannot modify accounts, disable users or reset passwords.",
    security(
        ("session_cookie" = [])
    ),
    params(
        ("id" = String, Path, description = "User ID"),
    ),
    request_body = UpdateUser,
    responses(
        (status = 200, description = "Updated user", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
        (status = 404, description = "User not found", body = crate::http::ErrorResponse),
        (status = 409, description = "Cannot disable or demote the last active administrator", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid patch data or self-modification refused", body = crate::http::ErrorResponse),
    )
)]
pub async fn update(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<UserId>,
    ApiJson(body): ApiJson<UpdateUser>,
) -> ApiResult<Json<User>> {
    principal.require_admin()?;
    let user = users::update_user(
        &state.app.ctx(principal.actor),
        id,
        UserPatch {
            disabled: body.disabled,
            is_admin: body.is_admin,
            display_name: body.display_name,
            password: body.password.map(SecretString::from),
        },
    )
    .await?;
    Ok(Json(user))
}

/// Permanently delete a user account (administrators only, web session only).
#[utoipa::path(
    delete,
    path = "/users/{id}",
    tag = "users",
    summary = "Permanently delete a user account (administrators only).",
    description = "Needs a signed-in web session. Removes the account, its sessions, tokens and passkeys; documents remain in the archive.",
    security(
        ("session_cookie" = [])
    ),
    params(
        ("id" = String, Path, description = "User ID"),
    ),
    responses(
        (status = 204, description = "User deleted"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
        (status = 404, description = "User not found", body = crate::http::ErrorResponse),
        (status = 409, description = "Cannot delete the last active administrator", body = crate::http::ErrorResponse),
        (status = 422, description = "Cannot delete your own account", body = crate::http::ErrorResponse),
    )
)]
pub async fn delete(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<UserId>,
) -> ApiResult<StatusCode> {
    principal.require_admin()?;
    users::delete_user(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
