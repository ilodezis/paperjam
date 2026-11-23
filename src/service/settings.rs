//! Runtime-mutable instance settings backed by SQLite, with environment-variable
//! defaults and hot reloading via `App::runtime_settings`.

use std::collections::HashMap;
use std::fmt;

use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::config::{
    Config, ConfigError, DEFAULT_HANDWRITING_MODEL, LlmProvider, MAX_EMAIL_TAG_CHARS,
    MAX_INBOX_INTERVAL_SECS, ModelPrice, SenderRule, parse_llm_prices, parse_sender_rule,
};
use crate::crypto;
use crate::db::Db;
use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::ids::UserId;
use crate::inbox::{self, FailedInboxFile};
use crate::service::{Ctx, Via, events};
use crate::signing::constant_time_eq;
use crate::{random, time};

const EMAIL_SECRET_PREFIX: &str = "pje_";
const EMAIL_SECRET_RANDOM_CHARS: usize = 40;
const EMAIL_SECRET_VISIBLE_PREFIX_CHARS: usize = 12;
const MAX_LLM_MODEL_CHARS: usize = 200;
const MAX_CALENDAR_LEAD_DAYS: u32 = 3650;
const LLM_API_KEY_SETTING: &str = "llm.api_key";

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

/// Active LLM configuration after merging DB overrides with env defaults.
#[derive(Debug, Clone)]
pub struct LlmRuntimeConfig {
    pub enabled: bool,
    pub api_key: Option<SecretString>,
    pub api_key_source: &'static str,
    pub base_url: Url,
    pub provider: String,
    pub effective_provider: LlmProvider,
    pub model: String,
    pub vision_model: String,
    pub handwriting_enabled: bool,
    pub handwriting_model: String,
    pub allowed_models: Vec<String>,
    pub reasoning_effort: Option<String>,
    pub create_tags: bool,
    pub monthly_budget_usd: Option<Decimal>,
    pub prices: HashMap<String, ModelPrice>,
    pub has_overrides: bool,
}

impl LlmRuntimeConfig {
    /// Effective handwriting re-read model, or `None` when disabled, empty,
    /// or identical to the primary vision model.
    pub fn effective_handwriting_model(&self) -> Option<&str> {
        (self.handwriting_enabled
            && !self.handwriting_model.is_empty()
            && self.handwriting_model != self.vision_model)
            .then_some(self.handwriting_model.as_str())
    }

    /// Whether `model` is permitted for LLM reprocessing.
    pub fn is_model_allowed(&self, model: &str) -> bool {
        self.allowed_models.iter().any(|m| m == model)
    }
}

/// Active calendar feed configuration after merging DB overrides with env defaults.
#[derive(Debug, Clone)]
pub struct CalendarRuntimeConfig {
    pub lead_days: u32,
    pub has_overrides: bool,
}

/// Active watched inbox folder configuration after merging DB overrides with env defaults.
#[derive(Debug, Clone)]
pub struct InboxRuntimeConfig {
    pub configured: bool,
    pub paused: bool,
    pub user: Option<EmailUserTarget>,
    pub interval_secs: u64,
    pub has_overrides: bool,
}

/// Snapshot of all runtime-configurable settings kept in memory on [`crate::service::App`].
#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub email: EmailRuntimeConfig,
    pub llm: LlmRuntimeConfig,
    pub calendar: CalendarRuntimeConfig,
    pub inbox: InboxRuntimeConfig,
}

#[derive(sqlx::FromRow)]
struct AppSettingRow {
    key: String,
    value: String,
    encrypted: i64,
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

    let setting_rows: Vec<AppSettingRow> =
        sqlx::query_as("SELECT key, value, encrypted FROM app_settings")
            .fetch_all(&db.read)
            .await?;
    let rows: HashMap<String, (String, i64)> = setting_rows
        .into_iter()
        .map(|r| (r.key, (r.value, r.encrypted)))
        .collect();

    let llm = load_llm_settings(config, &rows);
    let calendar = load_calendar_settings(config, &rows);
    let inbox = load_inbox_settings(config, &rows);

    Ok(RuntimeSettings {
        email,
        llm,
        calendar,
        inbox,
    })
}

fn parse_bool_setting(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn validate_http_url(raw: &str) -> Result<Url> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|_| {
        ServiceError::invalid("invalid_llm_base_url", "base URL must be a valid URL")
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ServiceError::invalid(
            "invalid_llm_base_url",
            "base URL must use http or https with a host",
        ));
    }
    Ok(url)
}

