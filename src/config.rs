//! Configuration from `PAPERJAM_*` environment variables (and an optional `.env`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use rust_decimal::Decimal;
use secrecy::SecretString;
use url::Url;

const MIN_SECRET_KEY_BYTES: usize = 32;
const DEFAULT_LLM_MODEL: &str = "anthropic/claude-haiku-5.5";
/// Re-reads handwritten pages on OpenRouter: the default vision model misreads
/// cursive names, dates and personal numbers that this one reads.
const DEFAULT_HANDWRITING_MODEL: &str = "anthropic/claude-sonnet-5.5";
const DEFAULT_LLM_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// The dialect of the chat-completions endpoint behind the LLM layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    /// OpenRouter: routing flags, `reasoning` object, usage with cost.
    OpenRouter,
    /// Any other OpenAI-compatible endpoint: standard fields only.
    Generic,
}

impl LlmProvider {
    /// OpenRouter exactly when the host is `openrouter.ai`.
    pub fn from_base_url(url: &Url) -> Self {
        if url.host_str() == Some("openrouter.ai") {
            LlmProvider::OpenRouter
        } else {
            LlmProvider::Generic
        }
    }

    /// How error messages and logs name the service, at the start of a
    /// sentence.
    pub fn name(self) -> &'static str {
        match self {
            LlmProvider::OpenRouter => "OpenRouter",
            LlmProvider::Generic => "The LLM API",
        }
    }
}

/// What a model costs, in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPrice {
    pub input_per_mtok: Decimal,
    pub output_per_mtok: Decimal,
}

/// `PAPERJAM_LLM_PRICES`: `model=input:output,model=input:output`, where the
/// last `=` ends the model id (ids may contain `:` or `/`).
fn llm_prices(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<HashMap<String, ModelPrice>, ConfigError> {
    const VAR: &str = "PAPERJAM_LLM_PRICES";
    let Some(raw) = get(VAR) else {
        return Ok(HashMap::new());
    };
    let invalid = |reason: String| ConfigError::Invalid { var: VAR, reason };
    let mut prices = HashMap::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let bad = || invalid(format!("{entry:?} is not model=input:output"));
        let (model, pair) = entry.rsplit_once('=').ok_or_else(bad)?;
        let (input, output) = pair.split_once(':').ok_or_else(bad)?;
        let number = |text: &str| {
            text.trim()
                .parse::<Decimal>()
                .ok()
                .filter(|d| !d.is_sign_negative())
                .ok_or_else(|| invalid(format!("{text:?} is not a price in USD per million")))
        };
        let model = model.trim();
        if model.is_empty() {
            return Err(bad());
        }
        let price = ModelPrice {
            input_per_mtok: number(input)?,
            output_per_mtok: number(output)?,
        };
        prices.insert(model.to_string(), price);
    }
    Ok(prices)
}

fn parse<T>(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<T>, ConfigError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    get(var)
        .map(|raw| {
            raw.trim().parse::<T>().map_err(|e| ConfigError::Invalid {
                var,
                reason: e.to_string(),
            })
        })
        .transpose()
}

fn parse_bool(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Option<bool>, ConfigError> {
    get(var)
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            other => Err(ConfigError::Invalid {
                var,
                reason: format!("expected true or false, got {other:?}"),
            }),
        })
        .transpose()
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },
    #[error("the secret key file {path} cannot be used: {reason}")]
    SecretFile { path: String, reason: String },
}

/// The file in the data directory that holds the secret key when
/// `PAPERJAM_SECRET_KEY` is not set. It is created on first start.
pub const SECRET_KEY_FILE: &str = "secret.key";

/// Random bytes in a generated secret key (twice as many hex characters).
const GENERATED_SECRET_BYTES: usize = 48;

/// The secret key kept in `<data_dir>/secret.key`, generated and stored on
/// first use. Several processes may start at once (the server and a CLI
/// command in the same container): the file is created exclusively and the
/// loser of the race reads what the winner wrote.
fn secret_key_from_file(data_dir: &Path) -> Result<String, ConfigError> {
    let path = data_dir.join(SECRET_KEY_FILE);
    let fail = |reason: String| ConfigError::SecretFile {
        path: path.display().to_string(),
        reason,
    };
    for _ in 0..50 {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let key = text.trim().to_string();
                if key.len() >= MIN_SECRET_KEY_BYTES {
                    return Ok(key);
                }
                // A writer that has created the file but not yet filled it.
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(fail(e.to_string())),
        }
        std::fs::create_dir_all(data_dir).map_err(|e| fail(e.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let key = crate::random::hex(GENERATED_SECRET_BYTES);
                file.write_all(key.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| fail(e.to_string()))?;
                return Ok(key);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(fail(e.to_string())),
        }
    }
    Err(fail(format!(
        "it must hold at least {MIN_SECRET_KEY_BYTES} characters"
    )))
}

/// The first administrator, created at start-up when no user exists yet.
#[derive(Debug, Clone)]
pub struct BootstrapAdmin {
    pub username: String,
    pub password: SecretString,
}

