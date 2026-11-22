//! Inbound email ingestion: sender checks, user and tag resolution, and
//! handing extracted files to [`crate::service::upload::upload`].

pub mod parse;

use std::io::Cursor;

use serde::Serialize;

use crate::config::{EmailIntakeConfig, SenderRule};
use crate::error::{ErrorKind, Result, ServiceError};
use crate::ids::{DocumentId, TagId, UserId};
use crate::service::upload::{UploadInput, UploadOutcome, upload};
use crate::service::{Actor, App, Ctx, Via, taxonomy, users};

/// A document created or restored from an email attachment (or body).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct IngestedDocument {
    #[schema(value_type = String)]
    pub id: DocumentId,
    pub filename: String,
    pub status: &'static str,
    pub restored: bool,
}

/// An attachment whose exact bytes are already in the archive.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DuplicateAttachment {
    #[schema(value_type = String)]
    pub document_id: DocumentId,
    pub filename: String,
}

/// An attachment or inline asset that was intentionally skipped.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SkippedItem {
    pub filename: String,
    pub reason: &'static str,
}

/// Summary returned after processing one email.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EmailIngestReport {
    pub documents: Vec<IngestedDocument>,
    pub duplicates: Vec<DuplicateAttachment>,
    pub skipped: Vec<SkippedItem>,
    pub used_body_fallback: bool,
}

fn sender_not_allowed() -> ServiceError {
    ServiceError::forbidden(
        "sender_not_allowed",
        "sender is not allowed to submit documents by email",
    )
}

fn clean_envelope_addr(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim();
    (!inner.is_empty()).then_some(inner)
}

/// Verifies the sender against `allowed`. When `envelope_from` is supplied it
/// decides alone, and an empty or unreadable value is rejected rather than
/// falling back to `From:`; otherwise every address in `mime_from` must match a
/// rule.
pub fn check_sender(
    allowed: &[SenderRule],
    envelope_from: Option<&str>,
    mime_from: &[String],
) -> Result<()> {
    if allowed.is_empty() {
        return Ok(());
    }
    if let Some(raw) = envelope_from {
        let Some(addr) = clean_envelope_addr(raw) else {
            return Err(sender_not_allowed());
        };
        if allowed.iter().any(|rule| rule.matches(addr)) {
            return Ok(());
        }
        return Err(sender_not_allowed());
    }
    if mime_from.is_empty()
        || !mime_from
            .iter()
            .all(|addr| allowed.iter().any(|rule| rule.matches(addr)))
    {
        return Err(sender_not_allowed());
    }
    Ok(())
}

async fn resolve_user(app: &App, configured: Option<&str>) -> Result<UserId> {
    let ctx = app.ctx(Actor::system());
    match configured {
        Some(name) => {
            let user = users::find_user_by_username(&ctx, name)
                .await
                .map_err(|e| {
                    tracing::error!(code = e.code, "PAPERJAM_EMAIL_USER lookup failed");
                    ServiceError::unavailable(
                        "email_user_unavailable",
                        "configured PAPERJAM_EMAIL_USER could not be resolved",
                    )
                })?;
            if user.disabled_at.is_some() {
                tracing::error!("PAPERJAM_EMAIL_USER points to a disabled account");
                return Err(ServiceError::unavailable(
                    "email_user_unavailable",
                    "configured PAPERJAM_EMAIL_USER account is disabled",
                ));
            }
            Ok(user.id)
        }
        None => {
            let all = users::list_users(&ctx).await?;
            let mut active = all.into_iter().filter(|u| u.disabled_at.is_none());
            match (active.next(), active.next()) {
                (Some(only), None) => Ok(only.id),
                (None, _) => {
                    tracing::error!("email intake failed: no active user exists");
                    Err(ServiceError::unavailable(
                        "email_user_unconfigured",
                        "no active user exists to attribute emailed documents to",
                    ))
                }
                (Some(_), Some(_)) => {
                    tracing::error!(
                        "email intake is unconfigured for multiple users: set PAPERJAM_EMAIL_USER"
                    );
                    Err(ServiceError::unavailable(
                        "email_user_unconfigured",
                        "multiple users exist; set PAPERJAM_EMAIL_USER",
                    ))
                }
            }
        }
    }
}

