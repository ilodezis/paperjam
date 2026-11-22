//! Accounts, passwords and web sessions.

use std::sync::OnceLock;

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::ids::{PersonId, UserId};
use crate::model::user::{NewUser, SessionToken, User};
use crate::service::{App, Ctx, events};
use crate::{random, signing, time};

pub const SESSION_LIFETIME_DAYS: i64 = 30;
const SESSION_PURPOSE: &str = "session";
const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_USERNAME_CHARS: usize = 64;
/// Password checks and hashes running at once. Each takes a thread and about
/// 19 MiB for Argon2, so a flood of sign-ins queues here instead of eating
/// the host's memory and cores.
const ARGON2_PERMITS: usize = 3;
static ARGON2_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(ARGON2_PERMITS);
/// How long a password check waits for a slot before it gives up. A flood
/// from many addresses then turns into quick refusals instead of a queue
/// that grows without end and holds every sign-in behind it.
const ARGON2_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const LOCALES: [&str; 2] = ["en", "ru"];

/// `SELECT <user columns><rest>` as a static string (sqlx wants static SQL).
macro_rules! select_user {
    ($rest:literal) => {
        concat!(
            "SELECT id, username, display_name, locale, person_id, is_admin, created_at, disabled_at",
            $rest
        )
    };
}

#[derive(sqlx::FromRow)]
struct Credentials {
    #[sqlx(flatten)]
    user: User,
    password_hash: String,
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

fn is_foreign_key_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_foreign_key_violation())
}

fn hash_password_blocking(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|_| ServiceError::internal("password_hash_failed", "could not hash the password"))
}

/// Runs an Argon2 job on a blocking thread, at most [`ARGON2_PERMITS`] at a time.
async fn argon2_job<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    argon2_job_within(ARGON2_WAIT, job).await
}

async fn argon2_job_within<T: Send + 'static>(
    wait: std::time::Duration,
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    let _slot = tokio::time::timeout(wait, ARGON2_SLOTS.acquire())
        .await
        .map_err(|_| {
            ServiceError::rate_limited(
                "too_many_attempts",
                "too many sign-ins at once, try again in a moment",
            )
        })?
        .map_err(|_| {
            ServiceError::internal("password_check_failed", "verification is shut down")
        })?;
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ServiceError::internal("password_check_failed", "verification task failed"))
}

fn password_matches_blocking(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Verified against when the username is unknown, so a miss costs the same
/// as a wrong password.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password_blocking("dummy password for timing").unwrap_or_default())
}

fn validate_username(username: &str) -> Result<()> {
    let ok = !username.is_empty()
        && username.chars().count() <= MAX_USERNAME_CHARS
        && username
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(ServiceError::invalid(
            "invalid_username",
            "usernames are 1-64 letters, digits, dots, dashes or underscores",
        ))
    }
}

fn session_key(app: &App) -> &[u8] {
    app.config.secret_key.expose_secret().as_bytes()
}

async fn validate_and_hash_password(password: &SecretString) -> Result<String> {
    let raw = password.expose_secret().to_string();
    if raw.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ServiceError::invalid(
            "weak_password",
            format!("passwords need at least {MIN_PASSWORD_CHARS} characters"),
        ));
    }
    if raw.len() > MAX_PASSWORD_BYTES {
        return Err(ServiceError::invalid(
            "password_too_long",
            "that password is too long",
        ));
    }
    argon2_job(move || hash_password_blocking(&raw)).await?
}

/// Creates an account. Only a web session or the CLI may: an administrator's
/// token must not be able to mint a password and sign in as a new administrator.
pub async fn create_user(ctx: &Ctx<'_>, new: NewUser) -> Result<User> {
    insert_user(ctx, new, false).await
}

/// Creates the first account of an instance, and only if there is none yet:
/// the check and the insert are one statement, so two requests racing for it
/// cannot both succeed.
pub async fn create_first_user(ctx: &Ctx<'_>, new: NewUser) -> Result<User> {
    insert_user(ctx, new, true).await
}

