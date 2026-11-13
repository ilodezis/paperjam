//! Passkeys of the signed-in user. Web session only: a token or OAuth client
//! is refused, agents never manage sign-in credentials.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiResult, HttpState};
use crate::ids::PasskeyId;
use crate::model::user::PasskeyView;
use crate::service::passkeys;

/// Passkeys of the signed-in user, and whether passkeys are available.
#[utoipa::path(
    get,
    path = "/passkeys",
    tag = "passkeys",
    summary = "Passkeys of the signed-in user, and whether passkeys are available.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 200, description = "Availability, relying party and the passkeys", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Needs a web session, not a token", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<passkeys::PasskeyList>> {
    Ok(Json(passkeys::list(&state.app.ctx(principal.actor)).await?))
}

/// Starts adding a passkey: WebAuthn creation options and a ceremony id.
#[utoipa::path(
    post,
    path = "/passkeys/challenge",
    tag = "passkeys",
    summary = "Starts adding a passkey: WebAuthn creation options and a ceremony id.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 200, description = "Ceremony id and `publicKey` options for navigator.credentials.create()", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Needs a web session, not a token", body = crate::http::ErrorResponse),
        (status = 503, description = "Passkeys are not configured (PAPERJAM_PUBLIC_URL)", body = crate::http::ErrorResponse),
    )
)]
pub async fn challenge(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<passkeys::Challenge>> {
    Ok(Json(
        passkeys::begin_registration(&state.app.ctx(principal.actor)).await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewPasskey {
    /// The id from `/passkeys/challenge`.
    pub ceremony: String,
    /// A name to recognize the passkey by, such as the device.
    pub label: String,
    /// The `PublicKeyCredential` from navigator.credentials.create(), as JSON.
    #[schema(value_type = Object)]
    pub credential: RegisterPublicKeyCredential,
}

/// Finishes adding a passkey to the signed-in account.
#[utoipa::path(
    post,
    path = "/passkeys",
    tag = "passkeys",
    summary = "Finishes adding a passkey to the signed-in account.",
    security(
        ("session_cookie" = [])
    ),
    request_body = NewPasskey,
    responses(
        (status = 201, description = "Passkey added", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Needs a web session, not a token", body = crate::http::ErrorResponse),
        (status = 409, description = "Passkey already registered", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid label or credential, or an expired ceremony", body = crate::http::ErrorResponse),
        (status = 503, description = "Passkeys are not configured (PAPERJAM_PUBLIC_URL)", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewPasskey>,
) -> ApiResult<(StatusCode, Json<PasskeyView>)> {
    let view = passkeys::finish_registration(
        &state.app.ctx(principal.actor),
        &new.ceremony,
        &new.credential,
        &new.label,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Removes a passkey of the signed-in user.
#[utoipa::path(
    delete,
    path = "/passkeys/{id}",
    tag = "passkeys",
    summary = "Removes a passkey of the signed-in user.",
    security(
        ("session_cookie" = [])
    ),
    params(
        ("id" = String, Path, description = "Passkey ID"),
    ),
    responses(
        (status = 204, description = "Passkey removed"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 403, description = "Needs a web session, not a token", body = crate::http::ErrorResponse),
        (status = 404, description = "Passkey not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PasskeyId>,
) -> ApiResult<StatusCode> {
    passkeys::revoke(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}