fn validate_provider(raw: &str) -> Result<String> {
    let norm = raw.trim().to_ascii_lowercase();
    match norm.as_str() {
        "auto" | "openrouter" | "generic" => Ok(norm),
        _ => Err(ServiceError::invalid(
            "invalid_llm_provider",
            "provider must be auto, openrouter, or generic",
        )),
    }
}

fn validate_model_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_LLM_MODEL_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return Err(ServiceError::invalid(
            "invalid_llm_model",
            format!("model identifier must be 1 to {MAX_LLM_MODEL_CHARS} printable characters"),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_reasoning_effort(raw: &str) -> Result<String> {
    let norm = raw.trim().to_ascii_lowercase();
    match norm.as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" => Ok(norm),
        _ => Err(ServiceError::invalid(
            "invalid_reasoning_effort",
            "reasoning effort must be one of none, minimal, low, medium, high, or xhigh",
        )),
    }
}

fn validate_budget(raw: &str) -> Result<Decimal> {
    let trimmed = raw.trim();
    trimmed
        .parse::<Decimal>()
        .ok()
        .filter(|d| !d.is_sign_negative())
        .ok_or_else(|| {
            ServiceError::invalid(
                "invalid_llm_budget",
                "monthly budget must be a non-negative decimal amount",
            )
        })
}

pub(crate) fn format_llm_prices(prices: &HashMap<String, ModelPrice>) -> String {
    let mut entries: Vec<(&String, &ModelPrice)> = prices.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(model, p)| format!("{model}={}:{}", p.input_per_mtok, p.output_per_mtok))
        .collect::<Vec<_>>()
        .join(",")
}

fn load_llm_settings(config: &Config, rows: &HashMap<String, (String, i64)>) -> LlmRuntimeConfig {
    let has_overrides = rows.keys().any(|k| k.starts_with("llm."));
    let enabled = rows
        .get("llm.enabled")
        .and_then(|(v, _)| parse_bool_setting(v))
        .unwrap_or(true);

    let mut db_api_key: Option<SecretString> = None;
    if let Some((raw, encrypted)) = rows.get(LLM_API_KEY_SETTING) {
        if *encrypted != 0 {
            match crypto::decrypt_secret(
                config.secret_key.expose_secret().as_bytes(),
                LLM_API_KEY_SETTING,
                raw,
            ) {
                Ok(plain) if !plain.trim().is_empty() => {
                    db_api_key = Some(SecretString::from(plain.trim().to_string()));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "failed to decrypt stored llm.api_key; treating database key as unavailable"
                    );
                }
            }
        } else if !raw.trim().is_empty() {
            db_api_key = Some(SecretString::from(raw.trim().to_string()));
        }
    }

    let (api_key, api_key_source) = match db_api_key {
        Some(key) => (Some(key), "database"),
        None => match &config.llm_api_key {
            Some(key) => (Some(key.clone()), "env"),
            None => (None, "none"),
        },
    };

    let base_url = rows
        .get("llm.base_url")
        .and_then(|(raw, _)| match validate_http_url(raw) {
            Ok(url) => Some(url),
            Err(e) => {
                tracing::warn!(error = %e, "ignoring invalid stored llm.base_url");
                None
            }
        })
        .unwrap_or_else(|| config.llm_base_url.clone());

    let provider = rows
        .get("llm.provider")
        .and_then(|(raw, _)| match validate_provider(raw) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "ignoring invalid stored llm.provider");
                None
            }
        })
        .unwrap_or_else(|| config.llm_provider_raw.clone());

    let effective_provider = match provider.as_str() {
        "openrouter" => LlmProvider::OpenRouter,
        "generic" => LlmProvider::Generic,
        _ => LlmProvider::from_base_url(&base_url),
    };

    let model = rows
        .get("llm.model")
        .and_then(|(raw, _)| validate_model_name(raw).ok())
        .unwrap_or_else(|| config.llm_model.clone());

    let vision_model = rows
        .get("llm.vision_model")
        .and_then(|(raw, _)| validate_model_name(raw).ok())
        .unwrap_or_else(|| config.llm_vision_model.clone());

    let handwriting_enabled = rows
        .get("llm.handwriting_enabled")
        .and_then(|(raw, _)| parse_bool_setting(raw))
        .unwrap_or(config.llm_handwriting_model.is_some());

    let handwriting_model = rows
        .get("llm.handwriting_model")
        .and_then(|(raw, _)| validate_model_name(raw).ok())
        .unwrap_or_else(|| {
            config
                .llm_handwriting_model
                .clone()
                .unwrap_or_else(|| DEFAULT_HANDWRITING_MODEL.to_string())
        });

    let stored_allowed = rows.get("llm.allowed_models").and_then(|(raw, _)| {
        let parsed = serde_json::from_str::<Vec<String>>(raw)
            .map_err(|e| tracing::warn!(error = %e, "ignoring malformed llm.allowed_models JSON"))
            .ok()?;
        let cleaned: Vec<String> = parsed
            .into_iter()
            .filter_map(|item| validate_model_name(&item).ok())
            .collect();
        (!cleaned.is_empty()).then_some(cleaned)
    });

    let allowed_models = match stored_allowed {
        Some(list) => list,
        None if config.llm_allowed_models_explicit => config.llm_allowed_models.clone(),
        None => {
            let mut models = vec![model.clone()];
            if !models.contains(&vision_model) {
                models.push(vision_model.clone());
            }
            if handwriting_enabled
                && !handwriting_model.is_empty()
                && handwriting_model != vision_model
                && !models.contains(&handwriting_model)
            {
                models.push(handwriting_model.clone());
            }
            models
        }
    };

    let reasoning_effort = match rows.get("llm.reasoning_effort") {
        Some((raw, _)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                match validate_reasoning_effort(trimmed) {
                    Ok(effort) => Some(effort),
                    Err(e) => {
                        tracing::warn!(error = %e, "ignoring invalid stored llm.reasoning_effort");
                        config.llm_reasoning_effort.clone()
                    }
                }
            }
        }
        None => config.llm_reasoning_effort.clone(),
    };

    let create_tags = rows
        .get("llm.create_tags")
        .and_then(|(raw, _)| parse_bool_setting(raw))
        .unwrap_or(config.llm_create_tags);

    let monthly_budget_usd = match rows.get("llm.monthly_budget_usd") {
        Some((raw, _)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                match validate_budget(trimmed) {
                    Ok(budget) => Some(budget),
                    Err(e) => {
                        tracing::warn!(error = %e, "ignoring invalid stored llm.monthly_budget_usd");
                        config.llm_monthly_budget_usd
                    }
                }
            }
        }
        None => config.llm_monthly_budget_usd,
    };

    let prices = match rows.get("llm.prices") {
        Some((raw, _)) => {
            if raw.trim().is_empty() {
                HashMap::new()
            } else {
                match parse_llm_prices(raw) {
                    Ok(map) => map,
                    Err(e) => {
                        tracing::warn!(error = %e, "ignoring invalid stored llm.prices");
                        config.llm_prices.clone()
                    }
                }
            }
        }
        None => config.llm_prices.clone(),
    };

    LlmRuntimeConfig {
        enabled,
        api_key,
        api_key_source,
        base_url,
        provider,
        effective_provider,
        model,
        vision_model,
        handwriting_enabled,
        handwriting_model,
        allowed_models,
        reasoning_effort,
        create_tags,
        monthly_budget_usd,
        prices,
        has_overrides,
    }
}