async fn insert_user(ctx: &Ctx<'_>, new: NewUser, only_if_empty: bool) -> Result<User> {
    ctx.actor.require_session_or_cli()?;
    let username = new.username.trim().to_string();
    validate_username(&username)?;
    if !LOCALES.contains(&new.locale.as_str()) {
        return Err(ServiceError::invalid(
            "invalid_locale",
            "supported locales are en and ru",
        ));
    }
    let display_name = match new.display_name.trim() {
        "" => username.clone(),
        name => name.to_string(),
    };
    let hash = validate_and_hash_password(&new.password).await?;

    let id = UserId::new();
    let created_at = time::now_ts();
    let mut tx = ctx.app.db.write.begin().await?;
    let sql = if only_if_empty {
        "INSERT INTO users (id, username, display_name, password_hash, locale, is_admin, created_at) \
         SELECT ?, ?, ?, ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users)"
    } else {
        "INSERT INTO users (id, username, display_name, password_hash, locale, is_admin, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)"
    };
    let inserted = sqlx::query(sql)
        .bind(id)
        .bind(&username)
        .bind(&display_name)
        .bind(&hash)
        .bind(&new.locale)
        .bind(new.is_admin)
        .bind(&created_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ServiceError::conflict("username_taken", "that username is already taken")
            } else {
                e.into()
            }
        })?;
    if inserted.rows_affected() == 0 {
        return Err(ServiceError::conflict(
            "already_set_up",
            "this instance already has a user",
        ));
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "user.create",
        None,
        json!({"user_id": id, "username": username, "is_admin": new.is_admin}),
    )
    .await?;
    tx.commit().await?;

    Ok(User {
        id,
        username,
        display_name,
        locale: new.locale,
        person_id: None,
        is_admin: new.is_admin,
        created_at,
        disabled_at: None,
    })
}

/// Refuses a sign-in whose username or password is longer than any account's
/// could be, before the database or the hash is touched.
pub fn check_login_lengths(username: &str, password: &str) -> Result<()> {
    // `take` keeps the count cheap however long the input is.
    let name_chars = username.chars().take(MAX_USERNAME_CHARS + 1).count();
    if name_chars > MAX_USERNAME_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(ServiceError::invalid(
            "credentials_too_long",
            "the username or password is too long",
        ));
    }
    Ok(())
}

/// Checks a username/password pair. Wrong password and unknown user are
/// indistinguishable to the caller (`invalid_credentials`).
pub async fn verify_password(ctx: &Ctx<'_>, username: &str, password: &str) -> Result<User> {
    check_login_lengths(username, password)?;
    let row: Option<Credentials> = sqlx::query_as(select_user!(
        ", password_hash FROM users WHERE username = ?"
    ))
    .bind(username.trim())
    .fetch_optional(&ctx.app.db.read)
    .await?;

    let stored = row
        .as_ref()
        .map_or_else(|| dummy_hash().to_string(), |r| r.password_hash.clone());
    let password = password.to_string();
    let matches = argon2_job(move || password_matches_blocking(&password, &stored)).await?;

    let invalid =
        || ServiceError::unauthorized("invalid_credentials", "wrong username or password");
    match row {
        Some(Credentials { user, .. }) if matches => {
            if user.disabled_at.is_some() {
                Err(ServiceError::unauthorized(
                    "account_disabled",
                    "this account is disabled",
                ))
            } else {
                Ok(user)
            }
        }
        _ => Err(invalid()),
    }
}

pub async fn get_user(ctx: &Ctx<'_>, id: UserId) -> Result<User> {
    sqlx::query_as(select_user!(" FROM users WHERE id = ?"))
        .bind(id)
        .fetch_optional(&ctx.app.db.read)
        .await?
        .ok_or_else(|| ServiceError::not_found("user_not_found", "no such user"))
}

pub async fn find_user_by_username(ctx: &Ctx<'_>, username: &str) -> Result<User> {
    sqlx::query_as(select_user!(" FROM users WHERE username = ?"))
        .bind(username.trim())
        .fetch_optional(&ctx.app.db.read)
        .await?
        .ok_or_else(|| ServiceError::not_found("user_not_found", "no such user"))
}

pub async fn list_users(ctx: &Ctx<'_>) -> Result<Vec<User>> {
    Ok(sqlx::query_as(select_user!(" FROM users ORDER BY id"))
        .fetch_all(&ctx.app.db.read)
        .await?)
}

