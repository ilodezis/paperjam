//! The token endpoint (authorization code + PKCE, refresh-token rotation)
//! and the revocation endpoint.

use axum::Form;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::hashing::sha256_hex;
use crate::http::HttpState;
use crate::ids::UserId;
use crate::oauth::{
    ACCESS_TTL, Client, OAuthError, REFRESH_TTL, access_token_grant, find_client,
    mint_access_token, no_store, revoke_grant_system, secrets_match,
};
use crate::service::App;
use crate::{random, time};

const REFRESH_PREFIX: &str = "pjr_";
const REFRESH_CHARS: usize = 48;

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

#[derive(Deserialize)]
pub struct RevokeForm {
    token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Credentials from `Authorization: Basic` or from the form body.
fn client_credentials(
    headers: &HeaderMap,
    form_id: Option<&str>,
    form_secret: Option<&str>,
) -> Result<(String, Option<String>), OAuthError> {
    if let Some(basic) = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    {
        let decoded = STANDARD
            .decode(basic.trim())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
        let (id, secret) = decoded
            .split_once(':')
            .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
        return Ok((id.to_string(), Some(secret.to_string())));
    }
    let id = form_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| OAuthError::invalid_client("client_id is required"))?;
    Ok((id.to_string(), form_secret.map(str::to_string)))
}

/// Identifies the calling client; confidential ones must prove the secret.
async fn authenticate_client(
    app: &App,
    headers: &HeaderMap,
    form_id: Option<&str>,
    form_secret: Option<&str>,
) -> Result<Client, OAuthError> {
    let (id, secret) = client_credentials(headers, form_id, form_secret)?;
    let client = find_client(app, &id)
        .await?
        .ok_or_else(|| OAuthError::invalid_client("unknown client"))?;
    match (&client.secret_hash, secret) {
        (Some(expected), Some(given)) => {
            if !secrets_match(expected, &sha256_hex(given.as_bytes())) {
                return Err(OAuthError::invalid_client("wrong client secret"));
            }
        }
        (Some(_), None) => return Err(OAuthError::invalid_client("this client needs its secret")),
        (None, Some(_)) => {
            return Err(OAuthError::invalid_client("this client has no secret"));
        }
        (None, None) => {}
    }
    Ok(client)
}

fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let valid_shape = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    if !valid_shape {
        return false;
    }
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    secrets_match(&computed, challenge)
}

/// What a successful exchange hands back.
struct Issued {
    access_token: String,
    refresh_token: String,
}

