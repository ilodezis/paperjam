//! Short-lived links that carry their own authority: a signed download link
//! for one original file, and a one-time upload link.

use chrono::{DateTime, Duration, Utc};
use secrecy::ExposeSecret;
use serde::Serialize;
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::ids::{DocumentId, UserId};
use crate::service::{App, Ctx, documents, events};
use crate::{random, signing, time};

const DOWNLOAD_PURPOSE: &str = "download-link";
/// How long a download link works.
pub const DOWNLOAD_TTL: Duration = Duration::minutes(10);
/// How long an upload link works.
pub const UPLOAD_TTL: Duration = Duration::minutes(15);
const UPLOAD_TOKEN_CHARS: usize = 40;

fn key(app: &App) -> &[u8] {
    app.config.secret_key.expose_secret().as_bytes()
}

/// The address clients should use to reach this server: the configured
/// public URL, else the `Host` the request came in on, else the bind address.
pub fn base_url(app: &App, host: Option<&str>) -> String {
    if let Some(url) = &app.config.public_url {
        return url.as_str().trim_end_matches('/').to_string();
    }
    match host {
        Some(host) if !host.is_empty() => format!("http://{host}"),
        _ => format!("http://{}", app.config.bind),
    }
}

fn invalid_download_link() -> ServiceError {
    ServiceError::unauthorized(
        "download_link_invalid",
        "this download link is invalid or has expired",
    )
}

/// A signed token for one document: `<document id>.<expiry unix seconds>.<signature>`.
pub fn download_token(app: &App, id: DocumentId) -> (String, DateTime<Utc>) {
    let expires = Utc::now() + DOWNLOAD_TTL;
    let payload = format!("{id}.{}", expires.timestamp());
    let signature = signing::sign(key(app), DOWNLOAD_PURPOSE, &payload);
    (format!("{payload}.{signature}"), expires)
}

/// The document a download token was minted for, if the token is genuine and fresh.
pub fn verify_download_token(app: &App, token: &str) -> Result<DocumentId> {
    let (payload, signature) = token.rsplit_once('.').ok_or_else(invalid_download_link)?;
    if !signing::verify(key(app), DOWNLOAD_PURPOSE, payload, signature) {
        return Err(invalid_download_link());
    }
    let (id, expiry) = payload.split_once('.').ok_or_else(invalid_download_link)?;
    let expiry: i64 = expiry.parse().map_err(|_| invalid_download_link())?;
    if expiry < Utc::now().timestamp() {
        return Err(invalid_download_link());
    }
    id.parse().map_err(|_| invalid_download_link())
}

/// The document behind a download link, while the link is valid and the
/// document is not in the trash: moving a document to the trash revokes the
/// links already handed out for it.
pub async fn open_download_link(app: &App, token: &str) -> Result<DocumentId> {
    let id = verify_download_token(app, token)?;
    let live: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM documents WHERE id = ? AND trashed_at IS NULL")
            .bind(id)
            .fetch_optional(&app.db.read)
            .await?;
    live.map(|_| id).ok_or_else(invalid_download_link)
}

fn invalid_upload_link() -> ServiceError {
    ServiceError::unauthorized(
        "upload_link_invalid",
        "this upload link is invalid, already used, or has expired",
    )
}

/// Mints a one-time upload link for the acting user. Returns the raw token
/// (shown once, stored hashed) and when it expires.
pub async fn create_upload_link(ctx: &Ctx<'_>) -> Result<(String, DateTime<Utc>)> {
    let user_id = ctx.actor.require_user()?;
    let raw = random::base62(UPLOAD_TOKEN_CHARS);
    let expires = Utc::now() + UPLOAD_TTL;
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query("INSERT INTO upload_links (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(sha256_hex(raw.as_bytes()))
        .bind(user_id)
        .bind(time::ts(expires))
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, &ctx.actor, "upload_link.create", None, json!({})).await?;
    tx.commit().await?;
    Ok((raw, expires))
}

/// Uses up an upload link and returns the user it uploads for. Exactly one
/// caller can win a given link, however many race for it.
pub async fn claim_upload_link(app: &App, raw: &str) -> Result<UserId> {
    let now = time::now_ts();
    let user: Option<(UserId,)> = sqlx::query_as(
        "UPDATE upload_links SET used_at = ? \
         WHERE token_hash = ? AND used_at IS NULL AND expires_at > ? \
           AND user_id IN (SELECT id FROM users WHERE disabled_at IS NULL) \
         RETURNING user_id",
    )
    .bind(&now)
    .bind(sha256_hex(raw.as_bytes()))
    .bind(&now)
    .fetch_optional(&app.db.write)
    .await?;
    user.map(|(id,)| id).ok_or_else(invalid_upload_link)
}

/// A download link as handed to a client.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DownloadLink {
    pub url: String,
    /// RFC 3339 UTC.
    pub expires_at: String,
    /// Untrusted display name.
    pub filename: String,
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
}

/// A short-lived download link for the original of `id`; `base_url` is where
/// the client reaches this server.
pub async fn create_download_link(
    ctx: &Ctx<'_>,
    base_url: &str,
    id: DocumentId,
) -> Result<DownloadLink> {
    let file = documents::original_file(ctx, id).await?;
    let (token, expires) = download_token(ctx.app, id);
    Ok(DownloadLink {
        url: format!("{base_url}/api/v1/dl/{token}"),
        expires_at: time::ts(expires),
        filename: file.download_name(),
        mime_type: file.mime,
        size: file.size,
    })
}

