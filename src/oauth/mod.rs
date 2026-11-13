//! The OAuth 2.1 authorization server for remote MCP clients: dynamic client
//! registration, authorization code + PKCE (S256 only), refresh-token rotation
//! and revocation. Access tokens carry the granting user's full access; there
//! are no scopes because the archive has no permission model to scope against.

pub mod authorize;
pub mod dcr;
pub mod metadata;
pub mod token;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::http::HttpState;
use crate::ids::UserId;
use crate::service::{Actor, App, Ctx, Via, events};
use crate::{signing, time};

/// Access tokens start with this; API tokens start with `pj_`.
pub const ACCESS_PREFIX: &str = "pjo_";
const ACCESS_PURPOSE: &str = "oauth-access";
/// How long an access token works.
pub const ACCESS_TTL: Duration = Duration::hours(1);
/// How long a client has to trade a code for tokens.
pub const CODE_TTL: Duration = Duration::minutes(10);
/// How long a refresh token works, counted from its issue.
pub const REFRESH_TTL: Duration = Duration::days(90);
/// A client nobody approved is deleted after this long.
pub const UNAPPROVED_CLIENT_TTL: Duration = Duration::hours(24);

pub fn router() -> Router<HttpState> {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(metadata::authorization_server),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(metadata::protected_resource),
        )
        // RFC 9728 clients ask for the metadata of the resource they were
        // given, which is `/mcp`.
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(metadata::protected_resource),
        )
        .route(
            "/oauth/register",
            post(dcr::register).layer(DefaultBodyLimit::max(dcr::MAX_REGISTRATION_BYTES)),
        )
        .route(
            "/oauth/authorize",
            get(authorize::show).post(authorize::decide),
        )
        .route("/oauth/token", post(token::token))
        .route("/oauth/revoke", post(token::revoke))
}

/// An OAuth error as the protocol wants it: `{"error", "error_description"}`.
#[derive(Debug)]
pub struct OAuthError {
    pub status: StatusCode,
    pub error: &'static str,
    pub description: String,
}

impl OAuthError {
    pub fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            error,
            description: description.into(),
        }
    }

    pub fn bad_request(error: &'static str, description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error, description)
    }

    pub fn invalid_grant(description: impl Into<String>) -> Self {
        Self::bad_request("invalid_grant", description)
    }

    pub fn invalid_client(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_client", description)
    }
}

impl From<ServiceError> for OAuthError {
    fn from(e: ServiceError) -> Self {
        // Details of an internal failure stay in the logs.
        tracing::error!(code = e.code, "oauth request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "the request could not be completed",
        )
    }
}

impl From<sqlx::Error> for OAuthError {
    fn from(e: sqlx::Error) -> Self {
        ServiceError::from(e).into()
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({"error": self.error, "error_description": self.description})),
        )
            .into_response();
        no_store(&mut response);
        response
    }
}

/// Token responses and errors must never be cached.
pub fn no_store(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

fn key(app: &App) -> &[u8] {
    app.config.secret_key.expose_secret().as_bytes()
}

/// Constant-time comparison of two secrets' hashes.
pub fn secrets_match(a: &str, b: &str) -> bool {
    let (a, b) = (sha256_hex(a.as_bytes()), sha256_hex(b.as_bytes()));
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}

// --- clients ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    /// SHA-256 of the client secret; public clients have none.
    pub secret_hash: Option<String>,
    /// A person said yes on the consent page (or the operator configured it).
    pub approved: bool,
    /// The client registered itself through `/oauth/register`; false for the
    /// one the operator configured.
    pub self_registered: bool,
    /// When the client's row was created, as stored.
    pub created_at: String,
}

impl Client {
    pub fn allows_redirect(&self, uri: &str) -> bool {
        self.redirect_uris.iter().any(|u| u == uri)
    }
}

/// Makes sure the operator's static client has its row, so grants can point at it.
async fn ensure_static_client(app: &App) -> Result<()> {
    let Some(client) = &app.config.oauth_static_client else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO oauth_clients (id, name, redirect_uris, secret_hash, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET redirect_uris = excluded.redirect_uris, \
             secret_hash = excluded.secret_hash",
    )
    .bind(&client.id)
    .bind(&client.id)
    .bind(json!(client.redirect_uris).to_string())
    .bind(sha256_hex(client.secret.expose_secret().as_bytes()))
    .bind(time::now_ts())
    .execute(&app.db.write)
    .await?;
    Ok(())
}

/// `id, name, redirect_uris, secret_hash, approved_by, created_at` of an `oauth_clients` row.
type ClientRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

