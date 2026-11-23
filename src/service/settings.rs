//! Runtime-mutable instance settings backed by SQLite, with environment-variable
//! defaults and hot reloading via `App::runtime_settings`.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{Config, ConfigError, MAX_EMAIL_TAG_CHARS, SenderRule, parse_sender_rule};
use crate::db::Db;
use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::ids::UserId;
use crate::service::{Ctx, Via, events};
use crate::signing::constant_time_eq;
use crate::{random, time};

const EMAIL_SECRET_PREFIX: &str = "pje_";
const EMAIL_SECRET_RANDOM_CHARS: usize = 40;
const EMAIL_SECRET_VISIBLE_PREFIX_CHARS: usize = 12;

/// How an incoming webhook secret is verified at runtime.
#[derive(Clone)]
pub enum EmailSecretVerifier {
    DbHash(String),
    EnvPlain(SecretString),
}

impl fmt::Debug for EmailSecretVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DbHash(_) => f.write_str("DbHash([REDACTED])"),
            Self::EnvPlain(_) => f.write_str("EnvPlain([REDACTED])"),
        }
    }
}

impl EmailSecretVerifier {
    /// Constant-time check of a presented Bearer secret.
    pub fn matches(&self, token: &str) -> bool {
        match self {
            Self::DbHash(hash) => {
                constant_time_eq(sha256_hex(token.as_bytes()).as_bytes(), hash.as_bytes())
            }
            Self::EnvPlain(sec) => {
                constant_time_eq(token.as_bytes(), sec.expose_secret().as_bytes())
            }
        }
    }
}

/// Which account receives emailed documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailUserTarget {
    UserId(UserId),
    Username(String),
}

/// Active inbound email configuration after merging DB overrides with env defaults.
#[derive(Debug, Clone)]
pub struct EmailRuntimeConfig {
    pub enabled: bool,
    pub source: &'static str,
    pub secret: Option<EmailSecretVerifier>,
    pub secret_source: &'static str,
    pub secret_prefix: Option<String>,
    pub user: Option<EmailUserTarget>,
    pub allowed_senders: Vec<SenderRule>,
    pub allowed_senders_raw: Vec<String>,
    pub body_fallback: bool,
    pub local_only: bool,
    pub tag: Option<String>,
    pub last_received_at: Option<String>,
    pub received_count: i64,
}

/// Snapshot of all runtime-configurable settings kept in memory on [`crate::service::App`].
#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub email: EmailRuntimeConfig,
}

#[derive(sqlx::FromRow)]
struct EmailIntakeRow {
    enabled: i64,
    secret_hash: Option<String>,
    secret_prefix: Option<String>,
    user_id: Option<UserId>,
    allowed_senders: String,
    body_fallback: i64,
    local_only: i64,
    tag: Option<String>,
    last_received_at: Option<String>,
    received_count: i64,
}

fn parse_stored_senders(raw_json: &str) -> (Vec<SenderRule>, Vec<String>) {
    let entries: Vec<String> = serde_json::from_str(raw_json).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "ignoring malformed email_intake.allowed_senders JSON");
        Vec::new()
    });
    let mut rules = Vec::with_capacity(entries.len());
    let mut raw = Vec::with_capacity(entries.len());
    for entry in entries {
        match parse_sender_rule(&entry) {
            Ok(rule) => {
                raw.push(rule.as_str().to_string());
                rules.push(rule);
            }
            Err(e) => {
                tracing::warn!(entry = %entry, error = %e, "skipping invalid stored sender rule");
            }
        }
    }
    (rules, raw)
}