fn load_calendar_settings(
    config: &Config,
    rows: &HashMap<String, (String, i64)>,
) -> CalendarRuntimeConfig {
    let has_overrides = rows.contains_key("calendar.lead_days");
    let lead_days = rows
        .get("calendar.lead_days")
        .and_then(|(raw, _)| {
            raw.trim()
                .parse::<u32>()
                .ok()
                .filter(|d| *d <= MAX_CALENDAR_LEAD_DAYS)
        })
        .unwrap_or(config.calendar_lead_days);
    CalendarRuntimeConfig {
        lead_days,
        has_overrides,
    }
}

fn load_inbox_settings(
    config: &Config,
    rows: &HashMap<String, (String, i64)>,
) -> InboxRuntimeConfig {
    let has_overrides = rows.keys().any(|k| k.starts_with("inbox."));
    let paused = rows
        .get("inbox.paused")
        .and_then(|(raw, _)| parse_bool_setting(raw))
        .unwrap_or(false);
    let user = match rows.get("inbox.user_id") {
        Some((raw, _)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                match trimmed.parse::<UserId>() {
                    Ok(uid) => Some(EmailUserTarget::UserId(uid)),
                    Err(e) => {
                        tracing::warn!(error = %e, "ignoring invalid stored inbox.user_id");
                        config.inbox_user.clone().map(EmailUserTarget::Username)
                    }
                }
            }
        }
        None => config.inbox_user.clone().map(EmailUserTarget::Username),
    };
    let interval_secs = rows
        .get("inbox.interval_secs")
        .and_then(|(raw, _)| {
            raw.trim()
                .parse::<u64>()
                .ok()
                .filter(|s| (1..=MAX_INBOX_INTERVAL_SECS).contains(s))
        })
        .unwrap_or(config.inbox_interval_secs);

    InboxRuntimeConfig {
        configured: config.inbox_dir.is_some(),
        paused,
        user,
        interval_secs,
        has_overrides,
    }
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