/// A one-time upload link as handed to a client.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UploadLink {
    pub url: String,
    /// RFC 3339 UTC.
    pub expires_at: String,
    pub curl_example: String,
}

/// Mints a one-time upload link for the acting user.
pub async fn issue_upload_link(ctx: &Ctx<'_>, base_url: &str) -> Result<UploadLink> {
    let (token, expires) = create_upload_link(ctx).await?;
    let url = format!("{base_url}/api/v1/ul/{token}");
    Ok(UploadLink {
        curl_example: format!("curl --fail -T path/to/file.pdf \"{url}?filename=file.pdf\""),
        expires_at: time::ts(expires),
        url,
    })
}

/// Gives a claimed link back, so a failed upload (wrong file type, a
/// duplicate) can be retried within the link's lifetime.
pub async fn release_upload_link(app: &App, raw: &str) -> Result<()> {
    sqlx::query("UPDATE upload_links SET used_at = NULL WHERE token_hash = ?")
        .bind(sha256_hex(raw.as_bytes()))
        .execute(&app.db.write)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{insert_document, test_app};
    use crate::service::{Actor, Via, users};

    #[tokio::test]
    async fn download_token_round_trips() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        let (token, expires) = download_token(&app, id);
        assert!(expires > Utc::now());
        assert_eq!(verify_download_token(&app, &token).unwrap(), id);
    }

    #[tokio::test]
    async fn moving_a_document_to_the_trash_revokes_its_download_links() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        let (token, _) = download_token(&app, id);
        assert_eq!(open_download_link(&app, &token).await.unwrap(), id);
        sqlx::query("UPDATE documents SET trashed_at = ? WHERE id = ?")
            .bind(time::now_ts())
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        let err = open_download_link(&app, &token).await.unwrap_err();
        assert_eq!(err.code, "download_link_invalid");
        // Restored, the link works again until it expires.
        sqlx::query("UPDATE documents SET trashed_at = NULL WHERE id = ?")
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        assert_eq!(open_download_link(&app, &token).await.unwrap(), id);
    }

    #[tokio::test]
    async fn tampered_expired_and_foreign_tokens_fail() {
        let (_dir, app) = test_app().await;
        let id = insert_document(&app).await;
        let other = insert_document(&app).await;
        let (token, _) = download_token(&app, id);
        assert!(verify_download_token(&app, &format!("{token}0")).is_err());
        assert!(
            verify_download_token(
                &app,
                &token.replacen(&id.to_string(), &other.to_string(), 1)
            )
            .is_err()
        );
        assert!(verify_download_token(&app, "garbage").is_err());
        assert!(verify_download_token(&app, "").is_err());

        let expired = format!("{id}.{}", Utc::now().timestamp() - 1);
        let signature = signing::sign(key(&app), DOWNLOAD_PURPOSE, &expired);
        assert!(verify_download_token(&app, &format!("{expired}.{signature}")).is_err());
        // A signature made for another purpose does not open a download.
        let session_sig = signing::sign(key(&app), "session", &id.to_string());
        assert!(verify_download_token(&app, &format!("{id}.9999999999.{session_sig}")).is_err());
    }

    #[tokio::test]
    async fn base_url_prefers_public_url_then_host() {
        let (_dir, app) = test_app().await;
        assert_eq!(base_url(&app, Some("127.0.0.1:9")), "http://127.0.0.1:9");
        assert_eq!(base_url(&app, None), format!("http://{}", app.config.bind));

        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().into_owned();
        let config = crate::config::Config::from_lookup(|name| match name {
            "PAPERJAM_SECRET_KEY" => Some("x".repeat(40)),
            "PAPERJAM_DATA_DIR" => Some(data_dir.clone()),
            "PAPERJAM_PUBLIC_URL" => Some("https://docs.example.org/".to_string()),
            _ => None,
        })
        .unwrap();
        let app = App::open(config).await.unwrap();
        assert_eq!(
            base_url(&app, Some("127.0.0.1:9")),
            "https://docs.example.org"
        );
    }

    #[tokio::test]
    async fn upload_link_is_claimed_once_and_can_be_released() {
        let (_dir, app) = test_app().await;
        let user = users::create_user(
            &app.ctx(Actor::cli()),
            crate::model::user::NewUser {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: secrecy::SecretString::from("correct horse battery".to_string()),
                locale: "en".into(),
                is_admin: true,
            },
        )
        .await
        .unwrap();
        let ctx = app.ctx(Actor::user(user.id, Via::Token));
        let (raw, _) = create_upload_link(&ctx).await.unwrap();

        assert_eq!(claim_upload_link(&app, &raw).await.unwrap(), user.id);
        assert!(
            claim_upload_link(&app, &raw).await.is_err(),
            "second claim must fail"
        );
        release_upload_link(&app, &raw).await.unwrap();
        assert_eq!(claim_upload_link(&app, &raw).await.unwrap(), user.id);
        assert!(claim_upload_link(&app, "not-a-link").await.is_err());
    }

    #[tokio::test]
    async fn expired_upload_link_is_refused() {
        let (_dir, app) = test_app().await;
        let user = users::create_user(
            &app.ctx(Actor::cli()),
            crate::model::user::NewUser {
                username: "bob".into(),
                display_name: "Bob".into(),
                password: secrecy::SecretString::from("correct horse battery".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO upload_links (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
            .bind(sha256_hex(b"old"))
            .bind(user.id)
            .bind(time::ts(Utc::now() - Duration::minutes(1)))
            .execute(&app.db.write)
            .await
            .unwrap();
        assert!(claim_upload_link(&app, "old").await.is_err());
    }
}