async fn resolve_tag(ctx: &Ctx<'_>, name: &str) -> Result<TagId> {
    if let Some((id,)) = sqlx::query_as::<_, (TagId,)>("SELECT id FROM tags WHERE name = ?")
        .bind(name)
        .fetch_optional(&ctx.app.db.read)
        .await?
    {
        return Ok(id);
    }
    match taxonomy::create_tag(ctx, name, None).await {
        Ok(view) => Ok(view.id),
        Err(e) if (e.kind, e.code) == (ErrorKind::Conflict, "tag_exists") => {
            let (id,) = sqlx::query_as::<_, (TagId,)>("SELECT id FROM tags WHERE name = ?")
                .bind(name)
                .fetch_one(&ctx.app.db.read)
                .await?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// Parses one raw RFC 5322 email, checks the sender, and ingests every
/// eligible file through the normal upload pipeline.
pub async fn ingest(
    app: &App,
    config: &EmailIntakeConfig,
    raw: &[u8],
    envelope_from: Option<&str>,
) -> Result<EmailIngestReport> {
    if envelope_from.is_some() {
        check_sender(&config.allowed_senders, envelope_from, &[])?;
    }
    let parsed = parse::parse_email(raw, config.body_fallback)?;
    check_sender(
        &config.allowed_senders,
        envelope_from,
        &parsed.from_addresses,
    )?;

    let user_id = resolve_user(app, config.user.as_deref()).await?;
    let ctx = app.ctx(Actor::user(user_id, Via::Email));
    let tag_id = match (&config.tag, parsed.candidates.is_empty()) {
        (Some(name), false) => Some(resolve_tag(&ctx, name).await?),
        _ => None,
    };

    let mut documents = Vec::new();
    let mut duplicates = Vec::new();
    let mut skipped: Vec<SkippedItem> = parsed
        .skipped
        .into_iter()
        .map(|s| SkippedItem {
            filename: s.filename,
            reason: s.reason,
        })
        .collect();

    for candidate in parsed.candidates {
        let filename = candidate.filename;
        let input = UploadInput {
            original_name: filename.clone(),
            declared_mime: candidate.declared_mime,
            local_only: config.local_only,
            tags: tag_id.into_iter().collect(),
            ..UploadInput::default()
        };
        match upload(&ctx, input, Cursor::new(candidate.bytes)).await {
            Ok(UploadOutcome::Created(id)) => documents.push(IngestedDocument {
                id,
                filename,
                status: "queued",
                restored: false,
            }),
            Ok(UploadOutcome::Restored(id)) => documents.push(IngestedDocument {
                id,
                filename,
                status: "queued",
                restored: true,
            }),
            Err(e) if (e.kind, e.code) == (ErrorKind::Conflict, "duplicate") => {
                let doc_id = e
                    .details
                    .as_ref()
                    .and_then(|d| d.get("document_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<DocumentId>().ok());
                if let Some(document_id) = doc_id {
                    duplicates.push(DuplicateAttachment {
                        document_id,
                        filename,
                    });
                } else {
                    skipped.push(SkippedItem {
                        filename,
                        reason: e.code,
                    });
                }
            }
            Err(e) if e.kind == ErrorKind::Invalid => {
                skipped.push(SkippedItem {
                    filename,
                    reason: e.code,
                });
            }
            Err(e) => return Err(e),
        }
    }

    tracing::info!(
        ingested = documents.len(),
        duplicates = duplicates.len(),
        skipped = skipped.len(),
        body_fallback = parsed.used_body_fallback,
        "processed inbound email"
    );

    Ok(EmailIngestReport {
        documents,
        duplicates,
        skipped,
        used_body_fallback: parsed.used_body_fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_allowlist_prefers_envelope_and_checks_all_mime_senders() {
        let rules = vec![
            SenderRule::Exact("alice@example.org".into()),
            SenderRule::Domain("@trusted.example".into()),
        ];

        assert!(check_sender(&[], None, &[]).is_ok());
        assert!(check_sender(&rules, Some("<Alice@Example.org>"), &[]).is_ok());
        assert!(
            check_sender(
                &rules,
                Some("bob@trusted.example"),
                &["attacker@evil.example".into()]
            )
            .is_ok()
        );
        assert!(
            check_sender(
                &rules,
                Some("attacker@evil.example"),
                &["alice@example.org".into()]
            )
            .is_err()
        );
        assert!(check_sender(&rules, Some("<>"), &["alice@example.org".into()]).is_err());
        // A relay that sends the header empty does not fall back to `From:`.
        assert!(check_sender(&rules, Some(""), &["alice@example.org".into()]).is_err());
        assert!(check_sender(&rules, Some("  "), &["alice@example.org".into()]).is_err());

        // Without envelope, every From address must be allowed.
        assert!(check_sender(&rules, None, &["alice@example.org".into()]).is_ok());
        assert!(
            check_sender(
                &rules,
                None,
                &["alice@example.org".into(), "attacker@evil.example".into()]
            )
            .is_err()
        );
        assert!(check_sender(&rules, None, &[]).is_err());
    }
}