fn bootstrap_admin(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<BootstrapAdmin>, ConfigError> {
    match (get("PAPERJAM_ADMIN_USER"), get("PAPERJAM_ADMIN_PASSWORD")) {
        (None, None) => Ok(None),
        (Some(username), Some(password)) => Ok(Some(BootstrapAdmin {
            username: username.trim().to_string(),
            password: SecretString::from(password),
        })),
        (None, _) => Err(ConfigError::Missing("PAPERJAM_ADMIN_USER")),
        (_, None) => Err(ConfigError::Missing("PAPERJAM_ADMIN_PASSWORD")),
    }
}

/// An OAuth client the operator configured by hand (no registration needed).
#[derive(Debug, Clone)]
pub struct StaticOAuthClient {
    pub id: String,
    pub secret: SecretString,
    pub redirect_uris: Vec<String>,
}

fn static_oauth_client(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<StaticOAuthClient>, ConfigError> {
    let id = get("PAPERJAM_OAUTH_STATIC_CLIENT_ID");
    let secret = get("PAPERJAM_OAUTH_STATIC_CLIENT_SECRET");
    let uris = get("PAPERJAM_OAUTH_STATIC_REDIRECT_URIS");
    let (id, secret, uris) = match (id, secret, uris) {
        (None, None, None) => return Ok(None),
        (Some(id), Some(secret), Some(uris)) => (id, secret, uris),
        (None, _, _) => return Err(ConfigError::Missing("PAPERJAM_OAUTH_STATIC_CLIENT_ID")),
        (_, None, _) => return Err(ConfigError::Missing("PAPERJAM_OAUTH_STATIC_CLIENT_SECRET")),
        (_, _, None) => return Err(ConfigError::Missing("PAPERJAM_OAUTH_STATIC_REDIRECT_URIS")),
    };
    let redirect_uris: Vec<String> = uris
        .split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .collect();
    if redirect_uris.is_empty() {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_OAUTH_STATIC_REDIRECT_URIS",
            reason: "list at least one redirect URI".to_string(),
        });
    }
    if secret.len() < MIN_SECRET_KEY_BYTES {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_OAUTH_STATIC_CLIENT_SECRET",
            reason: format!("must be at least {MIN_SECRET_KEY_BYTES} bytes"),
        });
    }
    Ok(Some(StaticOAuthClient {
        id: id.trim().to_string(),
        secret: SecretString::from(secret),
        redirect_uris,
    }))
}

const DEFAULT_INBOX_INTERVAL_SECS: u64 = 30;
const MAX_INBOX_INTERVAL_SECS: u64 = 86_400;

/// Absolute, symlink-free form of `path` for comparisons. The longest existing
/// ancestor is canonicalized; a tail that does not exist yet is normalized
/// lexically (it cannot hold a symlink).
fn resolve(path: &std::path::Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut head = absolute.clone();
    let mut tail = Vec::new();
    let mut resolved = loop {
        if let Ok(real) = head.canonicalize() {
            break real;
        }
        match (head.file_name(), head.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_owned());
                head = parent.to_path_buf();
            }
            _ => break absolute,
        }
    };
    for part in tail.into_iter().rev() {
        match part.to_str() {
            Some("..") => {
                resolved.pop();
            }
            Some(".") => {}
            _ => resolved.push(part),
        }
    }
    resolved
}

type InboxSettings = (Option<PathBuf>, Option<String>, u64);

fn inbox_settings(
    get: &impl Fn(&str) -> Option<String>,
    data_dir: &std::path::Path,
) -> Result<InboxSettings, ConfigError> {
    let interval: Option<u64> = parse(get, "PAPERJAM_INBOX_INTERVAL_SECS")?;
    if let Some(secs) = interval
        && !(1..=MAX_INBOX_INTERVAL_SECS).contains(&secs)
    {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_INBOX_INTERVAL_SECS",
            reason: format!("must be between 1 and {MAX_INBOX_INTERVAL_SECS}"),
        });
    }
    let user = get("PAPERJAM_INBOX_USER")
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    let Some(dir) = get("PAPERJAM_INBOX_DIR").map(PathBuf::from) else {
        if user.is_some() || interval.is_some() {
            return Err(ConfigError::Missing("PAPERJAM_INBOX_DIR"));
        }
        return Ok((None, None, DEFAULT_INBOX_INTERVAL_SECS));
    };
    // The scanner deletes what it ingests, so it must never see the archive.
    let (real_dir, real_data) = (resolve(&dir), resolve(data_dir));
    if real_dir.starts_with(&real_data) || real_data.starts_with(&real_dir) {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_INBOX_DIR",
            reason: "must not contain or sit inside PAPERJAM_DATA_DIR".to_string(),
        });
    }
    Ok((
        Some(dir),
        user,
        interval.unwrap_or(DEFAULT_INBOX_INTERVAL_SECS),
    ))
}

pub(crate) const MAX_EMAIL_TAG_CHARS: usize = 64;

/// Who may submit documents over the inbound email webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderRule {
    Exact(String),
    Domain(String),
}

impl SenderRule {
    pub fn as_str(&self) -> &str {
        match self {
            SenderRule::Exact(s) | SenderRule::Domain(s) => s,
        }
    }

    pub fn matches(&self, addr: &str) -> bool {
        let norm = addr.trim().to_ascii_lowercase();
        match self {
            SenderRule::Exact(exact) => norm == *exact,
            SenderRule::Domain(domain) => norm.split_once('@').is_some_and(|(local, host)| {
                !local.is_empty() && !host.contains('@') && host == &domain[1..]
            }),
        }
    }
}

/// Inbound email webhook settings; `None` keeps `POST /api/v1/ingest/email` off.
#[derive(Debug, Clone)]
pub struct EmailIntakeConfig {
    pub secret: SecretString,
    pub user: Option<String>,
    pub allowed_senders: Vec<SenderRule>,
    pub body_fallback: bool,
    pub local_only: bool,
    pub tag: Option<String>,
}

fn valid_domain_part(domain: &str) -> bool {
    !domain.is_empty()
        && domain.contains('.')
        && !domain.contains('@')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

pub(crate) fn parse_sender_rule(entry: &str) -> Result<SenderRule, ConfigError> {
    const VAR: &str = "PAPERJAM_EMAIL_ALLOWED_SENDERS";
    let item = entry.trim().to_ascii_lowercase();
    if item.is_empty() || item.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid {
            var: VAR,
            reason: format!("{entry:?} is not a sender address or @domain"),
        });
    }
    if let Some(domain) = item.strip_prefix('@') {
        if !valid_domain_part(domain) {
            return Err(ConfigError::Invalid {
                var: VAR,
                reason: format!("{entry:?} is not a valid @domain rule"),
            });
        }
        return Ok(SenderRule::Domain(item));
    }
    let Some((local, domain)) = item.split_once('@') else {
        return Err(ConfigError::Invalid {
            var: VAR,
            reason: format!("{entry:?} is not a sender address or @domain"),
        });
    };
    if local.is_empty() || !valid_domain_part(domain) {
        return Err(ConfigError::Invalid {
            var: VAR,
            reason: format!("{entry:?} is not a valid sender address"),
        });
    }
    Ok(SenderRule::Exact(item))
}