/// A client that exists and has not been revoked.
pub async fn find_client(app: &App, id: &str) -> Result<Option<Client>> {
    let is_static = app
        .config
        .oauth_static_client
        .as_ref()
        .is_some_and(|c| c.id == id);
    if is_static {
        ensure_static_client(app).await?;
    }
    let row: Option<ClientRow> = sqlx::query_as(
        "SELECT id, name, redirect_uris, secret_hash, approved_by, created_at \
         FROM oauth_clients WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&app.db.read)
    .await?;
    Ok(row.map(
        |(id, name, uris, secret_hash, approved_by, created_at)| Client {
            approved: is_static || approved_by.is_some(),
            self_registered: !is_static,
            created_at,
            redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
            id,
            name,
            secret_hash,
        },
    ))
}

/// Deletes what has outlived its use: clients nobody approved within a day,
/// expired codes and expired refresh tokens.
pub async fn purge_stale(app: &App) -> Result<()> {
    let now = Utc::now();
    let static_id = app
        .config
        .oauth_static_client
        .as_ref()
        .map(|c| c.id.clone())
        .unwrap_or_default();
    sqlx::query(
        "DELETE FROM oauth_clients WHERE approved_by IS NULL AND created_at < ? AND id != ?",
    )
    .bind(time::ts(now - UNAPPROVED_CLIENT_TTL))
    .bind(static_id)
    .execute(&app.db.write)
    .await?;
    let now = time::ts(now);
    sqlx::query("DELETE FROM oauth_codes WHERE expires_at < ?")
        .bind(&now)
        .execute(&app.db.write)
        .await?;
    sqlx::query("DELETE FROM oauth_refresh_tokens WHERE expires_at < ?")
        .bind(&now)
        .execute(&app.db.write)
        .await?;
    Ok(())
}

// --- access tokens -----------------------------------------------------------------

/// What an access token says; the database still decides whether it is alive.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    #[serde(rename = "c")]
    client_id: String,
    #[serde(rename = "u")]
    user_id: UserId,
    #[serde(rename = "g")]
    grant_id: String,
    #[serde(rename = "e")]
    expires: i64,
}

fn invalid_token() -> ServiceError {
    ServiceError::unauthorized(
        "invalid_token",
        "the access token is invalid or has expired",
    )
}

/// A signed token bound to `(client, user, grant)`: `pjo_<claims>.<signature>`.
pub fn mint_access_token(
    app: &App,
    client_id: &str,
    user_id: UserId,
    grant_id: &str,
) -> (String, DateTime<Utc>) {
    let expires = Utc::now() + ACCESS_TTL;
    let claims = Claims {
        client_id: client_id.to_string(),
        user_id,
        grant_id: grant_id.to_string(),
        expires: expires.timestamp(),
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
    let signature = signing::sign(key(app), ACCESS_PURPOSE, &payload);
    (format!("{ACCESS_PREFIX}{payload}.{signature}"), expires)
}

fn open_claims(app: &App, raw: &str) -> Option<Claims> {
    let rest = raw.strip_prefix(ACCESS_PREFIX)?;
    let (payload, signature) = rest.split_once('.')?;
    if !signing::verify(key(app), ACCESS_PURPOSE, payload, signature) {
        return None;
    }
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}

/// Who an access token belongs to, if it is genuine, fresh and its grant is
/// still alive. Revoking the grant, the client or disabling the user ends it.
pub async fn resolve_access_token(app: &App, raw: &str) -> Result<Actor> {
    let claims = open_claims(app, raw).ok_or_else(invalid_token)?;
    if claims.expires < Utc::now().timestamp() {
        return Err(invalid_token());
    }
    let alive: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM oauth_grants g \
         JOIN oauth_clients c ON c.id = g.client_id \
         JOIN users u ON u.id = g.user_id \
         WHERE g.id = ? AND g.client_id = ? AND g.user_id = ? \
           AND g.revoked_at IS NULL AND c.revoked_at IS NULL AND u.disabled_at IS NULL",
    )
    .bind(&claims.grant_id)
    .bind(&claims.client_id)
    .bind(claims.user_id)
    .fetch_optional(&app.db.read)
    .await?;
    alive.ok_or_else(invalid_token)?;
    Ok(Actor::user(claims.user_id, Via::OAuth))
}

/// The grant an access token was minted for, if its signature is genuine
/// (used to revoke by presenting the token; expiry does not matter there).
pub fn access_token_grant(app: &App, raw: &str) -> Option<(String, String)> {
    open_claims(app, raw).map(|c| (c.grant_id, c.client_id))
}

// --- grants ------------------------------------------------------------------------

/// A connected client, as the account page lists it.
#[derive(Debug, Serialize)]
pub struct GrantInfo {
    pub id: String,
    pub client_id: String,
    pub client_name: String,
    pub created_at: String,
    /// When the client last obtained tokens (any of its grants).
    pub last_used_at: Option<String>,
}