/// Loads [`RuntimeSettings`] from SQLite, falling back to [`Config`] when no
/// database override row exists yet.
pub async fn load(config: &Config, db: &Db) -> Result<RuntimeSettings> {
    let row: Option<EmailIntakeRow> = sqlx::query_as(
        "SELECT enabled, secret_hash, secret_prefix, user_id, allowed_senders, \
                body_fallback, local_only, tag, last_received_at, received_count \
         FROM email_intake WHERE id = 1",
    )
    .fetch_optional(&db.read)
    .await?;

    let email = match row {
        None => match &config.email_intake {
            Some(env) => EmailRuntimeConfig {
                enabled: true,
                source: "env",
                secret: Some(EmailSecretVerifier::EnvPlain(env.secret.clone())),
                secret_source: "env",
                secret_prefix: None,
                user: env.user.clone().map(EmailUserTarget::Username),
                allowed_senders: env.allowed_senders.clone(),
                allowed_senders_raw: env
                    .allowed_senders
                    .iter()
                    .map(|r| r.as_str().to_string())
                    .collect(),
                body_fallback: env.body_fallback,
                local_only: env.local_only,
                tag: env.tag.clone(),
                last_received_at: None,
                received_count: 0,
            },
            None => EmailRuntimeConfig {
                enabled: false,
                source: "none",
                secret: None,
                secret_source: "none",
                secret_prefix: None,
                user: None,
                allowed_senders: Vec::new(),
                allowed_senders_raw: Vec::new(),
                body_fallback: true,
                local_only: false,
                tag: None,
                last_received_at: None,
                received_count: 0,
            },
        },
        Some(row) => {
            let enabled = row.enabled != 0;
            let (secret, secret_source, secret_prefix) = match row.secret_hash {
                Some(hash) => (
                    Some(EmailSecretVerifier::DbHash(hash)),
                    "database",
                    row.secret_prefix,
                ),
                None if enabled && config.email_intake.is_some() => (
                    config
                        .email_intake
                        .as_ref()
                        .map(|env| EmailSecretVerifier::EnvPlain(env.secret.clone())),
                    "env",
                    None,
                ),
                None => (None, "none", None),
            };
            let (allowed_senders, allowed_senders_raw) = parse_stored_senders(&row.allowed_senders);
            EmailRuntimeConfig {
                enabled,
                source: "database",
                secret,
                secret_source,
                secret_prefix,
                user: row.user_id.map(EmailUserTarget::UserId),
                allowed_senders,
                allowed_senders_raw,
                body_fallback: row.body_fallback != 0,
                local_only: row.local_only != 0,
                tag: row.tag,
                last_received_at: row.last_received_at,
                received_count: row.received_count,
            }
        }
    };

    Ok(RuntimeSettings { email })
}

/// Public view of inbound email settings returned by the admin REST API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EmailSettingsView {
    pub enabled: bool,
    pub source: String,
    pub has_secret: bool,
    pub secret_source: String,
    pub secret_prefix: Option<String>,
    pub endpoint_url: String,
    #[schema(value_type = Option<String>)]
    pub user_id: Option<UserId>,
    pub allowed_senders: Vec<String>,
    pub body_fallback: bool,
    pub local_only: bool,
    pub tag: Option<String>,
    pub last_received_at: Option<String>,
    pub received_count: i64,
}

/// Returned once when a new email intake secret is generated or rotated.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CreatedEmailSecret {
    pub settings: EmailSettingsView,
    pub secret: String,
}

/// Distinguishes an omitted JSON field from an explicit `null` or value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PatchOpt<T> {
    #[default]
    Unset,
    Null,
    Value(T),
}

fn de_patch_user_id<'de, D>(deserializer: D) -> std::result::Result<PatchOpt<UserId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(PatchOpt::Null),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(PatchOpt::Null)
            } else {
                trimmed
                    .parse::<UserId>()
                    .map(PatchOpt::Value)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

fn de_patch_tag<'de, D>(deserializer: D) -> std::result::Result<PatchOpt<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(PatchOpt::Null),
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                Ok(PatchOpt::Null)
            } else {
                Ok(PatchOpt::Value(trimmed))
            }
        }
    }
}

/// Partial update for inbound email settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct EmailSettingsPatch {
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "de_patch_user_id")]
    #[schema(value_type = Option<String>)]
    pub user_id: PatchOpt<UserId>,
    pub allowed_senders: Option<Vec<String>>,
    pub body_fallback: Option<bool>,
    pub local_only: Option<bool>,
    #[serde(default, deserialize_with = "de_patch_tag")]
    #[schema(value_type = Option<String>)]
    pub tag: PatchOpt<String>,
}

