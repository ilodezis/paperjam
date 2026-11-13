//! Shared documents: a page that shows one document to anyone holding its
//! link, for 15 minutes, an hour or a day, until it is revoked.
//!
//! The link is `/s/<id><signature>`: a random id and an HMAC of it. The row
//! keeps no secret, so a live link can be shown again, and a guessed or
//! altered link fails on the signature before the database is asked.

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::ids::{DocumentId, UserId};
use crate::model::document::{DocumentView, GetOptions};
use crate::model::fields::{FieldDef, Labels};
use crate::service::documents::DownloadFile;
use crate::service::{Actor, App, Ctx, documents, events};
use crate::{random, signing, time};

const PURPOSE: &str = "share-link";
const ID_CHARS: usize = 16;
/// Hex characters of the signature kept in the link (128 bits).
const SIGNATURE_CHARS: usize = 32;

/// How long a share link works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ShareTtl {
    #[default]
    #[serde(rename = "15m")]
    Minutes15,
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "24h")]
    Day,
}

impl ShareTtl {
    pub fn duration(self) -> Duration {
        match self {
            ShareTtl::Minutes15 => Duration::minutes(15),
            ShareTtl::Hour => Duration::hours(1),
            ShareTtl::Day => Duration::hours(24),
        }
    }
}

fn default_true() -> bool {
    true
}

/// What to share.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct NewShare {
    /// How long the link works; `15m` when omitted.
    #[serde(default)]
    pub ttl: ShareTtl,
    /// Show the summary and the extracted values next to the pages; the title,
    /// type and date are always shown.
    #[serde(default = "default_true")]
    pub show_details: bool,
}

/// A live share link.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Share {
    pub id: String,
    pub url: String,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// RFC 3339 UTC.
    pub expires_at: String,
    pub show_details: bool,
}

fn share_not_found() -> ServiceError {
    ServiceError::not_found("share_not_found", "no such share link")
}

/// Like an invalid download link: the link is the credential, so a bad one is
/// refused as unauthorized.
fn link_gone() -> ServiceError {
    ServiceError::unauthorized(
        "share_link_invalid",
        "this link is invalid, revoked or has expired",
    )
}

fn key(app: &App) -> &[u8] {
    use secrecy::ExposeSecret;
    app.config.secret_key.expose_secret().as_bytes()
}

/// The token of share `id`: the id and the start of its signature.
fn token(app: &App, id: &str) -> String {
    let signature = signing::sign(key(app), PURPOSE, id);
    format!("{id}{}", &signature[..SIGNATURE_CHARS])
}

/// The share id a token names, if its signature is genuine.
fn verify_token(app: &App, token: &str) -> Option<String> {
    if token.len() != ID_CHARS + SIGNATURE_CHARS || !token.is_ascii() {
        return None;
    }
    let (id, signature) = token.split_at(ID_CHARS);
    signing::verify_prefix(key(app), PURPOSE, id, signature).then(|| id.to_string())
}

fn url(app: &App, base_url: &str, id: &str) -> String {
    format!("{base_url}/s/{}", token(app, id))
}

/// Creates a share link for document `id`.
pub async fn create_share(
    ctx: &Ctx<'_>,
    base_url: &str,
    document: DocumentId,
    new: NewShare,
) -> Result<Share> {
    let user: Option<UserId> = ctx.actor.user_id;
    let mut tx = ctx.app.db.write.begin().await?;
    let trashed: Option<(Option<String>,)> =
        sqlx::query_as("SELECT trashed_at FROM documents WHERE id = ?")
            .bind(document)
            .fetch_optional(&mut *tx)
            .await?;
    match trashed {
        None => {
            return Err(ServiceError::not_found(
                "document_not_found",
                "no such document",
            ));
        }
        Some((Some(_),)) => {
            return Err(ServiceError::conflict(
                "document_trashed",
                "a document in the trash cannot be shared",
            ));
        }
        Some((None,)) => {}
    }
    let id = random::base62(ID_CHARS);
    let now = Utc::now();
    let created_at = time::ts(now);
    let expires_at = time::ts(now + new.ttl.duration());
    sqlx::query(
        "INSERT INTO shares (id, document_id, created_by, created_at, expires_at, show_details) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(document)
    .bind(user)
    .bind(&created_at)
    .bind(&expires_at)
    .bind(new.show_details)
    .execute(&mut *tx)
    .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "share.create",
        Some(document),
        json!({"share": id, "ttl": new.ttl, "show_details": new.show_details}),
    )
    .await?;
    tx.commit().await?;
    Ok(Share {
        url: url(ctx.app, base_url, &id),
        id,
        created_at,
        expires_at,
        show_details: new.show_details,
    })
}