fn de_patch_string<'de, D>(deserializer: D) -> std::result::Result<PatchOpt<String>, D::Error>
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
    #[serde(default, deserialize_with = "de_patch_string")]
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

async fn resolve_target_user_id(
    db: &Db,
    target: Option<&EmailUserTarget>,
) -> Result<Option<UserId>> {
    match target {
        Some(EmailUserTarget::UserId(id)) => Ok(sqlx::query_scalar::<_, UserId>(
            "SELECT id FROM users WHERE id = ?",
        )
        .bind(*id)
        .fetch_optional(&db.read)
        .await?),
        Some(EmailUserTarget::Username(name)) => Ok(sqlx::query_scalar::<_, UserId>(
            "SELECT id FROM users WHERE username = ? AND disabled_at IS NULL",
        )
        .bind(name)
        .fetch_optional(&db.read)
        .await?),
        None => Ok(None),
    }
}

async fn ensure_active_user(
    db: &Db,
    uid: UserId,
    disabled_code: &'static str,
    disabled_message: &str,
) -> Result<()> {
    let user_row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT disabled_at FROM users WHERE id = ?")
            .bind(uid)
            .fetch_optional(&db.read)
            .await?;
    match user_row {
        None => Err(ServiceError::not_found("user_not_found", "no such user")),
        Some((Some(_),)) => Err(ServiceError::invalid(disabled_code, disabled_message)),
        Some((None,)) => Ok(()),
    }
}

/// Returns the current inbound email configuration for administrators.
pub async fn get_email_settings(ctx: &Ctx<'_>, endpoint_url: &str) -> Result<EmailSettingsView> {
    require_admin(ctx).await?;
    let email = ctx.app.runtime_settings().email;

    let user_id = resolve_target_user_id(&ctx.app.db, email.user.as_ref()).await?;

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
        ensure_active_user(
            &ctx.app.db,
            uid,
            "invalid_email_user",
            "cannot attribute emailed documents to a disabled user",
        )
        .await?;
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

/// Non-sensitive instance settings summary exposed to any signed-in user.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SettingsSummary {
    pub llm_enabled: bool,
    pub llm_configured: bool,
    pub llm_model: String,
    pub llm_handwriting_enabled: bool,
    pub llm_allowed_models: Vec<String>,
    pub llm_monthly_budget_usd: Option<String>,
    pub calendar_lead_days: u32,
    pub inbox_configured: bool,
    pub inbox_paused: bool,
}

/// Administrator view of runtime LLM settings (never includes the API key).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LlmSettingsView {
    pub enabled: bool,
    pub has_api_key: bool,
    pub api_key_source: String,
    pub base_url: String,
    pub provider: String,
    pub effective_provider: String,
    pub model: String,
    pub vision_model: String,
    pub handwriting_enabled: bool,
    pub handwriting_model: String,
    pub allowed_models: Vec<String>,
    pub reasoning_effort: Option<String>,
    pub create_tags: bool,
    pub monthly_budget_usd: Option<String>,
    pub prices: String,
    pub has_overrides: bool,
}

/// Administrator view of calendar settings.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CalendarSettingsView {
    pub lead_days: u32,
    pub has_overrides: bool,
}

/// Administrator view of watched inbox folder settings and recent rejections.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct InboxSettingsView {
    pub configured: bool,
    pub paused: bool,
    #[schema(value_type = Option<String>)]
    pub user_id: Option<UserId>,
    pub interval_secs: u64,
    pub failed_files: Vec<FailedInboxFile>,
    pub has_overrides: bool,
}

/// Complete administrator view returned by `GET /api/v1/settings` and `PATCH /api/v1/settings`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AdminSettingsView {
    pub llm: LlmSettingsView,
    pub calendar: CalendarSettingsView,
    pub inbox: InboxSettingsView,
    pub ocr_langs: String,
}

/// Partial update for runtime LLM settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct LlmSettingsPatch {
    pub enabled: Option<bool>,
    pub api_key: Option<String>,
    pub clear_api_key: Option<bool>,
    pub base_url: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub vision_model: Option<String>,
    pub handwriting_enabled: Option<bool>,
    pub handwriting_model: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    #[serde(default, deserialize_with = "de_patch_string")]
    #[schema(value_type = Option<String>)]
    pub reasoning_effort: PatchOpt<String>,
    pub create_tags: Option<bool>,
    #[serde(default, deserialize_with = "de_patch_string")]
    #[schema(value_type = Option<String>)]
    pub monthly_budget_usd: PatchOpt<String>,
    #[serde(default, deserialize_with = "de_patch_string")]
    #[schema(value_type = Option<String>)]
    pub prices: PatchOpt<String>,
}

