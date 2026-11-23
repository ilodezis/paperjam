//! Runtime settings REST endpoints (`/api/v1/settings*`).

use axum::Json;
use axum::extract::State;

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiResult, HttpState};
use crate::service::settings::{self, AdminSettingsPatch, AdminSettingsView, SettingsSummary};

/// Non-sensitive instance settings summary for any signed-in user.
#[utoipa::path(
    get,
    path = "/settings/summary",
    tag = "settings",
    summary = "Get non-sensitive runtime settings summary for the signed-in user.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Non-sensitive settings summary", body = SettingsSummary),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn get_summary(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<SettingsSummary>> {
    let summary = settings::get_settings_summary(&state.app.ctx(principal.actor)).await?;
    Ok(Json(summary))
}

/// View full runtime settings (administrators only).
#[utoipa::path(
    get,
    path = "/settings",
    tag = "settings",
    summary = "View runtime settings for LLM, calendar, and watched inbox folder (administrators only).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Current runtime settings", body = AdminSettingsView),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges required", body = crate::http::ErrorResponse),
    )
)]
pub async fn get_admin(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<AdminSettingsView>> {
    principal.require_admin()?;
    let view = settings::get_admin_settings(&state.app.ctx(principal.actor)).await?;
    Ok(Json(view))
}

/// Update runtime settings (administrators only, web session only).
#[utoipa::path(
    patch,
    path = "/settings",
    tag = "settings",
    summary = "Update runtime settings and hot-reload the active configuration (administrators only).",
    description = "Needs a signed-in web session: an API token cannot modify instance settings.",
    security(
        ("session_cookie" = [])
    ),
    request_body = AdminSettingsPatch,
    responses(
        (status = 200, description = "Updated runtime settings", body = AdminSettingsView),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
        (status = 404, description = "Target user not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid settings value", body = crate::http::ErrorResponse),
    )
)]
pub async fn patch_admin(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(patch): ApiJson<AdminSettingsPatch>,
) -> ApiResult<Json<AdminSettingsView>> {
    principal.require_admin_session()?;
    let view = settings::update_admin_settings(&state.app.ctx(principal.actor), patch).await?;
    Ok(Json(view))
}
