//! API tokens: `pj_` + 32 random base62 characters, shown once, stored hashed.

use chrono::{Duration, Utc};
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::ids::{TokenId, UserId};
use crate::model::user::TokenView;
use crate::service::{Actor, App, Ctx, Via, events};
use crate::{random, time};

const TOKEN_PREFIX: &str = "pj_";
const TOKEN_RANDOM_CHARS: usize = 32;
/// Characters of the raw value shown in lists so a token can be recognized.
const VISIBLE_PREFIX_CHARS: usize = TOKEN_PREFIX.len() + 4;
const MAX_NAME_CHARS: usize = 100;
/// `last_used_at` is refreshed at most this often per token.
const LAST_USED_GRANULARITY: Duration = Duration::minutes(1);

/// Creates a token for the acting user. Returns the view and the raw value,
/// which is never retrievable again.
///
/// Only a web session or the CLI may do this: a token that could issue tokens
/// would survive its own revocation.
pub async fn create_token(ctx: &Ctx<'_>, name: &str) -> Result<(TokenView, String)> {
    ctx.actor.require_session_or_cli()?;
    let user_id = ctx.actor.require_user()?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(ServiceError::invalid(
            "invalid_token_name",
            format!("token names are 1-{MAX_NAME_CHARS} characters"),
        ));
    }

    let raw = format!("{TOKEN_PREFIX}{}", random::base62(TOKEN_RANDOM_CHARS));
    let view = TokenView {
        id: TokenId::new(),
        user_id,
        name: name.to_string(),
        prefix: raw[..VISIBLE_PREFIX_CHARS].to_string(),
        created_at: time::now_ts(),
        last_used_at: None,
        revoked_at: None,
    };

    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, prefix, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(view.id)
    .bind(view.user_id)
    .bind(&view.name)
    .bind(sha256_hex(raw.as_bytes()))
    .bind(&view.prefix)
    .bind(&view.created_at)
    .execute(&mut *tx)
    .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "token.create",
        None,
        json!({"token_id": view.id, "name": view.name}),
    )
    .await?;
    tx.commit().await?;
    Ok((view, raw))
}

fn invalid_token() -> ServiceError {
    ServiceError::unauthorized("invalid_token", "the API token is invalid or revoked")
}

pub async fn resolve_token(app: &App, raw: &str) -> Result<Actor> {
    let row: Option<(TokenId, UserId, Option<String>)> = sqlx::query_as(
        "SELECT t.id, t.user_id, t.last_used_at \
         FROM api_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = ? AND t.revoked_at IS NULL AND u.disabled_at IS NULL",
    )
    .bind(sha256_hex(raw.as_bytes()))
    .fetch_optional(&app.db.read)
    .await?;
    let (id, user_id, last_used_at) = row.ok_or_else(invalid_token)?;

    let now = Utc::now();
    let stale = time::ts(now - LAST_USED_GRANULARITY);
    if last_used_at.is_none_or(|last| last < stale) {
        sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
            .bind(time::ts(now))
            .bind(id)
            .execute(&app.db.write)
            .await?;
    }
    Ok(Actor::user(user_id, Via::Token))
}

