//! Inbound email webhook: `POST /api/v1/ingest/email`.

use std::net::SocketAddr;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use futures_util::StreamExt;
use secrecy::ExposeSecret;

use crate::email::EmailIngestReport;
use crate::error::ServiceError;
use crate::http::{ApiResult, HttpState};
use crate::signing::constant_time_eq;

/// Ingest a raw RFC 5322 email message (`message/rfc822`).
///
/// Authenticated with the write-only `PAPERJAM_EMAIL_SECRET` bearer token,
/// optionally constrained by `PAPERJAM_EMAIL_ALLOWED_SENDERS`.
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
        (status = 403, description = "Sender not in PAPERJAM_EMAIL_ALLOWED_SENDERS", body = crate::http::ErrorResponse),
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
    let Some(config) = &state.app.config.email_intake else {
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
    if !presented.is_some_and(|token| {
        constant_time_eq(token.as_bytes(), config.secret.expose_secret().as_bytes())
    }) {
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

    let envelope_from = headers.get("x-envelope-from").and_then(|v| v.to_str().ok());
    let report = crate::email::ingest(&state.app, config, &raw, envelope_from).await?;
    Ok(Json(report))
}