#[derive(Debug, Default)]
pub struct UserPatch {
    pub disabled: Option<bool>,
    pub is_admin: Option<bool>,
    pub display_name: Option<String>,
    pub password: Option<SecretString>,
}

async fn fetch_user_tx(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, id: UserId) -> Result<User> {
    sqlx::query_as(select_user!(" FROM users WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ServiceError::not_found("user_not_found", "no such user"))
}

async fn assert_not_last_active_admin(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: &User,
) -> Result<()> {
    if !target.is_admin || target.disabled_at.is_some() {
        return Ok(());
    }
    let another: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM users WHERE is_admin = 1 AND disabled_at IS NULL AND id != ? LIMIT 1",
    )
    .bind(target.id)
    .fetch_optional(&mut **tx)
    .await?;
    if another.is_none() {
        return Err(ServiceError::conflict(
            "last_admin",
            "cannot disable or remove the last active administrator",
        ));
    }
    Ok(())
}

/// Updates an account's administrative fields or resets its password.
/// Web session or CLI only: an API token or OAuth client cannot modify accounts.
pub async fn update_user(ctx: &Ctx<'_>, id: UserId, patch: UserPatch) -> Result<User> {
    ctx.actor.require_session_or_cli()?;
    if patch.disabled == Some(true) && ctx.actor.user_id == Some(id) {
        return Err(ServiceError::invalid(
            "cannot_disable_self",
            "you cannot disable your own account",
        ));
    }
    if patch.is_admin == Some(false) && ctx.actor.user_id == Some(id) {
        return Err(ServiceError::invalid(
            "cannot_demote_self",
            "you cannot remove your own administrator access",
        ));
    }
    let display_name = match patch.display_name {
        Some(name) => {
            let trimmed = name.trim().to_string();
            if trimmed.is_empty() {
                return Err(ServiceError::invalid(
                    "invalid_display_name",
                    "display name cannot be empty",
                ));
            }
            Some(trimmed)
        }
        None => None,
    };
    let password_hash = match patch.password {
        Some(ref pw) => Some(validate_and_hash_password(pw).await?),
        None => None,
    };

    let mut tx = ctx.app.db.write.begin().await?;
    let current = fetch_user_tx(&mut tx, id).await?;
    if patch.disabled == Some(true) || patch.is_admin == Some(false) {
        assert_not_last_active_admin(&mut tx, &current).await?;
    }

    if let Some(disabled) = patch.disabled {
        if disabled {
            sqlx::query("UPDATE users SET disabled_at = COALESCE(disabled_at, ?) WHERE id = ?")
                .bind(time::now_ts())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("UPDATE users SET disabled_at = NULL WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    if let Some(is_admin) = patch.is_admin {
        sqlx::query("UPDATE users SET is_admin = ? WHERE id = ?")
            .bind(is_admin)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(ref name) = display_name {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(ref hash) = password_hash {
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    let updated = fetch_user_tx(&mut tx, id).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "user.update",
        None,
        json!({
            "user_id": id,
            "disabled": patch.disabled,
            "is_admin": patch.is_admin,
            "display_name": display_name,
            "password_reset": password_hash.is_some(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(updated)
}

/// Disables the account and ends all of its sessions. API tokens stop
/// resolving while the account is disabled.
pub async fn disable_user(ctx: &Ctx<'_>, id: UserId) -> Result<()> {
    update_user(
        ctx,
        id,
        UserPatch {
            disabled: Some(true),
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

/// Re-enables a previously disabled account.
pub async fn enable_user(ctx: &Ctx<'_>, id: UserId) -> Result<User> {
    update_user(
        ctx,
        id,
        UserPatch {
            disabled: Some(false),
            ..Default::default()
        },
    )
    .await
}

/// Permanently removes a user account and its credentials/sessions.
/// Documents and events remain in the archive (`added_by`/`updated_by` become NULL).
pub async fn delete_user(ctx: &Ctx<'_>, id: UserId) -> Result<()> {
    ctx.actor.require_session_or_cli()?;
    if ctx.actor.user_id == Some(id) {
        return Err(ServiceError::invalid(
            "cannot_delete_self",
            "you cannot delete your own account",
        ));
    }
    let mut tx = ctx.app.db.write.begin().await?;
    let target = fetch_user_tx(&mut tx, id).await?;
    assert_not_last_active_admin(&mut tx, &target).await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "user.delete",
        None,
        json!({"user_id": id, "username": target.username}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Changes the signed-in user's own password after verifying the current one.
/// Ends every other session of that user while keeping `keep_session` alive.
pub async fn change_password(
    ctx: &Ctx<'_>,
    user_id: UserId,
    current_password: &str,
    new_password: SecretString,
    keep_session: Option<&SessionToken>,
) -> Result<()> {
    ctx.actor.require_session_or_cli()?;
    if current_password.len() > MAX_PASSWORD_BYTES {
        return Err(ServiceError::invalid(
            "credentials_too_long",
            "the username or password is too long",
        ));
    }
    let row: Option<Credentials> =
        sqlx::query_as(select_user!(", password_hash FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_optional(&ctx.app.db.read)
            .await?;
    let creds = row.ok_or_else(|| ServiceError::not_found("user_not_found", "no such user"))?;

    let current = current_password.to_string();
    let stored = creds.password_hash;
    let matches = argon2_job(move || password_matches_blocking(&current, &stored)).await?;
    if !matches {
        return Err(ServiceError::forbidden(
            "wrong_password",
            "the current password is incorrect",
        ));
    }

    let new_hash = validate_and_hash_password(&new_password).await?;
    let keep_id = keep_session.and_then(|t| session_id(ctx.app, t).ok());

    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(ServiceError::not_found("user_not_found", "no such user"));
    }
    match keep_id {
        Some(ref sid) => {
            sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id != ?")
                .bind(user_id)
                .bind(sid)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "user.change_password",
        None,
        json!({"user_id": user_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn link_person(ctx: &Ctx<'_>, user: UserId, person: Option<PersonId>) -> Result<()> {
    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query("UPDATE users SET person_id = ? WHERE id = ?")
        .bind(person)
        .bind(user)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_foreign_key_violation(&e) {
                ServiceError::not_found("person_not_found", "no such person")
            } else {
                e.into()
            }
        })?;
    if updated.rows_affected() == 0 {
        return Err(ServiceError::not_found("user_not_found", "no such user"));
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "user.link_person",
        None,
        json!({"user_id": user, "person_id": person}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Sets the account's language: the interface default and the language the
/// model writes titles and summaries in for documents this user adds.
pub async fn set_locale(ctx: &Ctx<'_>, user: UserId, locale: &str) -> Result<User> {
    if !LOCALES.contains(&locale) {
        return Err(ServiceError::invalid(
            "invalid_locale",
            "supported locales are en and ru",
        ));
    }
    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query("UPDATE users SET locale = ? WHERE id = ?")
        .bind(locale)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(ServiceError::not_found("user_not_found", "no such user"));
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "user.set_locale",
        None,
        json!({"user_id": user, "locale": locale}),
    )
    .await?;
    tx.commit().await?;
    get_user(ctx, user).await
}

/// Deletes sessions past their expiry and returns how many went. They are already refused when
/// resolved; this only keeps the table from growing. Runs on the same occasions as the OAuth
/// `purge_stale`: when someone signs in, and once at start-up.
pub async fn purge_expired_sessions(app: &App) -> Result<u64> {
    purge_expired_sessions_at(app, Utc::now()).await
}

pub(crate) async fn purge_expired_sessions_at(app: &App, now: DateTime<Utc>) -> Result<u64> {
    // `<=` mirrors `resolve_session_at`, which accepts only `expires_at > now`.
    let purged = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(time::ts(now))
        .execute(&app.db.write)
        .await?;
    Ok(purged.rows_affected())
}

pub async fn create_session(ctx: &Ctx<'_>, user: UserId) -> Result<SessionToken> {
    // Housekeeping must not stand between a person and signing in.
    if let Err(e) = purge_expired_sessions(ctx.app).await {
        tracing::warn!(error = %e, "could not purge expired sessions");
    }
    let id = random::hex(32);
    let now = Utc::now();
    sqlx::query("INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(user)
        .bind(time::ts(now))
        .bind(time::ts(now + Duration::days(SESSION_LIFETIME_DAYS)))
        .execute(&ctx.app.db.write)
        .await?;
    let signature = signing::sign(session_key(ctx.app), SESSION_PURPOSE, &id);
    Ok(SessionToken(format!("{id}.{signature}")))
}

fn invalid_session() -> ServiceError {
    ServiceError::unauthorized("invalid_session", "the session is missing or expired")
}

/// Splits a cookie value into its session id after checking the signature.
fn session_id(app: &App, token: &SessionToken) -> Result<String> {
    let (id, signature) = token.0.split_once('.').ok_or_else(invalid_session)?;
    if signing::verify(session_key(app), SESSION_PURPOSE, id, signature) {
        Ok(id.to_string())
    } else {
        Err(invalid_session())
    }
}

pub async fn resolve_session(app: &App, token: &SessionToken) -> Result<User> {
    resolve_session_at(app, token, Utc::now()).await
}

pub(crate) async fn resolve_session_at(
    app: &App,
    token: &SessionToken,
    now: DateTime<Utc>,
) -> Result<User> {
    let id = session_id(app, token)?;
    sqlx::query_as(
        "SELECT u.id, u.username, u.display_name, u.locale, u.person_id, u.is_admin, \
                u.created_at, u.disabled_at \
         FROM sessions s JOIN users u ON u.id = s.user_id \
         WHERE s.id = ? AND s.expires_at > ? AND u.disabled_at IS NULL",
    )
    .bind(id)
    .bind(time::ts(now))
    .fetch_optional(&app.db.read)
    .await?
    .ok_or_else(invalid_session)
}

pub async fn delete_session(ctx: &Ctx<'_>, token: &SessionToken) -> Result<()> {
    let id = session_id(ctx.app, token)?;
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id)
        .execute(&ctx.app.db.write)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::service::testing::test_app;
    use crate::service::{Actor, Via};
    use secrecy::SecretString;

    fn new_user(name: &str, password: &str) -> NewUser {
        NewUser {
            username: name.to_string(),
            display_name: format!("{name} display"),
            password: SecretString::from(password.to_string()),
            locale: "en".to_string(),
            is_admin: false,
        }
    }

    #[test]
    fn login_lengths_are_capped_before_anything_else() {
        let name = "a".repeat(MAX_USERNAME_CHARS);
        let password = "p".repeat(MAX_PASSWORD_BYTES);
        assert!(check_login_lengths(&name, &password).is_ok());
        // Characters, not bytes, count for a name.
        assert!(check_login_lengths(&"é".repeat(MAX_USERNAME_CHARS), "x").is_ok());
        for (name, password) in [
            ("a".repeat(MAX_USERNAME_CHARS + 1), "x".to_string()),
            ("é".repeat(MAX_USERNAME_CHARS + 1), "x".to_string()),
            ("alice".to_string(), "p".repeat(MAX_PASSWORD_BYTES + 1)),
            ("a".repeat(5_000_000), "x".to_string()),
        ] {
            let err = check_login_lengths(&name, &password).unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Invalid, "credentials_too_long")
            );
        }
    }

    #[tokio::test]
    async fn oversized_credentials_never_reach_the_database_or_the_hash() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let err = verify_password(&ctx, &"a".repeat(1_000_000), "whatever")
            .await
            .unwrap_err();
        assert_eq!(err.code, "credentials_too_long");
        let err = verify_password(&ctx, "alice", &"p".repeat(1_000_000))
            .await
            .unwrap_err();
        assert_eq!(err.code, "credentials_too_long");
    }

    #[tokio::test]
    async fn password_checks_run_a_few_at_a_time() {
        // Hold every slot: a further check must wait instead of starting.
        let held = ARGON2_SLOTS
            .acquire_many(ARGON2_PERMITS as u32)
            .await
            .unwrap();
        let waiting =
            tokio::time::timeout(std::time::Duration::from_millis(200), argon2_job(|| true)).await;
        assert!(waiting.is_err(), "ran although every slot was taken");
        // Past the wait the check is refused instead of queueing forever.
        let refused = argon2_job_within(std::time::Duration::from_millis(50), || true)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "too_many_attempts");
        drop(held);
        assert!(argon2_job(|| true).await.unwrap());
    }

    #[tokio::test]
    async fn only_a_session_or_the_cli_creates_accounts() {
        let (_dir, app) = test_app().await;
        let admin = create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                is_admin: true,
                ..new_user("admin", "correct horse")
            },
        )
        .await
        .unwrap();
        for via in [Via::Token, Via::OAuth, Via::System, Via::Inbox, Via::Email] {
            let err = create_user(
                &app.ctx(Actor::user(admin.id, via)),
                new_user("mallory", "correct horse"),
            )
            .await
            .unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Forbidden, "session_required"),
                "{via:?}"
            );
        }
        let created = create_user(
            &app.ctx(Actor::user(admin.id, Via::Web)),
            new_user("bob", "correct horse"),
        )
        .await;
        assert!(created.is_ok(), "{created:?}");
        // Nothing was written for the refused ones.
        let names: Vec<(String,)> = sqlx::query_as("SELECT username FROM users ORDER BY username")
            .fetch_all(&app.db.read)
            .await
            .unwrap();
        let names: Vec<&str> = names.iter().map(|(n,)| n.as_str()).collect();
        assert_eq!(names, ["admin", "bob"]);
    }

    #[tokio::test]
    async fn password_roundtrip() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let created = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let verified = verify_password(&ctx, "alice", "correct horse")
            .await
            .unwrap();
        assert_eq!(created.id, verified.id);
        // Usernames are case-insensitive.
        assert_eq!(
            verify_password(&ctx, "ALICE", "correct horse")
                .await
                .unwrap()
                .id,
            created.id
        );
        // The stored value is a hash, not the password.
        let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(!hash.contains("correct horse"));
    }

    #[tokio::test]
    async fn wrong_password_is_unauthorized() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let err = verify_password(&ctx, "alice", "wrong password")
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unauthorized);
        assert_eq!(err.code, "invalid_credentials");
        // An unknown user looks exactly the same.
        let err = verify_password(&ctx, "nobody", "whatever1")
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_credentials");
    }

    #[tokio::test]
    async fn disabled_user_cannot_login() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        disable_user(&ctx, user.id).await.unwrap();
        let err = verify_password(&ctx, "alice", "correct horse")
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unauthorized);
        assert_eq!(err.code, "account_disabled");
    }

    #[tokio::test]
    async fn duplicate_username_conflicts() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let err = create_user(&ctx, new_user("Alice", "another pass"))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Conflict);
        assert_eq!(err.code, "username_taken");
    }

    #[tokio::test]
    async fn invalid_username_or_weak_password_is_rejected() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let err = create_user(&ctx, new_user("bad name!", "correct horse"))
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Invalid, "invalid_username")
        );
        let err = create_user(&ctx, new_user("", "correct horse"))
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_username");
        let err = create_user(&ctx, new_user("alice", "short"))
            .await
            .unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Invalid, "weak_password"));
    }

    #[tokio::test]
    async fn list_users_returns_everyone_without_hashes() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        create_user(&ctx, new_user("bob", "battery staple"))
            .await
            .unwrap();
        let users = list_users(&ctx).await.unwrap();
        let names: Vec<_> = users.iter().map(|u| u.username.as_str()).collect();
        assert_eq!(names, ["alice", "bob"]);
        assert!(!serde_json::to_string(&users).unwrap().contains("argon2"));
    }

    #[tokio::test]
    async fn session_roundtrip_and_logout() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let token = create_session(&ctx, user.id).await.unwrap();
        assert_eq!(resolve_session(&app, &token).await.unwrap().id, user.id);
        delete_session(&ctx, &token).await.unwrap();
        let err = resolve_session(&app, &token).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Unauthorized, "invalid_session")
        );
    }

    #[tokio::test]
    async fn tampered_or_foreign_session_token_is_rejected() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let token = create_session(&ctx, user.id).await.unwrap();
        let (id, sig) = token.0.split_once('.').unwrap();
        let forged = SessionToken(format!("{id}.{}", "0".repeat(sig.len())));
        assert!(resolve_session(&app, &forged).await.is_err());
        let no_sig = SessionToken(id.to_string());
        assert!(resolve_session(&app, &no_sig).await.is_err());
        assert!(
            resolve_session(&app, &SessionToken("garbage".into()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn expired_session_is_rejected() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let token = create_session(&ctx, user.id).await.unwrap();
        let in_29_days = chrono::Utc::now() + Duration::days(29);
        assert!(resolve_session_at(&app, &token, in_29_days).await.is_ok());
        let in_31_days = chrono::Utc::now() + Duration::days(31);
        assert!(resolve_session_at(&app, &token, in_31_days).await.is_err());
    }

    async fn session_ids(app: &App) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM sessions ORDER BY id")
            .fetch_all(&app.db.read)
            .await
            .unwrap()
    }

    /// Writes a session row that expired `ago` before now, straight into the table.
    async fn insert_expired_session(app: &App, user: UserId, id: &str, ago: Duration) {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO sessions (id, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(user)
        .bind(time::ts(now - Duration::days(SESSION_LIFETIME_DAYS) - ago))
        .bind(time::ts(now - ago))
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_purge_removes_expired_sessions_and_keeps_live_ones() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let live = create_session(&ctx, user.id).await.unwrap();
        insert_expired_session(&app, user.id, "expired-an-hour-ago", Duration::hours(1)).await;
        insert_expired_session(&app, user.id, "expired-a-month-ago", Duration::days(30)).await;
        assert_eq!(session_ids(&app).await.len(), 3);

        assert_eq!(purge_expired_sessions(&app).await.unwrap(), 2);

        let live_id = live.0.split_once('.').unwrap().0.to_string();
        assert_eq!(session_ids(&app).await, vec![live_id]);
        assert!(resolve_session(&app, &live).await.is_ok());
        // Nothing left to purge.
        assert_eq!(purge_expired_sessions(&app).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_purge_treats_the_expiry_instant_as_expired() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let token = create_session(&ctx, user.id).await.unwrap();
        let expiry = Utc::now() + Duration::days(SESSION_LIFETIME_DAYS);
        // A moment before the expiry the session lives on; past it, it is gone.
        let before = purge_expired_sessions_at(&app, expiry - Duration::minutes(1)).await;
        assert_eq!(before.unwrap(), 0);
        assert_eq!(session_ids(&app).await.len(), 1);
        let after = purge_expired_sessions_at(&app, expiry + Duration::minutes(1)).await;
        assert_eq!(after.unwrap(), 1);
        assert!(session_ids(&app).await.is_empty());
        assert!(resolve_session(&app, &token).await.is_err());
    }

    #[tokio::test]
    async fn signing_in_purges_expired_sessions() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        insert_expired_session(&app, user.id, "expired-yesterday", Duration::days(1)).await;
        let fresh = create_session(&ctx, user.id).await.unwrap();
        let fresh_id = fresh.0.split_once('.').unwrap().0.to_string();
        assert_eq!(session_ids(&app).await, vec![fresh_id]);
    }

    #[tokio::test]
    async fn disabling_a_user_ends_their_sessions() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        let token = create_session(&ctx, user.id).await.unwrap();
        disable_user(&ctx, user.id).await.unwrap();
        assert!(resolve_session(&app, &token).await.is_err());
    }

    #[tokio::test]
    async fn enabling_a_disabled_user_restores_login() {
        let (_dir, app) = test_app().await;
        let ctx = app.ctx(Actor::cli());
        let user = create_user(&ctx, new_user("alice", "correct horse"))
            .await
            .unwrap();
        disable_user(&ctx, user.id).await.unwrap();
        assert!(
            verify_password(&ctx, "alice", "correct horse")
                .await
                .is_err()
        );

        let reenabled = enable_user(&ctx, user.id).await.unwrap();
        assert!(reenabled.disabled_at.is_none());
        assert_eq!(
            verify_password(&ctx, "alice", "correct horse")
                .await
                .unwrap()
                .id,
            user.id
        );
    }

    #[tokio::test]
    async fn admin_guards_protect_self_and_last_admin() {
        let (_dir, app) = test_app().await;
        let cli = app.ctx(Actor::cli());
        let admin = create_user(
            &cli,
            NewUser {
                is_admin: true,
                ..new_user("admin", "correct horse")
            },
        )
        .await
        .unwrap();
        let bob = create_user(&cli, new_user("bob", "battery staple"))
            .await
            .unwrap();
        let web = app.ctx(Actor::user(admin.id, Via::Web));

        assert_eq!(
            update_user(
                &web,
                admin.id,
                UserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .code,
            "cannot_disable_self"
        );
        assert_eq!(
            update_user(
                &web,
                admin.id,
                UserPatch {
                    is_admin: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .code,
            "cannot_demote_self"
        );
        assert_eq!(
            delete_user(&web, admin.id).await.unwrap_err().code,
            "cannot_delete_self"
        );
        // Even via CLI, the last active admin cannot be removed or disabled.
        assert_eq!(
            delete_user(&cli, admin.id).await.unwrap_err().code,
            "last_admin"
        );
        assert_eq!(
            disable_user(&cli, admin.id).await.unwrap_err().code,
            "last_admin"
        );

        // Promoting bob makes it safe to disable or delete admin from bob's session.
        update_user(
            &web,
            bob.id,
            UserPatch {
                is_admin: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let bob_web = app.ctx(Actor::user(bob.id, Via::Web));
        delete_user(&bob_web, admin.id).await.unwrap();
        assert_eq!(
            get_user(&cli, admin.id).await.unwrap_err().code,
            "user_not_found"
        );
    }

    #[tokio::test]
    async fn deleting_a_user_clears_credentials_and_keeps_their_documents() {
        let (_dir, app) = test_app().await;
        let cli = app.ctx(Actor::cli());
        let admin = create_user(
            &cli,
            NewUser {
                is_admin: true,
                ..new_user("admin", "correct horse")
            },
        )
        .await
        .unwrap();
        let member = create_user(&cli, new_user("member", "battery staple"))
            .await
            .unwrap();
        let session = create_session(&cli, member.id).await.unwrap();
        assert_eq!(session_ids(&app).await.len(), 1);

        // Tokens/OAuth cannot delete or update users.
        for via in [Via::Token, Via::OAuth] {
            let err = delete_user(&app.ctx(Actor::user(admin.id, via)), member.id)
                .await
                .unwrap_err();
            assert_eq!(err.code, "session_required");
        }

        delete_user(&app.ctx(Actor::user(admin.id, Via::Web)), member.id)
            .await
            .unwrap();
        assert!(resolve_session(&app, &session).await.is_err());
        assert!(session_ids(&app).await.is_empty());
        assert_eq!(
            find_user_by_username(&cli, "member")
                .await
                .unwrap_err()
                .code,
            "user_not_found"
        );
    }

    #[tokio::test]
    async fn change_password_verifies_current_and_keeps_only_active_session() {
        let (_dir, app) = test_app().await;
        let cli = app.ctx(Actor::cli());
        let alice = create_user(&cli, new_user("alice", "old password 1"))
            .await
            .unwrap();
        let current = create_session(&cli, alice.id).await.unwrap();
        let other = create_session(&cli, alice.id).await.unwrap();
        let web = app.ctx(Actor::user(alice.id, Via::Web));

        let err = change_password(
            &web,
            alice.id,
            "wrong password",
            SecretString::from("new password 2".to_string()),
            Some(&current),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Forbidden, "wrong_password")
        );

        let err = change_password(
            &web,
            alice.id,
            "old password 1",
            SecretString::from("short".to_string()),
            Some(&current),
        )
        .await
        .unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Invalid, "weak_password"));

        // Tokens cannot change passwords.
        let err = change_password(
            &app.ctx(Actor::user(alice.id, Via::Token)),
            alice.id,
            "old password 1",
            SecretString::from("new password 2".to_string()),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "session_required");

        change_password(
            &web,
            alice.id,
            "old password 1",
            SecretString::from("new password 2".to_string()),
            Some(&current),
        )
        .await
        .unwrap();

        assert!(resolve_session(&app, &current).await.is_ok());
        assert!(resolve_session(&app, &other).await.is_err());
        assert!(
            verify_password(&cli, "alice", "old password 1")
                .await
                .is_err()
        );
        assert_eq!(
            verify_password(&cli, "alice", "new password 2")
                .await
                .unwrap()
                .id,
            alice.id
        );
    }
}