pub async fn revoke_token(ctx: &Ctx<'_>, id: TokenId) -> Result<()> {
    let user_id = ctx.actor.require_user()?;
    let mut tx = ctx.app.db.write.begin().await?;
    // Another user's token is reported as missing rather than forbidden.
    let updated = sqlx::query(
        "UPDATE api_tokens SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(time::now_ts())
    .bind(id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ServiceError::not_found("token_not_found", "no such token"));
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "token.revoke",
        None,
        json!({"token_id": id}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn list_tokens(ctx: &Ctx<'_>) -> Result<Vec<TokenView>> {
    let user_id = ctx.actor.require_user()?;
    Ok(sqlx::query_as(
        "SELECT id, user_id, name, prefix, created_at, last_used_at, revoked_at \
         FROM api_tokens WHERE user_id = ? AND revoked_at IS NULL ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(&ctx.app.db.read)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::user::{NewUser, User};
    use crate::service::Via;
    use crate::service::testing::test_app;
    use crate::service::users::{create_user, disable_user};
    use secrecy::SecretString;

    async fn user(app: &App, name: &str) -> User {
        create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                username: name.into(),
                display_name: name.into(),
                password: SecretString::from("correct horse".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap()
    }

    fn web(u: &User) -> Actor {
        Actor::user(u.id, Via::Web)
    }

    #[tokio::test]
    async fn a_token_cannot_issue_tokens() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        for via in [Via::Token, Via::OAuth, Via::System, Via::Inbox] {
            let err = create_token(&app.ctx(Actor::user(alice.id, via)), "spare")
                .await
                .unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Forbidden, "session_required"),
                "{via:?}"
            );
        }
        let none: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_tokens")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(none.0, 0);
        // A web session and the server's own CLI still can.
        assert!(create_token(&app.ctx(web(&alice)), "laptop").await.is_ok());
        assert!(
            create_token(&app.ctx(Actor::user(alice.id, Via::Cli)), "ci")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn token_shown_once_hash_stored() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let (view, raw) = create_token(&ctx, "laptop").await.unwrap();

        assert!(raw.starts_with("pj_"));
        assert_eq!(raw.len(), 3 + 32);
        assert!(raw[3..].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(view.name, "laptop");
        assert!(raw.starts_with(&view.prefix));
        assert!(view.prefix.len() < raw.len());

        // The raw value appears nowhere in the database.
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT token_hash, prefix FROM api_tokens")
                .fetch_all(&app.db.read)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].0, raw);
        assert_eq!(rows[0].0.len(), 64);
        // Nor is it in the list view.
        let listed = list_tokens(&ctx).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!serde_json::to_string(&listed).unwrap().contains(&raw));
    }

    #[tokio::test]
    async fn resolve_token_returns_the_owner_as_token_actor() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let (_, raw) = create_token(&ctx, "laptop").await.unwrap();
        let actor = resolve_token(&app, &raw).await.unwrap();
        assert_eq!(actor, Actor::user(alice.id, Via::Token));
        let err = resolve_token(&app, "pj_doesnotexistdoesnotexistdoesnotex")
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Unauthorized, "invalid_token")
        );
    }

    #[tokio::test]
    async fn revoked_token_rejected() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let (view, raw) = create_token(&ctx, "laptop").await.unwrap();
        revoke_token(&ctx, view.id).await.unwrap();
        let err = resolve_token(&app, &raw).await.unwrap_err();
        assert_eq!(err.code, "invalid_token");
        assert!(list_tokens(&ctx).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn token_of_disabled_user_is_rejected() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let (_, raw) = create_token(&ctx, "laptop").await.unwrap();
        disable_user(&app.ctx(Actor::cli()), alice.id)
            .await
            .unwrap();
        assert_eq!(
            resolve_token(&app, &raw).await.unwrap_err().code,
            "invalid_token"
        );
    }

    #[tokio::test]
    async fn users_cannot_revoke_each_others_tokens() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let bob = user(&app, "bob").await;
        let (view, raw) = create_token(&app.ctx(web(&alice)), "a").await.unwrap();
        let err = revoke_token(&app.ctx(web(&bob)), view.id)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        assert!(resolve_token(&app, &raw).await.is_ok());
        // Bob does not see the tokens of Alice either.
        assert!(list_tokens(&app.ctx(web(&bob))).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn last_used_is_updated_at_most_once_per_minute() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let (view, raw) = create_token(&ctx, "laptop").await.unwrap();
        assert!(list_tokens(&ctx).await.unwrap()[0].last_used_at.is_none());

        resolve_token(&app, &raw).await.unwrap();
        let first = list_tokens(&ctx).await.unwrap()[0]
            .last_used_at
            .clone()
            .unwrap();

        // Within the same minute the stamp does not move.
        resolve_token(&app, &raw).await.unwrap();
        let second = list_tokens(&ctx).await.unwrap()[0]
            .last_used_at
            .clone()
            .unwrap();
        assert_eq!(first, second);

        // Pretend the last use was two minutes ago: the next use refreshes it.
        let old = crate::time::ts(chrono::Utc::now() - chrono::Duration::minutes(2));
        sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
            .bind(&old)
            .bind(view.id)
            .execute(&app.db.write)
            .await
            .unwrap();
        resolve_token(&app, &raw).await.unwrap();
        let third = list_tokens(&ctx).await.unwrap()[0]
            .last_used_at
            .clone()
            .unwrap();
        assert!(third > old);
    }
}