/// Partial update for calendar settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct CalendarSettingsPatch {
    pub lead_days: Option<u32>,
}

/// Partial update for watched inbox folder settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct InboxSettingsPatch {
    pub paused: Option<bool>,
    #[serde(default, deserialize_with = "de_patch_user_id")]
    #[schema(value_type = Option<String>)]
    pub user_id: PatchOpt<UserId>,
    pub interval_secs: Option<u64>,
}

/// Partial update for runtime instance settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct AdminSettingsPatch {
    pub llm: Option<LlmSettingsPatch>,
    pub calendar: Option<CalendarSettingsPatch>,
    pub inbox: Option<InboxSettingsPatch>,
    pub reset_llm: Option<bool>,
    pub reset_calendar: Option<bool>,
    pub reset_inbox: Option<bool>,
}

struct ValidatedLlmPatch {
    enabled: Option<bool>,
    api_key: Option<String>,
    clear_api_key: bool,
    base_url: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    vision_model: Option<String>,
    handwriting_enabled: Option<bool>,
    handwriting_model: Option<String>,
    allowed_models: Option<Vec<String>>,
    reasoning_effort: PatchOpt<String>,
    create_tags: Option<bool>,
    monthly_budget_usd: PatchOpt<String>,
    prices: PatchOpt<String>,
}

fn validate_llm_patch(patch: LlmSettingsPatch) -> Result<ValidatedLlmPatch> {
    let base_url = patch
        .base_url
        .map(|raw| validate_http_url(&raw).map(|u| u.as_str().to_string()))
        .transpose()?;
    let provider = patch
        .provider
        .map(|raw| validate_provider(&raw))
        .transpose()?;
    let model = patch
        .model
        .map(|raw| validate_model_name(&raw))
        .transpose()?;
    let vision_model = patch
        .vision_model
        .map(|raw| validate_model_name(&raw))
        .transpose()?;
    let handwriting_model = patch
        .handwriting_model
        .map(|raw| validate_model_name(&raw))
        .transpose()?;
    let allowed_models = patch
        .allowed_models
        .map(|list| {
            let mut cleaned = Vec::with_capacity(list.len());
            for item in list {
                let m = validate_model_name(&item)?;
                if !cleaned.contains(&m) {
                    cleaned.push(m);
                }
            }
            Ok::<Vec<String>, ServiceError>(cleaned)
        })
        .transpose()?;
    let reasoning_effort = match patch.reasoning_effort {
        PatchOpt::Unset => PatchOpt::Unset,
        PatchOpt::Null => PatchOpt::Null,
        PatchOpt::Value(raw) => PatchOpt::Value(validate_reasoning_effort(&raw)?),
    };
    let monthly_budget_usd = match patch.monthly_budget_usd {
        PatchOpt::Unset => PatchOpt::Unset,
        PatchOpt::Null => PatchOpt::Null,
        PatchOpt::Value(raw) => PatchOpt::Value(validate_budget(&raw)?.to_string()),
    };
    let prices = match patch.prices {
        PatchOpt::Unset => PatchOpt::Unset,
        PatchOpt::Null => PatchOpt::Null,
        PatchOpt::Value(raw) => {
            let map = parse_llm_prices(&raw)
                .map_err(|reason| ServiceError::invalid("invalid_llm_prices", reason))?;
            PatchOpt::Value(format_llm_prices(&map))
        }
    };
    let api_key = patch
        .api_key
        .map(|raw| raw.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(ValidatedLlmPatch {
        enabled: patch.enabled,
        api_key,
        clear_api_key: patch.clear_api_key == Some(true),
        base_url,
        provider,
        model,
        vision_model,
        handwriting_enabled: patch.handwriting_enabled,
        handwriting_model,
        allowed_models,
        reasoning_effort,
        create_tags: patch.create_tags,
        monthly_budget_usd,
        prices,
    })
}

async fn upsert_app_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    value: &str,
    encrypted: bool,
    now: &str,
    updated_by: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO app_settings (key, value, encrypted, updated_at, updated_by)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET
             value = excluded.value,
             encrypted = excluded.encrypted,
             updated_at = excluded.updated_at,
             updated_by = excluded.updated_by",
    )
    .bind(key)
    .bind(value)
    .bind(i64::from(encrypted))
    .bind(now)
    .bind(updated_by)
    .execute(&mut **tx)
    .await?;
    if !changed_keys.iter().any(|k| k == key) {
        changed_keys.push(key.to_string());
    }
    Ok(())
}