fn email_intake_settings(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<EmailIntakeConfig>, ConfigError> {
    let secret = get("PAPERJAM_EMAIL_SECRET");
    let user = get("PAPERJAM_EMAIL_USER").map(|u| u.trim().to_string());
    let senders = get("PAPERJAM_EMAIL_ALLOWED_SENDERS");
    let body_fallback = parse_bool(get, "PAPERJAM_EMAIL_BODY_FALLBACK")?;
    let local_only = parse_bool(get, "PAPERJAM_EMAIL_LOCAL_ONLY")?;
    let tag = get("PAPERJAM_EMAIL_TAG").map(|t| t.trim().to_string());

    let Some(secret) = secret else {
        if user.is_some()
            || senders.is_some()
            || body_fallback.is_some()
            || local_only.is_some()
            || tag.is_some()
        {
            return Err(ConfigError::Missing("PAPERJAM_EMAIL_SECRET"));
        }
        return Ok(None);
    };

    let secret = secret.trim().to_string();
    if secret.len() < MIN_SECRET_KEY_BYTES {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_EMAIL_SECRET",
            reason: format!("must be at least {MIN_SECRET_KEY_BYTES} bytes"),
        });
    }

    let allowed_senders = match senders {
        None => {
            tracing::warn!(
                "PAPERJAM_EMAIL_ALLOWED_SENDERS is unset: any sender that reaches the email endpoint can add documents"
            );
            Vec::new()
        }
        Some(raw) => {
            let mut rules = Vec::new();
            for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                rules.push(parse_sender_rule(entry)?);
            }
            if rules.is_empty() {
                return Err(ConfigError::Invalid {
                    var: "PAPERJAM_EMAIL_ALLOWED_SENDERS",
                    reason: "list at least one sender address or @domain".to_string(),
                });
            }
            rules
        }
    };

    if let Some(ref t) = tag
        && (t.chars().count() > MAX_EMAIL_TAG_CHARS || t.chars().any(char::is_control))
    {
        return Err(ConfigError::Invalid {
            var: "PAPERJAM_EMAIL_TAG",
            reason: format!("must be at most {MAX_EMAIL_TAG_CHARS} printable characters"),
        });
    }

    Ok(Some(EmailIntakeConfig {
        secret: SecretString::from(secret),
        user,
        allowed_senders,
        body_fallback: body_fallback.unwrap_or(true),
        local_only: local_only.unwrap_or(false),
        tag,
    }))
}

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    pub public_url: Option<Url>,
    pub secret_key: SecretString,
    pub ocr_langs: String,
    pub ocr_workers: usize,
    /// How long the process that parses a PDF or image may stay silent
    /// before it is killed.
    pub extract_timeout: std::time::Duration,
    /// Address-space limit of that process in MiB (Linux); 0 for none.
    pub extract_memory_mb: u64,
    /// Only tests change these two: the program (and its arguments) that
    /// stands in for `paperjam extract-worker`. `None` runs this binary.
    pub extract_worker_program: Option<PathBuf>,
    pub extract_worker_args: Vec<String>,
    pub llm_workers: usize,
    /// `PAPERJAM_LLM_API_KEY`, or the older `PAPERJAM_OPENROUTER_API_KEY`.
    pub llm_api_key: Option<SecretString>,
    /// API root of any OpenAI-compatible endpoint; OpenRouter by default.
    pub llm_base_url: Url,
    /// Which dialect the endpoint speaks; derived from the host unless set.
    pub llm_provider: LlmProvider,
    /// USD per million tokens, by model, for endpoints that report no cost.
    pub llm_prices: HashMap<String, ModelPrice>,
    pub llm_model: String,
    pub llm_vision_model: String,
    /// Second reading of pages the vision model reports as handwritten;
    /// `None` keeps the first reading.
    pub llm_handwriting_model: Option<String>,
    pub llm_allowed_models: Vec<String>,
    /// Only what the operator set; OpenRouter then falls back to `low` and
    /// other endpoints get no reasoning field at all.
    pub llm_reasoning_effort: Option<String>,
    pub llm_create_tags: bool,
    pub llm_monthly_budget_usd: Option<Decimal>,
    pub max_upload_bytes: u64,
    /// Days before a due date that the calendar feed adds a "renew" event; 0 turns it off.
    pub calendar_lead_days: u32,
    pub oauth_static_client: Option<StaticOAuthClient>,
    /// Creates the first administrator at start-up when no user exists.
    pub bootstrap_admin: Option<BootstrapAdmin>,
    /// Watched folder; `None` keeps the feature off.
    pub inbox_dir: Option<PathBuf>,
    pub inbox_user: Option<String>,
    pub inbox_interval_secs: u64,
    /// Inbound email webhook; `None` keeps the feature off.
    pub email_intake: Option<EmailIntakeConfig>,
    /// Proxies whose client-address header is believed.
    pub trusted_proxies: Vec<ipnet::IpNet>,
    /// The header a trusted proxy puts the client address in.
    pub client_ip_header: axum::http::HeaderName,
}