/// The signed-in user's live grants that have been put to use.
pub async fn list_grants(ctx: &Ctx<'_>) -> Result<Vec<GrantInfo>> {
    let user_id = ctx.actor.require_user()?;
    let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT g.id, g.client_id, c.name, g.created_at, c.last_used_at \
         FROM oauth_grants g JOIN oauth_clients c ON c.id = g.client_id \
         WHERE g.user_id = ? AND g.revoked_at IS NULL AND c.revoked_at IS NULL \
           AND EXISTS (SELECT 1 FROM oauth_refresh_tokens r WHERE r.grant_id = g.id) \
         ORDER BY g.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(&ctx.app.db.read)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, client_id, client_name, created_at, last_used_at)| GrantInfo {
                id,
                client_id,
                client_name,
                created_at,
                last_used_at,
            },
        )
        .collect())
}

/// Ends a grant of the signed-in user: its access tokens and refresh tokens
/// stop working at once.
pub async fn revoke_grant(ctx: &Ctx<'_>, grant_id: &str) -> Result<()> {
    let user_id = ctx.actor.require_user()?;
    let mut tx = ctx.app.db.write.begin().await?;
    // Another user's grant is reported as missing rather than forbidden.
    let updated = sqlx::query(
        "UPDATE oauth_grants SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(time::now_ts())
    .bind(grant_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ServiceError::not_found("grant_not_found", "no such grant"));
    }
    events::record(&mut tx, &ctx.actor, "oauth.grant.revoke", None, json!({})).await?;
    tx.commit().await?;
    Ok(())
}

/// Revokes a grant without a signed-in user: the client presented its own
/// token (revocation endpoint) or a token was replayed (reuse detection).
pub(crate) async fn revoke_grant_system(app: &App, grant_id: &str, reason: &str) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    let updated =
        sqlx::query("UPDATE oauth_grants SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(time::now_ts())
            .bind(grant_id)
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() > 0 {
        events::record(
            &mut tx,
            &Actor::system(),
            "oauth.grant.revoke",
            None,
            json!({"reason": reason}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::test_app;

    async fn insert_client(app: &App, id: &str, approved: bool, created_at: &str) {
        let approver: Option<UserId> = if approved {
            let user = crate::service::users::create_user(
                &app.ctx(Actor::cli()),
                crate::model::user::NewUser {
                    username: format!("u-{id}"),
                    display_name: "U".into(),
                    password: secrecy::SecretString::from("correct horse battery".to_string()),
                    locale: "en".into(),
                    is_admin: false,
                },
            )
            .await
            .unwrap();
            Some(user.id)
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO oauth_clients (id, name, redirect_uris, approved_by, created_at) \
             VALUES (?, 'c', '[]', ?, ?)",
        )
        .bind(id)
        .bind(approver)
        .bind(created_at)
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unapproved_clients_expire_after_a_day() {
        let (_dir, app) = test_app().await;
        let old = time::ts(Utc::now() - Duration::hours(25));
        let fresh = time::ts(Utc::now() - Duration::hours(23));
        insert_client(&app, "old-unapproved", false, &old).await;
        insert_client(&app, "fresh-unapproved", false, &fresh).await;
        insert_client(&app, "old-approved", true, &old).await;

        purge_stale(&app).await.unwrap();

        let left: Vec<(String,)> = sqlx::query_as("SELECT id FROM oauth_clients ORDER BY id")
            .fetch_all(&app.db.read)
            .await
            .unwrap();
        let left: Vec<&str> = left.iter().map(|(id,)| id.as_str()).collect();
        assert_eq!(left, ["fresh-unapproved", "old-approved"]);
    }

    #[tokio::test]
    async fn access_tokens_expire_and_cannot_be_forged() {
        let (_dir, app) = test_app().await;
        let user = UserId::new();
        let (token, _) = mint_access_token(&app, "client", user, "grant");
        assert!(open_claims(&app, &token).is_some());
        assert!(open_claims(&app, &format!("{token}0")).is_none());
        assert!(open_claims(&app, "pjo_nonsense").is_none());
        assert!(open_claims(&app, &token.replacen("pjo_", "pj_", 1)).is_none());

        // A well-signed token past its expiry is refused before the database is asked.
        let claims = Claims {
            client_id: "client".into(),
            user_id: user,
            grant_id: "grant".into(),
            expires: Utc::now().timestamp() - 1,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signature = signing::sign(key(&app), ACCESS_PURPOSE, &payload);
        let expired = format!("{ACCESS_PREFIX}{payload}.{signature}");
        let err = resolve_access_token(&app, &expired).await.unwrap_err();
        assert_eq!(err.code, "invalid_token");
        // Genuine and fresh, but no grant behind it.
        let err = resolve_access_token(&app, &token).await.unwrap_err();
        assert_eq!(err.code, "invalid_token");
    }

    #[test]
    fn secrets_compare_by_value() {
        assert!(secrets_match("abc", "abc"));
        assert!(!secrets_match("abc", "abd"));
        assert!(!secrets_match("abc", "abcd"));
    }
}