/// The live (not expired, not revoked) share links of document `id`, newest first.
pub async fn list_shares(
    ctx: &Ctx<'_>,
    base_url: &str,
    document: DocumentId,
) -> Result<Vec<Share>> {
    let rows: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT id, created_at, expires_at, show_details FROM shares \
         WHERE document_id = ? AND revoked_at IS NULL AND expires_at > ? \
         ORDER BY created_at DESC, id",
    )
    .bind(document)
    .bind(time::now_ts())
    .fetch_all(&ctx.app.db.read)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, created_at, expires_at, show_details)| Share {
            url: url(ctx.app, base_url, &id),
            id,
            created_at,
            expires_at,
            show_details,
        })
        .collect())
}

/// Revokes a share link; revoking one that already stopped working is fine.
pub async fn revoke_share(ctx: &Ctx<'_>, id: &str) -> Result<()> {
    let mut tx = ctx.app.db.write.begin().await?;
    let row: Option<(DocumentId, Option<String>)> =
        sqlx::query_as("SELECT document_id, revoked_at FROM shares WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (document, revoked) = row.ok_or_else(share_not_found)?;
    if revoked.is_none() {
        sqlx::query("UPDATE shares SET revoked_at = ? WHERE id = ?")
            .bind(time::now_ts())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        events::record(
            &mut tx,
            &ctx.actor,
            "share.revoke",
            Some(document),
            json!({"share": id}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A document as its share link shows it.
pub struct Shared {
    pub document: DocumentView,
    pub show_details: bool,
    /// RFC 3339 UTC.
    pub expires_at: String,
    /// Display name of the person who shared it, while their account exists.
    pub shared_by: Option<String>,
    /// Names of the document's type by language.
    pub type_labels: Option<Labels>,
    /// The fields of the document's type, for their names.
    pub field_defs: Vec<FieldDef>,
    /// The pages that have a render, with its size in pixels when known.
    pub pages: Vec<SharedPage>,
    /// What the download button serves.
    pub download: DownloadFile,
}

pub struct SharedPage {
    pub n: u32,
    pub size: Option<(u32, u32)>,
}

/// The document behind a share token, if the link still works: genuine, not
/// revoked, not expired, and its document not in the trash.
pub async fn open(app: &App, token: &str) -> Result<Shared> {
    let (document, expires_at, show_details, shared_by) = resolve(app, token).await?;
    let ctx = app.ctx(Actor::system());
    let document = documents::get_document(&ctx, document, GetOptions::default()).await?;
    let type_row: Option<(String, String)> = match document.type_id {
        Some(type_id) => {
            sqlx::query_as("SELECT labels, fields FROM document_types WHERE id = ?")
                .bind(type_id)
                .fetch_optional(&app.db.read)
                .await?
        }
        None => None,
    };
    let (type_labels, field_defs) = match type_row {
        Some((labels, fields)) => (
            serde_json::from_str(&labels).ok(),
            serde_json::from_str(&fields).unwrap_or_default(),
        ),
        None => (None, Vec::new()),
    };
    let rows: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT n, width, height FROM pages \
         WHERE document_id = ? AND render_key IS NOT NULL ORDER BY n",
    )
    .bind(document.id)
    .fetch_all(&app.db.read)
    .await?;
    let pages = rows
        .into_iter()
        .map(|(n, w, h)| SharedPage {
            n: n.max(0) as u32,
            size: w.zip(h).map(|(w, h)| (w.max(0) as u32, h.max(0) as u32)),
        })
        .collect();
    let download = documents::download_file(&ctx, document.id).await?;
    Ok(Shared {
        document,
        show_details,
        expires_at,
        shared_by,
        type_labels,
        field_defs,
        pages,
        download,
    })
}

/// The document a share token opens, with the share's expiry, whether it
/// shows details, and who shared it.
async fn resolve(app: &App, token: &str) -> Result<(DocumentId, String, bool, Option<String>)> {
    let id = verify_token(app, token).ok_or_else(link_gone)?;
    let row: Option<(DocumentId, String, bool, Option<String>)> = sqlx::query_as(
        "SELECT s.document_id, s.expires_at, s.show_details, u.display_name \
         FROM shares s \
         JOIN documents d ON d.id = s.document_id \
         LEFT JOIN users u ON u.id = s.created_by \
         WHERE s.id = ? AND s.revoked_at IS NULL AND s.expires_at > ? AND d.trashed_at IS NULL",
    )
    .bind(&id)
    .bind(time::now_ts())
    .fetch_optional(&app.db.read)
    .await?;
    row.ok_or_else(link_gone)
}

/// The document a share token opens, for its page images and download; the
/// same checks as [`open`].
pub async fn document_of(app: &App, token: &str) -> Result<DocumentId> {
    Ok(resolve(app, token).await?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::testing::{insert_document, test_app};

    async fn insert_with_file(app: &App) -> DocumentId {
        let id = insert_document(app).await;
        sqlx::query(
            "INSERT INTO files (document_id, sha256, mime, size, original_name, storage_key) \
             VALUES (?, ?, 'application/pdf', 4, 'scan.pdf', 'k')",
        )
        .bind(id)
        .bind(id.to_string())
        .execute(&app.db.write)
        .await
        .unwrap();
        id
    }

    fn token_of(share: &Share) -> String {
        share.url.rsplit('/').next().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_share_link_opens_its_document_until_revoked() {
        let (_dir, app) = test_app().await;
        let doc = insert_with_file(&app).await;
        let ctx = app.ctx(Actor::cli());
        let share = create_share(
            &ctx,
            "https://docs.example.org",
            doc,
            NewShare {
                ttl: ShareTtl::Hour,
                show_details: false,
            },
        )
        .await
        .unwrap();
        assert!(share.url.starts_with("https://docs.example.org/s/"));
        let token = token_of(&share);
        assert_eq!(token.len(), ID_CHARS + SIGNATURE_CHARS);

        let shared = open(&app, &token).await.unwrap();
        assert_eq!(shared.document.id, doc);
        assert!(!shared.show_details);

        let listed = list_shares(&ctx, "https://docs.example.org", doc)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].url, share.url, "a live link can be shown again");

        revoke_share(&ctx, &share.id).await.unwrap();
        assert!(open(&app, &token).await.is_err());
        assert!(list_shares(&ctx, "x", doc).await.unwrap().is_empty());
        // Revoking twice is fine; an unknown id is not found.
        revoke_share(&ctx, &share.id).await.unwrap();
        assert!(revoke_share(&ctx, "nope").await.is_err());
    }

    #[tokio::test]
    async fn altered_expired_and_trashed_links_do_not_open() {
        let (_dir, app) = test_app().await;
        let doc = insert_with_file(&app).await;
        let ctx = app.ctx(Actor::cli());
        let share = create_share(
            &ctx,
            "x",
            doc,
            NewShare {
                ttl: ShareTtl::Minutes15,
                show_details: true,
            },
        )
        .await
        .unwrap();
        let token = token_of(&share);

        let mut altered = token.clone().into_bytes();
        let last = altered.len() - 1;
        altered[last] = if altered[last] == b'0' { b'1' } else { b'0' };
        assert!(
            open(&app, &String::from_utf8(altered).unwrap())
                .await
                .is_err()
        );
        assert!(open(&app, "").await.is_err());
        assert!(open(&app, &token[..ID_CHARS]).await.is_err());
        // A signature for another purpose does not open a share.
        let foreign = format!(
            "{}{}",
            share.id,
            &signing::sign(key(&app), "download-link", &share.id)[..SIGNATURE_CHARS]
        );
        assert!(open(&app, &foreign).await.is_err());

        sqlx::query("UPDATE documents SET trashed_at = ? WHERE id = ?")
            .bind(time::now_ts())
            .bind(doc)
            .execute(&app.db.write)
            .await
            .unwrap();
        assert!(
            open(&app, &token).await.is_err(),
            "a trashed document is not shown"
        );
        assert!(
            create_share(
                &ctx,
                "x",
                doc,
                NewShare {
                    ttl: ShareTtl::Day,
                    show_details: true
                }
            )
            .await
            .is_err()
        );

        sqlx::query("UPDATE documents SET trashed_at = NULL WHERE id = ?")
            .bind(doc)
            .execute(&app.db.write)
            .await
            .unwrap();
        assert!(open(&app, &token).await.is_ok());
        sqlx::query("UPDATE shares SET expires_at = ? WHERE id = ?")
            .bind(time::ts(Utc::now() - Duration::seconds(1)))
            .bind(&share.id)
            .execute(&app.db.write)
            .await
            .unwrap();
        assert!(open(&app, &token).await.is_err());
    }

    #[test]
    fn ttls_are_15_minutes_an_hour_or_a_day() {
        assert_eq!(serde_json::to_value(ShareTtl::default()).unwrap(), "15m");
        let ttl: ShareTtl = serde_json::from_value(json!("24h")).unwrap();
        assert_eq!(ttl.duration(), Duration::hours(24));
        assert!(serde_json::from_value::<ShareTtl>(json!("7d")).is_err());
        let new: NewShare = serde_json::from_value(json!({})).unwrap();
        assert_eq!(new.ttl, ShareTtl::Minutes15);
        assert!(new.show_details);
    }
}