impl Config {
    /// Reads `.env` if present, then the process environment.
    pub fn from_env() -> Result<Config, ConfigError> {
        let _ = dotenvy::dotenv();
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds a config from an arbitrary variable lookup (tests use a map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        // An empty value (`PAPERJAM_X=` in a .env file) counts as unset.
        let get = |name: &str| get(name).filter(|v| !v.trim().is_empty());

        let data_dir =
            get("PAPERJAM_DATA_DIR").map_or_else(|| PathBuf::from("/data"), PathBuf::from);
        // The variable wins; without it the key lives in the data directory.
        let secret_key = match get("PAPERJAM_SECRET_KEY") {
            Some(key) if key.len() < MIN_SECRET_KEY_BYTES => {
                return Err(ConfigError::Invalid {
                    var: "PAPERJAM_SECRET_KEY",
                    reason: format!("must be at least {MIN_SECRET_KEY_BYTES} bytes"),
                });
            }
            Some(key) => key,
            None => secret_key_from_file(&data_dir)?,
        };

        let llm_model = get("PAPERJAM_LLM_MODEL").unwrap_or_else(|| DEFAULT_LLM_MODEL.to_string());
        let llm_vision_model =
            get("PAPERJAM_LLM_VISION_MODEL").unwrap_or_else(|| llm_model.clone());

        // The new name wins; the old one keeps existing deployments working.
        let base_url_var = if get("PAPERJAM_LLM_BASE_URL").is_some() {
            "PAPERJAM_LLM_BASE_URL"
        } else {
            "PAPERJAM_OPENROUTER_BASE_URL"
        };
        let llm_base_url = match parse(&get, base_url_var)? {
            Some(url) => url,
            None => Url::parse(DEFAULT_LLM_BASE_URL).expect("a valid default URL"),
        };
        let llm_provider = match get("PAPERJAM_LLM_PROVIDER") {
            None => LlmProvider::from_base_url(&llm_base_url),
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "auto" => LlmProvider::from_base_url(&llm_base_url),
                "openrouter" => LlmProvider::OpenRouter,
                "generic" => LlmProvider::Generic,
                other => {
                    return Err(ConfigError::Invalid {
                        var: "PAPERJAM_LLM_PROVIDER",
                        reason: format!("expected auto, openrouter or generic, got {other:?}"),
                    });
                }
            },
        };

        // Only OpenRouter is known to serve the default; `off` turns it off.
        let llm_handwriting_model = match get("PAPERJAM_LLM_HANDWRITING_MODEL") {
            Some(raw) if raw.trim().eq_ignore_ascii_case("off") => None,
            Some(raw) => Some(raw.trim().to_string()),
            None => (llm_provider == LlmProvider::OpenRouter)
                .then(|| DEFAULT_HANDWRITING_MODEL.to_string()),
        }
        .filter(|m| *m != llm_vision_model);
        let llm_allowed_models: Vec<String> = match get("PAPERJAM_LLM_ALLOWED_MODELS") {
            Some(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            None => {
                let mut models = vec![llm_model.clone()];
                for extra in std::iter::once(&llm_vision_model).chain(&llm_handwriting_model) {
                    if !models.contains(extra) {
                        models.push(extra.clone());
                    }
                }
                models
            }
        };

        let (inbox_dir, inbox_user, inbox_interval_secs) = inbox_settings(&get, &data_dir)?;
        let email_intake = email_intake_settings(&get)?;

        Ok(Config {
            data_dir,
            bind: parse(&get, "PAPERJAM_BIND")?
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8140))),
            public_url: parse(&get, "PAPERJAM_PUBLIC_URL")?,
            secret_key: SecretString::from(secret_key),
            ocr_langs: get("PAPERJAM_OCR_LANGS").unwrap_or_else(|| "eng".to_string()),
            ocr_workers: parse(&get, "PAPERJAM_OCR_WORKERS")?.unwrap_or(1),
            extract_timeout: std::time::Duration::from_secs(extract_timeout_secs(&get)?),
            extract_memory_mb: parse(&get, "PAPERJAM_EXTRACT_MEMORY_MB")?.unwrap_or(1536),
            extract_worker_program: None,
            extract_worker_args: Vec::new(),
            llm_workers: parse(&get, "PAPERJAM_LLM_WORKERS")?.unwrap_or(2),
            llm_api_key: get("PAPERJAM_LLM_API_KEY")
                .or_else(|| get("PAPERJAM_OPENROUTER_API_KEY"))
                .map(SecretString::from),
            llm_base_url,
            llm_provider,
            llm_prices: llm_prices(&get)?,
            llm_model,
            llm_vision_model,
            llm_handwriting_model,
            llm_allowed_models,
            llm_reasoning_effort: get("PAPERJAM_LLM_REASONING_EFFORT"),
            llm_create_tags: parse_bool(&get, "PAPERJAM_LLM_CREATE_TAGS")?.unwrap_or(true),
            llm_monthly_budget_usd: parse(&get, "PAPERJAM_LLM_MONTHLY_BUDGET_USD")?,
            max_upload_bytes: parse(&get, "PAPERJAM_MAX_UPLOAD_BYTES")?
                .unwrap_or(100 * 1024 * 1024),
            calendar_lead_days: parse(&get, "PAPERJAM_CALENDAR_LEAD_DAYS")?.unwrap_or(60),
            oauth_static_client: static_oauth_client(&get)?,
            bootstrap_admin: bootstrap_admin(&get)?,
            inbox_dir,
            inbox_user,
            inbox_interval_secs,
            email_intake,
            trusted_proxies: trusted_proxies(&get)?,
            client_ip_header: client_ip_header(&get)?,
        })
    }

    /// Whether `model` is allowed for LLM reprocessing.
    pub fn is_model_allowed(&self, model: &str) -> bool {
        self.llm_allowed_models.iter().any(|m| m == model)
    }
}

fn extract_timeout_secs(get: &impl Fn(&str) -> Option<String>) -> Result<u64, ConfigError> {
    const VAR: &str = "PAPERJAM_EXTRACT_TIMEOUT_SECS";
    match parse::<u64>(get, VAR)? {
        None => Ok(120),
        Some(0) => Err(ConfigError::Invalid {
            var: VAR,
            reason: "must be at least 1".to_string(),
        }),
        Some(secs) => Ok(secs),
    }
}

/// Loopback and the private range Docker hands out to its networks: a
/// container behind a reverse proxy sees the proxy at the network gateway.
const DEFAULT_TRUSTED_PROXIES: &str = "127.0.0.0/8,::1/128,172.16.0.0/12";