async fn delete_app_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    let res = sqlx::query("DELETE FROM app_settings WHERE key = ?")
        .bind(key)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() > 0 && !changed_keys.iter().any(|k| k == key) {
        changed_keys.push(key.to_string());
    }
    Ok(())
}

/// Returns non-sensitive runtime settings for any signed-in user.
pub async fn get_settings_summary(ctx: &Ctx<'_>) -> Result<SettingsSummary> {
    if ctx.actor.via != Via::Cli {
        ctx.actor.require_user()?;
    }
    let runtime = ctx.app.runtime_settings();
    let llm_configured = runtime.llm.api_key.is_some();
    let llm_handwriting_enabled = runtime.llm.effective_handwriting_model().is_some();
    Ok(SettingsSummary {
        llm_enabled: runtime.llm.enabled && llm_configured,
        llm_configured,
        llm_model: runtime.llm.model,
        llm_handwriting_enabled,
        llm_allowed_models: runtime.llm.allowed_models,
        llm_monthly_budget_usd: runtime.llm.monthly_budget_usd.map(|d| d.to_string()),
        calendar_lead_days: runtime.calendar.lead_days,
        inbox_configured: runtime.inbox.configured,
        inbox_paused: runtime.inbox.paused,
    })
}

/// Returns the full administrator view of runtime settings (without secret values).
pub async fn get_admin_settings(ctx: &Ctx<'_>) -> Result<AdminSettingsView> {
    require_admin(ctx).await?;
    let runtime = ctx.app.runtime_settings();
    let inbox_user_id = resolve_target_user_id(&ctx.app.db, runtime.inbox.user.as_ref()).await?;
    let failed_files = match ctx.app.config.inbox_dir.clone() {
        Some(dir) => tokio::task::spawn_blocking(move || inbox::list_failed_files(&dir))
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };

    Ok(AdminSettingsView {
        llm: LlmSettingsView {
            enabled: runtime.llm.enabled,
            has_api_key: runtime.llm.api_key.is_some(),
            api_key_source: runtime.llm.api_key_source.to_string(),
            base_url: runtime.llm.base_url.as_str().to_string(),
            provider: runtime.llm.provider,
            effective_provider: runtime.llm.effective_provider.as_str().to_string(),
            model: runtime.llm.model,
            vision_model: runtime.llm.vision_model,
            handwriting_enabled: runtime.llm.handwriting_enabled,
            handwriting_model: runtime.llm.handwriting_model,
            allowed_models: runtime.llm.allowed_models,
            reasoning_effort: runtime.llm.reasoning_effort,
            create_tags: runtime.llm.create_tags,
            monthly_budget_usd: runtime.llm.monthly_budget_usd.map(|d| d.to_string()),
            prices: format_llm_prices(&runtime.llm.prices),
            has_overrides: runtime.llm.has_overrides,
        },
        calendar: CalendarSettingsView {
            lead_days: runtime.calendar.lead_days,
            has_overrides: runtime.calendar.has_overrides,
        },
        inbox: InboxSettingsView {
            configured: runtime.inbox.configured,
            paused: runtime.inbox.paused,
            user_id: inbox_user_id,
            interval_secs: runtime.inbox.interval_secs,
            failed_files,
            has_overrides: runtime.inbox.has_overrides,
        },
        ocr_langs: ctx.app.config.ocr_langs.clone(),
    })
}

async fn upsert_opt_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    value: Option<&str>,
    now: &str,
    updated_by: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    if let Some(value) = value {
        upsert_app_setting(tx, key, value, false, now, updated_by, changed_keys).await?;
    }
    Ok(())
}

async fn upsert_bool_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    value: Option<bool>,
    now: &str,
    updated_by: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    if let Some(value) = value {
        let text = if value { "true" } else { "false" };
        upsert_app_setting(tx, key, text, false, now, updated_by, changed_keys).await?;
    }
    Ok(())
}

async fn apply_clearable_setting(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    patch: PatchOpt<String>,
    now: &str,
    updated_by: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    match patch {
        PatchOpt::Unset => Ok(()),
        PatchOpt::Null => {
            upsert_app_setting(tx, key, "", false, now, updated_by, changed_keys).await
        }
        PatchOpt::Value(value) => {
            upsert_app_setting(tx, key, &value, false, now, updated_by, changed_keys).await
        }
    }
}

