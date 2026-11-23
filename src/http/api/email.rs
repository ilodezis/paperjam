//! Inbound email webhook (`POST /api/v1/ingest/email`) and admin settings
//! endpoints (`/api/v1/settings/email*`).

use std::net::SocketAddr;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{AUTHORIZATION, HOST};
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;

use crate::email::EmailIngestReport;
use crate::error::ServiceError;
use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiResult, HttpState};
use crate::service::settings::{self, CreatedEmailSecret, EmailSettingsPatch, EmailSettingsView};

fn endpoint_url(state: &HttpState, headers: &HeaderMap) -> String {
    let host = headers.get(HOST).and_then(|h| h.to_str().ok());
    format!(
        "{}/api/v1/ingest/email",
        crate::service::links::base_url(&state.app, host)
    )
}

/// Ingest a raw RFC 5322 email message (`message/rfc822`).
///
/// Authenticated with the write-only email intake bearer secret,
/// optionally constrained by an allowed-senders list.
#[utoipa::path(
    post,
    path = "/ingest/email",
    tag = "documents",
    summary = "Ingest attachments (or the body) of a raw RFC 5322 email.",
    params(
        ("X-Envelope-From" = Option<String>, Header, description = "SMTP envelope sender verified by the mail relay"),
    ),
    request_body(content = String, content_type = "message/rfc822"),
    responses(
        (status = 200, description = "Email processed", body = EmailIngestReport),
        (status = 401, description = "Invalid or missing email ingest secret", body = crate::http::ErrorResponse),
        (status = 403, description = "Sender not in allowed senders list", body = crate::http::ErrorResponse),
        (status = 413, description = "Email exceeds PAPERJAM_MAX_UPLOAD_BYTES", body = crate::http::ErrorResponse),
        (status = 422, description = "Malformed email message", body = crate::http::ErrorResponse),
        (status = 429, description = "Too many failed attempts", body = crate::http::ErrorResponse),
        (status = 503, description = "Email intake is not configured or target user is unavailable", body = crate::http::ErrorResponse),
    )
)]
pub async fn ingest(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<EmailIngestReport>> {
    let cfg = state.app.runtime_settings().email;
    let verifier = if cfg.enabled {
        cfg.secret.as_ref()
    } else {
        None
    };
    let Some(verifier) = verifier else {
        return Err(ServiceError::unavailable(
            "email_not_configured",
            "email intake is not configured",
        )
        .into());
    };

    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_email_ingest(ip)?;

    let presented = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if !presented.is_some_and(|token| verifier.matches(token)) {
        state.limiter.email_ingest_failed(ip);
        return Err(ServiceError::unauthorized(
            "invalid_email_secret",
            "invalid or missing email ingest secret",
        )
        .into());
    }
    state.limiter.email_ingest_succeeded(ip);

    let max_bytes = usize::try_from(state.app.config.max_upload_bytes).unwrap_or(usize::MAX);
    let mut raw = Vec::new();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            ServiceError::invalid(
                "invalid_upload",
                format!("could not read the email body: {e}"),
            )
        })?;
        if raw.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ServiceError::payload_too_large(
                "file_too_large",
                format!("email exceeds the {max_bytes} byte limit"),
            )
            .into());
        }
        raw.extend_from_slice(&chunk);
    }

    // A present but non-ASCII header counts as empty, so the allowlist rejects
    // it instead of silently falling back to `From:`.
    let envelope_from = headers
        .get("x-envelope-from")
        .map(|v| v.to_str().unwrap_or(""));
    let report = crate::email::ingest(&state.app, &cfg, &raw, envelope_from).await?;
    if let Err(e) = settings::record_email_received(&state.app).await {
        tracing::warn!(error = %e, "failed to record email intake receipt timestamp");
    }
    Ok(Json(report))
}

/// View current inbound email settings (administrators only).
#[utoipa::path(
    get,
    path = "/settings/email",
    tag = "settings",
    summary = "View inbound email intake settings (administrators only).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Current inbound email settings", body = EmailSettingsView),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges required", body = crate::http::ErrorResponse),
    )
)]
pub async fn get_settings(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult<Json<EmailSettingsView>> {
    principal.require_admin()?;
    let url = endpoint_url(&state, &headers);
    let view = settings::get_email_settings(&state.app.ctx(principal.actor), &url).await?;
    Ok(Json(view))
}

/// Update inbound email settings (administrators only, web session only).
#[utoipa::path(
    patch,
    path = "/settings/email",
    tag = "settings",
    summary = "Update inbound email intake settings (administrators only).",
    description = "Needs a signed-in web session: an API token cannot modify email intake settings.",
    security(
        ("session_cookie" = [])
    ),
    request_body = EmailSettingsPatch,
    responses(
        (status = 200, description = "Updated inbound email settings", body = EmailSettingsView),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
        (status = 404, description = "Target user not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid settings or missing secret when enabling", body = crate::http::ErrorResponse),
    )
)]
pub async fn patch_settings(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
    ApiJson(patch): ApiJson<EmailSettingsPatch>,
) -> ApiResult<Json<EmailSettingsView>> {
    principal.require_admin_session()?;
    let url = endpoint_url(&state, &headers);
    let view =
        settings::update_email_settings(&state.app.ctx(principal.actor), patch, &url).await?;
    Ok(Json(view))
}

/// Generate or rotate the inbound email webhook secret (administrators only, web session only).
#[utoipa::path(
    post,
    path = "/settings/email/secret",
    tag = "settings",
    summary = "Generate or rotate the email intake webhook secret (administrators only).",
    description = "Needs a signed-in web session. The secret value is returned in this response only.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 201, description = "Generated or rotated email intake secret", body = CreatedEmailSecret),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
    )
)]
pub async fn rotate_secret(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<CreatedEmailSecret>)> {
    principal.require_admin_session()?;
    let url = endpoint_url(&state, &headers);
    let created = settings::rotate_email_secret(&state.app.ctx(principal.actor), &url).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Revoke the inbound email webhook secret and disable email intake (administrators only, web session only).
#[utoipa::path(
    delete,
    path = "/settings/email/secret",
    tag = "settings",
    summary = "Revoke the email intake webhook secret and disable intake (administrators only).",
    description = "Needs a signed-in web session. Any configured secret stops working immediately.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 200, description = "Revoked email intake secret and disabled inbound email", body = EmailSettingsView),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Administrator privileges and a web session required", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke_secret(
    State(state): State<HttpState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult<Json<EmailSettingsView>> {
    principal.require_admin_session()?;
    let url = endpoint_url(&state, &headers);
    let view = settings::revoke_email_secret(&state.app.ctx(principal.actor), &url).await?;
    Ok(Json(view))
}