async fn require_admin(ctx: &Ctx<'_>) -> Result<()> {
    if ctx.actor.via == Via::Cli && ctx.actor.user_id.is_none() {
        return Ok(());
    }
    let user_id = ctx.actor.require_user()?;
    let row: Option<(bool, Option<String>)> =
        sqlx::query_as("SELECT is_admin, disabled_at FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&ctx.app.db.read)
            .await?;
    match row {
        Some((true, None)) => Ok(()),
        _ => Err(ServiceError::forbidden(
            "admin_required",
            "this action is for administrators",
        )),
    }
}

async fn require_admin_session(ctx: &Ctx<'_>) -> Result<()> {
    require_admin(ctx).await?;
    ctx.actor.require_session_or_cli()?;
    Ok(())
}

struct EmailIntakeState {
    had_row: bool,
    enabled: bool,
    secret_hash: Option<String>,
    secret_prefix: Option<String>,
    user_id: Option<UserId>,
    allowed_senders: Vec<String>,
    body_fallback: bool,
    local_only: bool,
    tag: Option<String>,
}

async fn load_or_seed_email_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    config: &Config,
) -> Result<EmailIntakeState> {
    let row: Option<EmailIntakeRow> = sqlx::query_as(
        "SELECT enabled, secret_hash, secret_prefix, user_id, allowed_senders, \
                body_fallback, local_only, tag, last_received_at, received_count \
         FROM email_intake WHERE id = 1",
    )
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(row) = row {
        let (_, allowed_senders) = parse_stored_senders(&row.allowed_senders);
        return Ok(EmailIntakeState {
            had_row: true,
            enabled: row.enabled != 0,
            secret_hash: row.secret_hash,
            secret_prefix: row.secret_prefix,
            user_id: row.user_id,
            allowed_senders,
            body_fallback: row.body_fallback != 0,
            local_only: row.local_only != 0,
            tag: row.tag,
        });
    }

    let Some(env) = &config.email_intake else {
        return Ok(EmailIntakeState {
            had_row: false,
            enabled: false,
            secret_hash: None,
            secret_prefix: None,
            user_id: None,
            allowed_senders: Vec::new(),
            body_fallback: true,
            local_only: false,
            tag: None,
        });
    };

    let user_id = match &env.user {
        Some(name) => {
            sqlx::query_scalar::<_, UserId>(
                "SELECT id FROM users WHERE username = ? AND disabled_at IS NULL",
            )
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?
        }
        None => None,
    };

    Ok(EmailIntakeState {
        had_row: false,
        enabled: true,
        secret_hash: None,
        secret_prefix: None,
        user_id,
        allowed_senders: env
            .allowed_senders
            .iter()
            .map(|r| r.as_str().to_string())
            .collect(),
        body_fallback: env.body_fallback,
        local_only: env.local_only,
        tag: env.tag.clone(),
    })
}