async fn issue_tokens(
    app: &App,
    client: &Client,
    user_id: UserId,
    grant_id: &str,
    tx: &mut sqlx::SqliteConnection,
) -> Result<Issued, OAuthError> {
    let refresh = format!("{REFRESH_PREFIX}{}", random::base62(REFRESH_CHARS));
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO oauth_refresh_tokens (token_hash, grant_id, created_at, expires_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(sha256_hex(refresh.as_bytes()))
    .bind(grant_id)
    .bind(time::ts(now))
    .bind(time::ts(now + REFRESH_TTL))
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE oauth_clients SET last_used_at = ? WHERE id = ?")
        .bind(time::ts(now))
        .bind(&client.id)
        .execute(&mut *tx)
        .await?;
    let (access_token, _) = mint_access_token(app, &client.id, user_id, grant_id);
    Ok(Issued {
        access_token,
        refresh_token: refresh,
    })
}

fn token_response(issued: Issued) -> Response {
    let mut response = Json(json!({
        "access_token": issued.access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL.num_seconds(),
        "refresh_token": issued.refresh_token,
    }))
    .into_response();
    no_store(&mut response);
    response
}

/// `POST /oauth/token`
pub async fn token(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Result<Response, OAuthError> {
    let app = &*state.app;
    let client = authenticate_client(
        app,
        &headers,
        form.client_id.as_deref(),
        form.client_secret.as_deref(),
    )
    .await?;
    if !client.approved {
        return Err(OAuthError::bad_request(
            "unauthorized_client",
            "this client has not been approved by a user",
        ));
    }
    match form.grant_type.as_deref() {
        Some("authorization_code") => exchange_code(app, &client, &form).await,
        Some("refresh_token") => refresh(app, &client, &form).await,
        _ => Err(OAuthError::bad_request(
            "unsupported_grant_type",
            "grant_type must be authorization_code or refresh_token",
        )),
    }
}

async fn exchange_code(
    app: &App,
    client: &Client,
    form: &TokenForm,
) -> Result<Response, OAuthError> {
    let missing =
        |what: &str| OAuthError::bad_request("invalid_request", format!("{what} is required"));
    let code = form.code.as_deref().ok_or_else(|| missing("code"))?;
    let redirect_uri = form
        .redirect_uri
        .as_deref()
        .ok_or_else(|| missing("redirect_uri"))?;
    let verifier = form
        .code_verifier
        .as_deref()
        .ok_or_else(|| missing("code_verifier"))?;
    let hash = sha256_hex(code.as_bytes());

    let mut tx = app.db.write.begin().await?;
    // A code works once: whoever claims it first wins, every later try fails.
    let claimed: Option<(String, String, String, String)> = sqlx::query_as(
        "UPDATE oauth_codes SET used_at = ? WHERE code_hash = ? AND used_at IS NULL \
         RETURNING grant_id, redirect_uri, code_challenge, expires_at",
    )
    .bind(time::now_ts())
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((grant_id, bound_redirect, challenge, expires_at)) = claimed else {
        // A code shown twice means somebody else holds a copy: end the grant.
        let replayed: Option<(String,)> =
            sqlx::query_as("SELECT grant_id FROM oauth_codes WHERE code_hash = ?")
                .bind(&hash)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        if let Some((grant_id,)) = replayed {
            revoke_grant_system(app, &grant_id, "authorization code replayed").await?;
        }
        return Err(OAuthError::invalid_grant(
            "the code is invalid or already used",
        ));
    };
    let grant: Option<(String, UserId)> = sqlx::query_as(
        "SELECT g.client_id, g.user_id FROM oauth_grants g \
         JOIN users u ON u.id = g.user_id \
         WHERE g.id = ? AND g.revoked_at IS NULL AND u.disabled_at IS NULL",
    )
    .bind(&grant_id)
    .fetch_optional(&mut *tx)
    .await?;
    let valid = grant.as_ref().is_some_and(|(owner, _)| *owner == client.id)
        && expires_at >= time::now_ts()
        && bound_redirect == redirect_uri
        && pkce_matches(verifier, &challenge);
    let Some((_, user_id)) = grant.filter(|_| valid) else {
        tx.commit().await?;
        return Err(OAuthError::invalid_grant(
            "the code does not match this request",
        ));
    };
    let issued = issue_tokens(app, client, user_id, &grant_id, &mut tx).await?;
    tx.commit().await?;
    Ok(token_response(issued))
}

async fn refresh(app: &App, client: &Client, form: &TokenForm) -> Result<Response, OAuthError> {
    let presented = form
        .refresh_token
        .as_deref()
        .ok_or_else(|| OAuthError::bad_request("invalid_request", "refresh_token is required"))?;
    let hash = sha256_hex(presented.as_bytes());
    let now = time::now_ts();

    let mut tx = app.db.write.begin().await?;
    // Rotation: a refresh token is good for exactly one refresh.
    let claimed: Option<(String,)> = sqlx::query_as(
        "UPDATE oauth_refresh_tokens SET used_at = ? \
         WHERE token_hash = ? AND used_at IS NULL AND expires_at > ? \
           AND grant_id IN ( \
               SELECT g.id FROM oauth_grants g JOIN users u ON u.id = g.user_id \
               WHERE g.client_id = ? AND g.revoked_at IS NULL AND u.disabled_at IS NULL) \
         RETURNING grant_id",
    )
    .bind(&now)
    .bind(&hash)
    .bind(&now)
    .bind(&client.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((grant_id,)) = claimed else {
        // An old token coming back means it leaked: end the whole grant.
        let replayed: Option<(String,)> = sqlx::query_as(
            "SELECT r.grant_id FROM oauth_refresh_tokens r \
             JOIN oauth_grants g ON g.id = r.grant_id \
             WHERE r.token_hash = ? AND r.used_at IS NOT NULL AND g.client_id = ?",
        )
        .bind(&hash)
        .bind(&client.id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        if let Some((grant_id,)) = replayed {
            revoke_grant_system(app, &grant_id, "refresh token replayed").await?;
        }
        return Err(OAuthError::invalid_grant(
            "the refresh token is invalid, expired or revoked",
        ));
    };
    let (user_id,): (UserId,) = sqlx::query_as("SELECT user_id FROM oauth_grants WHERE id = ?")
        .bind(&grant_id)
        .fetch_one(&mut *tx)
        .await?;
    let issued = issue_tokens(app, client, user_id, &grant_id, &mut tx).await?;
    tx.commit().await?;
    Ok(token_response(issued))
}

/// `POST /oauth/revoke` (RFC 7009). Answers 200 whatever it was given, so it
/// cannot be used to probe for valid tokens.
pub async fn revoke(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Form(form): Form<RevokeForm>,
) -> Result<Response, OAuthError> {
    let app = &*state.app;
    let client = authenticate_client(
        app,
        &headers,
        form.client_id.as_deref(),
        form.client_secret.as_deref(),
    )
    .await?;
    if let Some(token) = form.token.as_deref() {
        let grant = if let Some((grant_id, owner)) = access_token_grant(app, token) {
            (owner == client.id).then_some(grant_id)
        } else {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT r.grant_id FROM oauth_refresh_tokens r \
                 JOIN oauth_grants g ON g.id = r.grant_id \
                 WHERE r.token_hash = ? AND g.client_id = ?",
            )
            .bind(sha256_hex(token.as_bytes()))
            .bind(&client.id)
            .fetch_optional(&app.db.read)
            .await?;
            row.map(|(id,)| id)
        };
        if let Some(grant_id) = grant {
            revoke_grant_system(app, &grant_id, "revoked by the client").await?;
        }
    }
    let mut response = StatusCode::OK.into_response();
    no_store(&mut response);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_follows_rfc_7636_appendix_b() {
        // The worked example of the RFC.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches(
            verifier,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cN"
        ));
        // The verifier itself is not an acceptable challenge ("plain").
        assert!(!pkce_matches(verifier, verifier));
        assert!(!pkce_matches("short", challenge));
    }
}