fn trusted_proxies(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<ipnet::IpNet>, ConfigError> {
    const VAR: &str = "PAPERJAM_TRUSTED_PROXIES";
    let raw = get(VAR).unwrap_or_else(|| DEFAULT_TRUSTED_PROXIES.to_string());
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<ipnet::IpNet>()
                .or_else(|_| s.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                .map_err(|_| ConfigError::Invalid {
                    var: VAR,
                    reason: format!("{s} is not an address or CIDR range"),
                })
        })
        .collect()
}

fn client_ip_header(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<axum::http::HeaderName, ConfigError> {
    const VAR: &str = "PAPERJAM_CLIENT_IP_HEADER";
    let raw = get(VAR).unwrap_or_else(|| "x-forwarded-for".to_string());
    axum::http::HeaderName::try_from(raw.trim().to_ascii_lowercase()).map_err(|_| {
        ConfigError::Invalid {
            var: VAR,
            reason: format!("{raw} is not a header name"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn proxy_settings() {
        let c = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY)])).unwrap();
        assert_eq!(c.trusted_proxies.len(), 3);
        assert_eq!(c.client_ip_header, "x-forwarded-for");
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_TRUSTED_PROXIES", "10.0.0.1, 192.168.0.0/16"),
            ("PAPERJAM_CLIENT_IP_HEADER", "CF-Connecting-IP"),
        ]))
        .unwrap();
        assert_eq!(c.trusted_proxies[0].to_string(), "10.0.0.1/32");
        assert_eq!(c.client_ip_header, "cf-connecting-ip");
        for (var, value) in [
            ("PAPERJAM_TRUSTED_PROXIES", "not-a-range"),
            ("PAPERJAM_CLIENT_IP_HEADER", "bad header"),
        ] {
            let err = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY), (var, value)]))
                .err()
                .unwrap();
            assert!(matches!(err, ConfigError::Invalid { var: v, .. } if v == var));
        }
    }

    #[test]
    fn missing_secret_is_generated_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let vars = [("PAPERJAM_DATA_DIR", path.as_str())];
        let first = Config::from_lookup(lookup(&vars)).unwrap();
        let key = first.secret_key.expose_secret().to_string();
        assert!(key.len() >= MIN_SECRET_KEY_BYTES);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(SECRET_KEY_FILE)).unwrap(),
            key
        );
        let second = Config::from_lookup(lookup(&vars)).unwrap();
        assert_eq!(second.secret_key.expose_secret(), key);
    }

    #[test]
    fn the_variable_wins_over_the_secret_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SECRET_KEY_FILE), "f".repeat(64)).unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_DATA_DIR", path.as_str()),
            ("PAPERJAM_SECRET_KEY", KEY),
        ]))
        .unwrap();
        assert_eq!(c.secret_key.expose_secret(), KEY);
    }

    #[test]
    fn a_too_short_secret_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SECRET_KEY_FILE), "short").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let err = Config::from_lookup(lookup(&[("PAPERJAM_DATA_DIR", path.as_str())]))
            .err()
            .unwrap();
        assert!(matches!(err, ConfigError::SecretFile { .. }), "{err}");
    }

    #[test]
    fn bootstrap_admin_needs_both_variables() {
        let c = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY)])).unwrap();
        assert!(c.bootstrap_admin.is_none());
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_ADMIN_USER", " alice "),
            ("PAPERJAM_ADMIN_PASSWORD", "correct horse battery"),
        ]))
        .unwrap();
        let admin = c.bootstrap_admin.unwrap();
        assert_eq!(admin.username, "alice");
        for only in ["PAPERJAM_ADMIN_USER", "PAPERJAM_ADMIN_PASSWORD"] {
            let err = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY), (only, "x")]))
                .err()
                .unwrap();
            assert!(matches!(err, ConfigError::Missing(_)), "{err}");
        }
    }

    #[test]
    fn short_secret_is_error() {
        let err = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", "too-short")]))
            .err()
            .unwrap();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PAPERJAM_SECRET_KEY",
                ..
            }
        ));
    }

    #[test]
    fn defaults_applied() {
        let c = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY)])).unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/data"));
        assert_eq!(c.bind, "127.0.0.1:8140".parse::<SocketAddr>().unwrap());
        assert!(c.public_url.is_none());
        assert_eq!(c.secret_key.expose_secret(), KEY);
        assert_eq!(c.ocr_langs, "eng");
        assert_eq!(c.ocr_workers, 1);
        assert_eq!(c.llm_workers, 2);
        assert!(c.llm_api_key.is_none());
        assert_eq!(c.llm_base_url.as_str(), "https://openrouter.ai/api/v1");
        assert_eq!(c.llm_provider, LlmProvider::OpenRouter);
        assert!(c.llm_prices.is_empty());
        assert_eq!(c.llm_model, "anthropic/claude-haiku-5.5");
        assert_eq!(c.llm_vision_model, "anthropic/claude-haiku-5.5");
        assert_eq!(
            c.llm_handwriting_model.as_deref(),
            Some("anthropic/claude-sonnet-5.5")
        );
        assert_eq!(
            c.llm_allowed_models,
            vec!["anthropic/claude-haiku-5.5", "anthropic/claude-sonnet-5.5"]
        );
        assert!(c.is_model_allowed("anthropic/claude-haiku-5.5"));
        assert!(!c.is_model_allowed("openai/gpt-4o"));
        assert_eq!(c.llm_reasoning_effort, None);
        assert!(c.llm_create_tags);
        assert!(c.llm_monthly_budget_usd.is_none());
        assert_eq!(c.max_upload_bytes, 100 * 1024 * 1024);
        assert!(c.oauth_static_client.is_none());
        assert_eq!(c.extract_timeout, std::time::Duration::from_secs(120));
        assert_eq!(c.extract_memory_mb, 1536);
        assert!(c.extract_worker_program.is_none());
    }

    #[test]
    fn the_parsing_worker_limits_are_configurable_and_sane() {
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_EXTRACT_TIMEOUT_SECS", "30"),
            ("PAPERJAM_EXTRACT_MEMORY_MB", "0"),
        ]))
        .unwrap();
        assert_eq!(c.extract_timeout, std::time::Duration::from_secs(30));
        assert_eq!(c.extract_memory_mb, 0);

        let err = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_EXTRACT_TIMEOUT_SECS", "0"),
        ]))
        .err()
        .unwrap();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PAPERJAM_EXTRACT_TIMEOUT_SECS",
                ..
            }
        ));
    }

    #[test]
    fn the_handwriting_model_is_configurable_and_off_elsewhere() {
        let model = |vars: &[(&str, &str)]| {
            let mut all = vec![("PAPERJAM_SECRET_KEY", KEY)];
            all.extend_from_slice(vars);
            Config::from_lookup(lookup(&all))
                .unwrap()
                .llm_handwriting_model
        };
        assert_eq!(
            model(&[("PAPERJAM_LLM_HANDWRITING_MODEL", "model-hand")]).as_deref(),
            Some("model-hand")
        );
        assert_eq!(model(&[("PAPERJAM_LLM_HANDWRITING_MODEL", "off")]), None);
        // Another endpoint may not serve the OpenRouter default.
        assert_eq!(
            model(&[("PAPERJAM_LLM_BASE_URL", "http://localhost:11434/v1")]),
            None
        );
        // A second reading by the same model would change nothing.
        assert_eq!(
            model(&[
                ("PAPERJAM_LLM_VISION_MODEL", "model-v"),
                ("PAPERJAM_LLM_HANDWRITING_MODEL", "model-v"),
            ]),
            None
        );
    }

    #[test]
    fn llm_allowed_models_configuration() {
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_LLM_MODEL", "model-text"),
            ("PAPERJAM_LLM_VISION_MODEL", "model-vision"),
        ]))
        .unwrap();
        assert_eq!(
            c.llm_allowed_models,
            vec!["model-text", "model-vision", "anthropic/claude-sonnet-5.5"]
        );
        assert!(c.is_model_allowed("model-text"));
        assert!(c.is_model_allowed("model-vision"));
        assert!(!c.is_model_allowed("model-other"));

        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_LLM_ALLOWED_MODELS", "custom/a, custom/b "),
        ]))
        .unwrap();
        assert_eq!(c.llm_allowed_models, vec!["custom/a", "custom/b"]);
        assert!(c.is_model_allowed("custom/a"));
        assert!(c.is_model_allowed("custom/b"));
        assert!(!c.is_model_allowed("anthropic/claude-haiku-5.5"));
    }

    #[test]
    fn static_oauth_client_needs_all_three_variables() {
        let secret = "s".repeat(40);
        let full = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_OAUTH_STATIC_CLIENT_ID", "my-client"),
            ("PAPERJAM_OAUTH_STATIC_CLIENT_SECRET", &secret),
            (
                "PAPERJAM_OAUTH_STATIC_REDIRECT_URIS",
                "https://a.example/cb, https://b.example/cb",
            ),
        ]))
        .unwrap();
        let client = full.oauth_static_client.unwrap();
        assert_eq!(client.id, "my-client");
        assert_eq!(client.redirect_uris.len(), 2);

        let partial = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_OAUTH_STATIC_CLIENT_ID", "my-client"),
        ]));
        assert!(matches!(
            partial.err().unwrap(),
            ConfigError::Missing("PAPERJAM_OAUTH_STATIC_CLIENT_SECRET")
        ));
    }

    #[test]
    fn overrides_are_parsed() {
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_DATA_DIR", "/tmp/pj"),
            ("PAPERJAM_BIND", "0.0.0.0:9000"),
            ("PAPERJAM_PUBLIC_URL", "https://docs.example.org"),
            ("PAPERJAM_OPENROUTER_API_KEY", "test-api-key"),
            ("PAPERJAM_OPENROUTER_BASE_URL", "http://127.0.0.1:9/api/v1"),
            ("PAPERJAM_LLM_CREATE_TAGS", "false"),
            ("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "5.50"),
            ("PAPERJAM_OCR_WORKERS", "3"),
            ("PAPERJAM_MAX_UPLOAD_BYTES", "1024"),
        ]))
        .unwrap();
        assert_eq!(c.data_dir, PathBuf::from("/tmp/pj"));
        assert_eq!(c.bind.port(), 9000);
        assert_eq!(c.public_url.unwrap().as_str(), "https://docs.example.org/");
        assert_eq!(c.llm_api_key.unwrap().expose_secret(), "test-api-key");
        assert_eq!(c.llm_base_url.as_str(), "http://127.0.0.1:9/api/v1");
        assert!(!c.llm_create_tags);
        assert_eq!(c.llm_monthly_budget_usd.unwrap().to_string(), "5.50");
        assert_eq!(c.ocr_workers, 3);
        assert_eq!(c.max_upload_bytes, 1024);
    }

    fn llm_config(extra: &[(&str, &str)]) -> Config {
        let mut vars = vec![("PAPERJAM_SECRET_KEY", KEY)];
        vars.extend_from_slice(extra);
        Config::from_lookup(lookup(&vars)).unwrap()
    }

    #[test]
    fn the_neutral_llm_names_work_and_win_over_the_old_ones() {
        let c = llm_config(&[
            ("PAPERJAM_LLM_API_KEY", "new-key"),
            ("PAPERJAM_LLM_BASE_URL", "http://localhost:11434/v1"),
        ]);
        assert_eq!(c.llm_api_key.unwrap().expose_secret(), "new-key");
        assert_eq!(c.llm_base_url.as_str(), "http://localhost:11434/v1");

        let c = llm_config(&[
            ("PAPERJAM_LLM_API_KEY", "new-key"),
            ("PAPERJAM_OPENROUTER_API_KEY", "old-key"),
            ("PAPERJAM_LLM_BASE_URL", "http://localhost:1/v1"),
            ("PAPERJAM_OPENROUTER_BASE_URL", "http://localhost:2/v1"),
        ]);
        assert_eq!(c.llm_api_key.unwrap().expose_secret(), "new-key");
        assert_eq!(c.llm_base_url.as_str(), "http://localhost:1/v1");
    }

    #[test]
    fn the_old_openrouter_names_still_work() {
        let c = llm_config(&[
            ("PAPERJAM_OPENROUTER_API_KEY", "old-key"),
            (
                "PAPERJAM_OPENROUTER_BASE_URL",
                "https://openrouter.ai/api/v1",
            ),
        ]);
        assert_eq!(c.llm_api_key.unwrap().expose_secret(), "old-key");
        assert_eq!(c.llm_provider, LlmProvider::OpenRouter);
        // An empty new name counts as unset, so the old one still applies.
        let c = llm_config(&[
            ("PAPERJAM_LLM_API_KEY", ""),
            ("PAPERJAM_OPENROUTER_API_KEY", "old-key"),
        ]);
        assert_eq!(c.llm_api_key.unwrap().expose_secret(), "old-key");
    }

    #[test]
    fn the_provider_follows_the_host_unless_overridden() {
        let provider = |extra: &[(&str, &str)]| llm_config(extra).llm_provider;
        assert_eq!(provider(&[]), LlmProvider::OpenRouter);
        for url in [
            "https://api.openai.com/v1",
            "http://localhost:11434/v1",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://openrouter.ai.example.org/v1",
        ] {
            assert_eq!(
                provider(&[("PAPERJAM_LLM_BASE_URL", url)]),
                LlmProvider::Generic,
                "{url}"
            );
        }
        assert_eq!(
            provider(&[("PAPERJAM_LLM_BASE_URL", "https://OpenRouter.ai/api/v1")]),
            LlmProvider::OpenRouter
        );
        // A proxy or a test server in front of OpenRouter says so explicitly.
        let proxied = [
            ("PAPERJAM_LLM_BASE_URL", "http://127.0.0.1:9/v1"),
            ("PAPERJAM_LLM_PROVIDER", "openrouter"),
        ];
        assert_eq!(provider(&proxied), LlmProvider::OpenRouter);
        let forced = [("PAPERJAM_LLM_PROVIDER", "generic")];
        assert_eq!(provider(&forced), LlmProvider::Generic);
        let auto = [
            ("PAPERJAM_LLM_BASE_URL", "http://127.0.0.1:9/v1"),
            ("PAPERJAM_LLM_PROVIDER", "auto"),
        ];
        assert_eq!(provider(&auto), LlmProvider::Generic);
        let err = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_LLM_PROVIDER", "azure"),
        ]))
        .err()
        .unwrap();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PAPERJAM_LLM_PROVIDER",
                ..
            }
        ));
    }

    #[test]
    fn the_price_table_is_parsed() {
        let c = llm_config(&[(
            "PAPERJAM_LLM_PRICES",
            "gpt-4.1-mini=0.40:1.60, llama3.1:8b=0:0 ,models/gemini-2.5-flash=0.3:2.5",
        )]);
        assert_eq!(c.llm_prices.len(), 3);
        let price = c.llm_prices["gpt-4.1-mini"];
        assert_eq!(price.input_per_mtok.to_string(), "0.40");
        assert_eq!(price.output_per_mtok.to_string(), "1.60");
        assert_eq!(c.llm_prices["llama3.1:8b"].input_per_mtok, Decimal::ZERO);
        assert!(c.llm_prices.contains_key("models/gemini-2.5-flash"));
    }

    #[test]
    fn a_malformed_price_table_names_the_variable() {
        for bad in ["gpt", "gpt=1", "gpt=a:b", "gpt=-1:2", "=1:2", "gpt=1:2:3"] {
            let err = Config::from_lookup(lookup(&[
                ("PAPERJAM_SECRET_KEY", KEY),
                ("PAPERJAM_LLM_PRICES", bad),
            ]))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} should be rejected"));
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "PAPERJAM_LLM_PRICES",
                        ..
                    }
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn invalid_values_name_the_variable() {
        let err = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_BIND", "not-an-address"),
        ]))
        .err()
        .unwrap();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                var: "PAPERJAM_BIND",
                ..
            }
        ));
    }

    #[test]
    fn inbox_is_off_by_default_and_validated() {
        let c = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY)])).unwrap();
        assert!(c.inbox_dir.is_none());
        assert_eq!(c.inbox_interval_secs, 30);

        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_DATA_DIR", "/data"),
            ("PAPERJAM_INBOX_DIR", "/inbox"),
            ("PAPERJAM_INBOX_USER", " anna "),
            ("PAPERJAM_INBOX_INTERVAL_SECS", "5"),
        ]))
        .unwrap();
        assert_eq!(c.inbox_dir, Some(PathBuf::from("/inbox")));
        assert_eq!(c.inbox_user.as_deref(), Some("anna"));
        assert_eq!(c.inbox_interval_secs, 5);

        let bad = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("PAPERJAM_SECRET_KEY", KEY), ("PAPERJAM_DATA_DIR", "/data")];
            pairs.extend_from_slice(extra);
            Config::from_lookup(lookup(&pairs)).err().unwrap()
        };
        for secs in ["0", "abc", "86401"] {
            assert!(matches!(
                bad(&[
                    ("PAPERJAM_INBOX_DIR", "/inbox"),
                    ("PAPERJAM_INBOX_INTERVAL_SECS", secs)
                ]),
                ConfigError::Invalid {
                    var: "PAPERJAM_INBOX_INTERVAL_SECS",
                    ..
                }
            ));
        }
        for dir in ["/data", "/data/inbox", "/"] {
            assert!(matches!(
                bad(&[("PAPERJAM_INBOX_DIR", dir)]),
                ConfigError::Invalid {
                    var: "PAPERJAM_INBOX_DIR",
                    ..
                }
            ));
        }
        assert!(matches!(
            bad(&[("PAPERJAM_INBOX_USER", "anna")]),
            ConfigError::Missing("PAPERJAM_INBOX_DIR")
        ));
    }

    #[test]
    fn an_empty_inbox_user_counts_as_unset() {
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_INBOX_DIR", "/inbox"),
            ("PAPERJAM_INBOX_USER", "  "),
        ]))
        .unwrap();
        assert_eq!(c.inbox_user, None);
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_INBOX_USER", ""),
        ]))
        .unwrap();
        assert!(c.inbox_dir.is_none());
    }

    fn inbox_rejected(data: &std::path::Path, inbox: &std::path::Path) -> bool {
        matches!(
            Config::from_lookup(lookup(&[
                ("PAPERJAM_SECRET_KEY", KEY),
                ("PAPERJAM_DATA_DIR", data.to_str().unwrap()),
                ("PAPERJAM_INBOX_DIR", inbox.to_str().unwrap()),
            ])),
            Err(ConfigError::Invalid {
                var: "PAPERJAM_INBOX_DIR",
                ..
            })
        )
    }

    #[test]
    fn the_inbox_check_sees_through_dot_dot_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(data.join("sub")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();

        assert!(!inbox_rejected(&data, &elsewhere));
        let sneaky = elsewhere.join("..").join("data").join("sub");
        assert!(inbox_rejected(&data, &sneaky));
        let not_yet_created = data.join("new").join("..").join("inbox");
        assert!(inbox_rejected(&data, &not_yet_created));
        assert!(!inbox_rejected(&data, &data.join("..").join("elsewhere")));

        #[cfg(unix)]
        {
            let link = elsewhere.join("link");
            std::os::unix::fs::symlink(&data, &link).unwrap();
            assert!(inbox_rejected(&data, &link));
            assert!(inbox_rejected(&data, &link.join("sub")));
            assert!(inbox_rejected(&link, &data.join("sub")));
            let parent_link = root.path().join("parent_link");
            std::os::unix::fs::symlink(root.path(), &parent_link).unwrap();
            assert!(inbox_rejected(&data, &parent_link));
        }
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_LLM_API_KEY", "test-very-secret-key"),
            (
                "PAPERJAM_EMAIL_SECRET",
                "test-very-secret-email-webhook-key-0123",
            ),
        ]))
        .unwrap();
        let dbg = format!("{c:?}");
        assert!(!dbg.contains(KEY));
        assert!(!dbg.contains("test-very-secret-key"));
        assert!(!dbg.contains("test-very-secret-email-webhook-key-0123"));
    }

    #[test]
    fn email_intake_is_off_by_default_and_validated() {
        let off = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY)])).unwrap();
        assert!(off.email_intake.is_none());

        for var in [
            ("PAPERJAM_EMAIL_USER", "alice"),
            ("PAPERJAM_EMAIL_ALLOWED_SENDERS", "alice@example.org"),
            ("PAPERJAM_EMAIL_BODY_FALLBACK", "false"),
            ("PAPERJAM_EMAIL_LOCAL_ONLY", "true"),
            ("PAPERJAM_EMAIL_TAG", "email"),
        ] {
            let err = Config::from_lookup(lookup(&[("PAPERJAM_SECRET_KEY", KEY), var]))
                .err()
                .unwrap();
            assert!(
                matches!(err, ConfigError::Missing("PAPERJAM_EMAIL_SECRET")),
                "{var:?}: {err:?}"
            );
        }

        let short = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            ("PAPERJAM_EMAIL_SECRET", "too-short"),
        ]))
        .err()
        .unwrap();
        assert!(matches!(
            short,
            ConfigError::Invalid {
                var: "PAPERJAM_EMAIL_SECRET",
                ..
            }
        ));

        let c = Config::from_lookup(lookup(&[
            ("PAPERJAM_SECRET_KEY", KEY),
            (
                "PAPERJAM_EMAIL_SECRET",
                "  email-ingest-secret-at-least-32-bytes!  ",
            ),
            ("PAPERJAM_EMAIL_USER", " alice "),
            (
                "PAPERJAM_EMAIL_ALLOWED_SENDERS",
                " Alice@Example.org , @Partner.Example.com ",
            ),
            ("PAPERJAM_EMAIL_BODY_FALLBACK", "false"),
            ("PAPERJAM_EMAIL_LOCAL_ONLY", "true"),
            ("PAPERJAM_EMAIL_TAG", " inbox-mail "),
        ]))
        .unwrap();
        let email = c.email_intake.unwrap();
        assert_eq!(
            email.secret.expose_secret(),
            "email-ingest-secret-at-least-32-bytes!"
        );
        assert_eq!(email.user.as_deref(), Some("alice"));
        assert!(!email.body_fallback);
        assert!(email.local_only);
        assert_eq!(email.tag.as_deref(), Some("inbox-mail"));
        assert_eq!(
            email.allowed_senders,
            vec![
                SenderRule::Exact("alice@example.org".into()),
                SenderRule::Domain("@partner.example.com".into()),
            ]
        );
        assert!(email.allowed_senders[0].matches("ALICE@example.org"));
        assert!(!email.allowed_senders[0].matches("bob@example.org"));
        assert!(email.allowed_senders[1].matches("Billing@partner.example.com"));
        assert!(!email.allowed_senders[1].matches("@partner.example.com"));
        assert!(!email.allowed_senders[1].matches("x@notpartner.example.com"));
        assert!(!email.allowed_senders[1].matches("a@b@partner.example.com"));

        for bad in [
            "alice",
            "@localhost",
            "alice@localhost",
            "a@b@example.org",
            "alice @example.org",
            "@.example.org",
            "@example.org.",
            ",",
        ] {
            let err = Config::from_lookup(lookup(&[
                ("PAPERJAM_SECRET_KEY", KEY),
                (
                    "PAPERJAM_EMAIL_SECRET",
                    "email-ingest-secret-at-least-32-bytes!",
                ),
                ("PAPERJAM_EMAIL_ALLOWED_SENDERS", bad),
            ]))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} should be rejected"));
            assert!(
                matches!(
                    err,
                    ConfigError::Invalid {
                        var: "PAPERJAM_EMAIL_ALLOWED_SENDERS",
                        ..
                    }
                ),
                "{bad:?}: {err:?}"
            );
        }
    }
}