async fn save_email_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &EmailIntakeState,
    updated_by: Option<UserId>,
) -> Result<()> {
    let senders_json = serde_json::to_string(&state.allowed_senders)
        .map_err(|e| ServiceError::internal("json_encode_failed", e.to_string()))?;
    sqlx::query(
        "INSERT INTO email_intake (
             id, enabled, secret_hash, secret_prefix, user_id, allowed_senders,
             body_fallback, local_only, tag, updated_at, updated_by
         ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             enabled = excluded.enabled,
             secret_hash = excluded.secret_hash,
             secret_prefix = excluded.secret_prefix,
             user_id = excluded.user_id,
             allowed_senders = excluded.allowed_senders,
             body_fallback = excluded.body_fallback,
             local_only = excluded.local_only,
             tag = excluded.tag,
             updated_at = excluded.updated_at,
             updated_by = excluded.updated_by",
    )
    .bind(i64::from(state.enabled))
    .bind(&state.secret_hash)
    .bind(&state.secret_prefix)
    .bind(state.user_id)
    .bind(&senders_json)
    .bind(i64::from(state.body_fallback))
    .bind(i64::from(state.local_only))
    .bind(&state.tag)
    .bind(time::now_ts())
    .bind(updated_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn validate_tag(tag: PatchOpt<String>) -> Result<PatchOpt<String>> {
    let PatchOpt::Value(raw) = tag else {
        return Ok(tag);
    };
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Ok(PatchOpt::Null);
    }
    if trimmed.chars().count() > MAX_EMAIL_TAG_CHARS || trimmed.chars().any(char::is_control) {
        return Err(ServiceError::invalid(
            "invalid_tag",
            format!("must be at most {MAX_EMAIL_TAG_CHARS} printable characters"),
        ));
    }
    Ok(PatchOpt::Value(trimmed))
}

fn validate_senders(senders: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    senders
        .map(|list| {
            list.into_iter()
                .map(|entry| {
                    parse_sender_rule(&entry)
                        .map(|rule| rule.as_str().to_string())
                        .map_err(|e| match e {
                            ConfigError::Invalid { reason, .. } => {
                                ServiceError::invalid("invalid_sender_rule", reason)
                            }
                            other => {
                                ServiceError::invalid("invalid_sender_rule", other.to_string())
                            }
                        })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
}

/// Returns the current inbound email configuration for administrators.
pub async fn get_email_settings(ctx: &Ctx<'_>, endpoint_url: &str) -> Result<EmailSettingsView> {
    require_admin(ctx).await?;
    let email = ctx.app.runtime_settings().email;

    let user_id = match &email.user {
        Some(EmailUserTarget::UserId(id)) => {
            sqlx::query_scalar::<_, UserId>("SELECT id FROM users WHERE id = ?")
                .bind(*id)
                .fetch_optional(&ctx.app.db.read)
                .await?
        }
        Some(EmailUserTarget::Username(name)) => {
            sqlx::query_scalar::<_, UserId>(
                "SELECT id FROM users WHERE username = ? AND disabled_at IS NULL",
            )
            .bind(name)
            .fetch_optional(&ctx.app.db.read)
            .await?
        }
        None => None,
    };

    let last_received_at = match email.last_received_at {
        Some(ts) => Some(ts),
        None => {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT MAX(at) FROM events WHERE via = 'email'",
            )
            .fetch_one(&ctx.app.db.read)
            .await?
        }
    };

    Ok(EmailSettingsView {
        enabled: email.enabled,
        source: email.source.to_string(),
        has_secret: email.secret.is_some(),
        secret_source: email.secret_source.to_string(),
        secret_prefix: email.secret_prefix,
        endpoint_url: endpoint_url.to_string(),
        user_id,
        allowed_senders: email.allowed_senders_raw,
        body_fallback: email.body_fallback,
        local_only: email.local_only,
        tag: email.tag,
        last_received_at,
        received_count: email.received_count,
    })
}

/// Updates non-secret inbound email settings and hot-reloads [`RuntimeSettings`].
pub async fn update_email_settings(
    ctx: &Ctx<'_>,
    patch: EmailSettingsPatch,
    endpoint_url: &str,
) -> Result<EmailSettingsView> {
    require_admin_session(ctx).await?;

    let validated_senders = validate_senders(patch.allowed_senders)?;
    let validated_tag = validate_tag(patch.tag)?;

    if let PatchOpt::Value(uid) = patch.user_id {
        let user_row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT disabled_at FROM users WHERE id = ?")
                .bind(uid)
                .fetch_optional(&ctx.app.db.read)
                .await?;
        match user_row {
            None => return Err(ServiceError::not_found("user_not_found", "no such user")),
            Some((Some(_),)) => {
                return Err(ServiceError::invalid(
                    "invalid_email_user",
                    "cannot attribute emailed documents to a disabled user",
                ));
            }
            Some((None,)) => {}
        }
    }

    let mut tx = ctx.app.db.write.begin().await?;
    let mut state = load_or_seed_email_state(&mut tx, &ctx.app.config).await?;

    if let Some(enabled) = patch.enabled {
        state.enabled = enabled;
    }
    match patch.user_id {
        PatchOpt::Unset => {}
        PatchOpt::Null => state.user_id = None,
        PatchOpt::Value(uid) => state.user_id = Some(uid),
    }
    if let Some(senders) = validated_senders {
        state.allowed_senders = senders;
    }
    if let Some(body_fallback) = patch.body_fallback {
        state.body_fallback = body_fallback;
    }
    if let Some(local_only) = patch.local_only {
        state.local_only = local_only;
    }
    match validated_tag {
        PatchOpt::Unset => {}
        PatchOpt::Null => state.tag = None,
        PatchOpt::Value(t) => state.tag = Some(t),
    }

    if state.enabled && state.secret_hash.is_none() && ctx.app.config.email_intake.is_none() {
        return Err(ServiceError::invalid(
            "email_secret_required",
            "generate an email intake secret before enabling inbound email",
        ));
    }

    save_email_state(&mut tx, &state, ctx.actor.user_id).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "settings.email.update",
        None,
        json!({
            "enabled": state.enabled,
            "user_id": state.user_id,
            "allowed_senders": state.allowed_senders,
            "body_fallback": state.body_fallback,
            "local_only": state.local_only,
            "tag": state.tag,
        }),
    )
    .await?;
    tx.commit().await?;

    ctx.app.reload_settings().await?;
    get_email_settings(ctx, endpoint_url).await
}

/// Generates or rotates the database-backed email intake secret, returning the
/// plaintext secret once.
pub async fn rotate_email_secret(ctx: &Ctx<'_>, endpoint_url: &str) -> Result<CreatedEmailSecret> {
    require_admin_session(ctx).await?;

    let secret = format!(
        "{EMAIL_SECRET_PREFIX}{}",
        random::base62(EMAIL_SECRET_RANDOM_CHARS)
    );
    let secret_prefix = secret[..EMAIL_SECRET_VISIBLE_PREFIX_CHARS].to_string();
    let secret_hash = sha256_hex(secret.as_bytes());

    let mut tx = ctx.app.db.write.begin().await?;
    let mut state = load_or_seed_email_state(&mut tx, &ctx.app.config).await?;
    if !state.had_row || state.secret_hash.is_none() {
        state.enabled = true;
    }
    state.secret_hash = Some(secret_hash);
    state.secret_prefix = Some(secret_prefix.clone());

    save_email_state(&mut tx, &state, ctx.actor.user_id).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "settings.email.rotate_secret",
        None,
        json!({
            "prefix": secret_prefix,
            "enabled": state.enabled,
        }),
    )
    .await?;
    tx.commit().await?;

    ctx.app.reload_settings().await?;
    let settings = get_email_settings(ctx, endpoint_url).await?;
    Ok(CreatedEmailSecret { settings, secret })
}

/// Revokes any stored email intake secret and disables inbound email.
pub async fn revoke_email_secret(ctx: &Ctx<'_>, endpoint_url: &str) -> Result<EmailSettingsView> {
    require_admin_session(ctx).await?;

    let mut tx = ctx.app.db.write.begin().await?;
    let mut state = load_or_seed_email_state(&mut tx, &ctx.app.config).await?;
    state.secret_hash = None;
    state.secret_prefix = None;
    state.enabled = false;

    save_email_state(&mut tx, &state, ctx.actor.user_id).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "settings.email.revoke_secret",
        None,
        json!({}),
    )
    .await?;
    tx.commit().await?;

    ctx.app.reload_settings().await?;
    get_email_settings(ctx, endpoint_url).await
}

/// Updates the `email_intake` receipt counter and timestamp when a database row exists.
pub async fn record_email_received(app: &crate::service::App) -> Result<()> {
    let now = time::now_ts();
    let res = sqlx::query(
        "UPDATE email_intake SET last_received_at = ?, received_count = received_count + 1 WHERE id = 1",
    )
    .bind(&now)
    .execute(&app.db.write)
    .await?;
    if res.rows_affected() > 0 {
        app.reload_settings().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_opt_distinguishes_omitted_null_empty_and_value() {
        let omitted: EmailSettingsPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(omitted.user_id, PatchOpt::Unset);
        assert_eq!(omitted.tag, PatchOpt::Unset);

        let nulls: EmailSettingsPatch =
            serde_json::from_str(r#"{"user_id": null, "tag": null}"#).unwrap();
        assert_eq!(nulls.user_id, PatchOpt::Null);
        assert_eq!(nulls.tag, PatchOpt::Null);

        let empties: EmailSettingsPatch =
            serde_json::from_str(r#"{"user_id": "", "tag": "   "}"#).unwrap();
        assert_eq!(empties.user_id, PatchOpt::Null);
        assert_eq!(empties.tag, PatchOpt::Null);

        let uid = UserId::new();
        let vals: EmailSettingsPatch =
            serde_json::from_str(&format!(r#"{{"user_id": "{uid}", "tag": " mail "}}"#)).unwrap();
        assert_eq!(vals.user_id, PatchOpt::Value(uid));
        assert_eq!(vals.tag, PatchOpt::Value("mail".to_string()));
    }

    #[test]
    fn secret_verifier_redacts_debug_output() {
        let a = EmailSecretVerifier::DbHash("super-sensitive-hash".into());
        let b = EmailSecretVerifier::EnvPlain(SecretString::from("super-sensitive-secret"));
        let dbg = format!("{a:?} {b:?}");
        assert!(!dbg.contains("super-sensitive"));
    }
}