async fn apply_llm_patch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    secret_key: &[u8],
    llm: ValidatedLlmPatch,
    now: &str,
    actor_id: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    upsert_bool_setting(tx, "llm.enabled", llm.enabled, now, actor_id, changed_keys).await?;
    if llm.clear_api_key {
        delete_app_setting(tx, LLM_API_KEY_SETTING, changed_keys).await?;
    } else if let Some(plain_key) = llm.api_key {
        let encrypted = crypto::encrypt_secret(secret_key, LLM_API_KEY_SETTING, &plain_key)?;
        upsert_app_setting(
            tx,
            LLM_API_KEY_SETTING,
            &encrypted,
            true,
            now,
            actor_id,
            changed_keys,
        )
        .await?;
    }
    upsert_opt_setting(
        tx,
        "llm.base_url",
        llm.base_url.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_opt_setting(
        tx,
        "llm.provider",
        llm.provider.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_opt_setting(
        tx,
        "llm.model",
        llm.model.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_opt_setting(
        tx,
        "llm.vision_model",
        llm.vision_model.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_bool_setting(
        tx,
        "llm.handwriting_enabled",
        llm.handwriting_enabled,
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_opt_setting(
        tx,
        "llm.handwriting_model",
        llm.handwriting_model.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    if let Some(allowed) = llm.allowed_models {
        if allowed.is_empty() {
            delete_app_setting(tx, "llm.allowed_models", changed_keys).await?;
        } else {
            let encoded = serde_json::to_string(&allowed)
                .map_err(|e| ServiceError::internal("json_encode_failed", e.to_string()))?;
            upsert_app_setting(
                tx,
                "llm.allowed_models",
                &encoded,
                false,
                now,
                actor_id,
                changed_keys,
            )
            .await?;
        }
    }
    apply_clearable_setting(
        tx,
        "llm.reasoning_effort",
        llm.reasoning_effort,
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    upsert_bool_setting(
        tx,
        "llm.create_tags",
        llm.create_tags,
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    apply_clearable_setting(
        tx,
        "llm.monthly_budget_usd",
        llm.monthly_budget_usd,
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    apply_clearable_setting(tx, "llm.prices", llm.prices, now, actor_id, changed_keys).await
}

async fn apply_inbox_patch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    inbox: InboxSettingsPatch,
    now: &str,
    actor_id: Option<UserId>,
    changed_keys: &mut Vec<String>,
) -> Result<()> {
    upsert_bool_setting(
        tx,
        "inbox.paused",
        inbox.paused,
        now,
        actor_id,
        changed_keys,
    )
    .await?;
    let user_patch = match inbox.user_id {
        PatchOpt::Unset => PatchOpt::Unset,
        PatchOpt::Null => PatchOpt::Null,
        PatchOpt::Value(uid) => PatchOpt::Value(uid.to_string()),
    };
    apply_clearable_setting(tx, "inbox.user_id", user_patch, now, actor_id, changed_keys).await?;
    let interval_str = inbox.interval_secs.map(|s| s.to_string());
    upsert_opt_setting(
        tx,
        "inbox.interval_secs",
        interval_str.as_deref(),
        now,
        actor_id,
        changed_keys,
    )
    .await
}

/// Validates and persists runtime setting overrides, audits key names, and hot-reloads [`App`].
pub async fn update_admin_settings(
    ctx: &Ctx<'_>,
    patch: AdminSettingsPatch,
) -> Result<AdminSettingsView> {
    require_admin_session(ctx).await?;

    let llm_patch = patch.llm.map(validate_llm_patch).transpose()?;
    let calendar_lead_days = patch
        .calendar
        .and_then(|c| c.lead_days)
        .map(|days| {
            if days > MAX_CALENDAR_LEAD_DAYS {
                return Err(ServiceError::invalid(
                    "invalid_calendar_lead_days",
                    format!("calendar lead days must be between 0 and {MAX_CALENDAR_LEAD_DAYS}"),
                ));
            }
            Ok(days)
        })
        .transpose()?;

    let inbox_patch = patch.inbox;
    if let Some(ref inbox) = inbox_patch {
        if let Some(secs) = inbox.interval_secs
            && !(1..=MAX_INBOX_INTERVAL_SECS).contains(&secs)
        {
            return Err(ServiceError::invalid(
                "invalid_inbox_interval",
                format!("inbox interval must be between 1 and {MAX_INBOX_INTERVAL_SECS} seconds"),
            ));
        }
        if let PatchOpt::Value(uid) = inbox.user_id {
            ensure_active_user(
                &ctx.app.db,
                uid,
                "invalid_inbox_user",
                "cannot attribute inbox documents to a disabled user",
            )
            .await?;
        }
    }

    let mut tx = ctx.app.db.write.begin().await?;
    let mut changed_keys: Vec<String> = Vec::new();
    let now = time::now_ts();
    let actor_id = ctx.actor.user_id;

    if patch.reset_llm == Some(true) {
        sqlx::query("DELETE FROM app_settings WHERE key LIKE 'llm.%'")
            .execute(&mut *tx)
            .await?;
        changed_keys.push("llm.*".to_string());
    }
    if patch.reset_calendar == Some(true) {
        sqlx::query("DELETE FROM app_settings WHERE key = 'calendar.lead_days'")
            .execute(&mut *tx)
            .await?;
        changed_keys.push("calendar.lead_days".to_string());
    }
    if patch.reset_inbox == Some(true) {
        sqlx::query("DELETE FROM app_settings WHERE key LIKE 'inbox.%'")
            .execute(&mut *tx)
            .await?;
        changed_keys.push("inbox.*".to_string());
    }

    if let Some(llm) = llm_patch {
        apply_llm_patch(
            &mut tx,
            ctx.app.config.secret_key.expose_secret().as_bytes(),
            llm,
            &now,
            actor_id,
            &mut changed_keys,
        )
        .await?;
    }

    let lead_str = calendar_lead_days.map(|d| d.to_string());
    upsert_opt_setting(
        &mut tx,
        "calendar.lead_days",
        lead_str.as_deref(),
        &now,
        actor_id,
        &mut changed_keys,
    )
    .await?;

    if let Some(inbox) = inbox_patch {
        apply_inbox_patch(&mut tx, inbox, &now, actor_id, &mut changed_keys).await?;
    }

    if !changed_keys.is_empty() {
        events::record(
            &mut tx,
            &ctx.actor,
            "settings.update",
            None,
            json!({ "keys": changed_keys }),
        )
        .await?;
    }
    tx.commit().await?;

    ctx.app.reload_settings().await?;
    get_admin_settings(ctx).await
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
    fn admin_patch_opt_fields_distinguish_unset_null_and_value() {
        let omitted: AdminSettingsPatch = serde_json::from_str(r#"{"llm":{},"inbox":{}}"#).unwrap();
        let llm = omitted.llm.unwrap();
        assert_eq!(llm.reasoning_effort, PatchOpt::Unset);
        assert_eq!(llm.monthly_budget_usd, PatchOpt::Unset);
        assert_eq!(llm.prices, PatchOpt::Unset);
        assert_eq!(omitted.inbox.unwrap().user_id, PatchOpt::Unset);

        let cleared: AdminSettingsPatch = serde_json::from_str(
            r#"{"llm":{"reasoning_effort":null,"monthly_budget_usd":"","prices":"  "},"inbox":{"user_id":""}}"#,
        )
        .unwrap();
        let llm = cleared.llm.unwrap();
        assert_eq!(llm.reasoning_effort, PatchOpt::Null);
        assert_eq!(llm.monthly_budget_usd, PatchOpt::Null);
        assert_eq!(llm.prices, PatchOpt::Null);
        assert_eq!(cleared.inbox.unwrap().user_id, PatchOpt::Null);
    }

    #[test]
    fn prices_serialize_canonically_sorted() {
        let parsed = parse_llm_prices("z-model=1:2, a-model=0.40:1.60").unwrap();
        assert_eq!(format_llm_prices(&parsed), "a-model=0.40:1.60,z-model=1:2");
    }

    #[tokio::test]
    async fn corrupt_encrypted_api_key_logs_and_does_not_fail_load() {
        let (_dir, app) = crate::service::testing::test_app().await;
        sqlx::query(
            "INSERT INTO app_settings (key, value, encrypted, updated_at) VALUES (?, ?, 1, ?)",
        )
        .bind("llm.api_key")
        .bind("v1:not-valid-ciphertext")
        .bind(time::now_ts())
        .execute(&app.db.write)
        .await
        .unwrap();

        let loaded = load(&app.config, &app.db).await.unwrap();
        assert!(loaded.llm.api_key.is_none());
        assert_eq!(loaded.llm.api_key_source, "none");
        assert!(loaded.llm.has_overrides);
    }

    #[test]
    fn secret_verifier_redacts_debug_output() {
        let a = EmailSecretVerifier::DbHash("super-sensitive-hash".into());
        let b = EmailSecretVerifier::EnvPlain(SecretString::from("super-sensitive-secret"));
        let dbg = format!("{a:?} {b:?}");
        assert!(!dbg.contains("super-sensitive"));
    }
}
