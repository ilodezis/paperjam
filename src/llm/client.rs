//! Chat completions client for schema-constrained JSON answers, for OpenRouter
//! and for any other OpenAI-compatible endpoint.
//!
//! The dialect is chosen by [`LlmProvider`]. On OpenRouter every request pins
//! the privacy-relevant routing flags: only endpoints that honour the JSON
//! schema (`require_parameters`) and that do not retain prompts
//! (`data_collection: "deny"`). Any other endpoint gets the standard OpenAI
//! fields only. `temperature` and `top_p` are never sent (current Anthropic
//! models reject them).
//!
//! Errors never carry model output or document text: messages name the
//! failure and, for schema mismatches, the JSON path, nothing else. A billed
//! but unusable answer (refusal, truncation, garbage) hands its usage back in
//! the error details so the caller can still account for the spend.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::{Number, Value, json};
use url::Url;

use crate::config::{Config, LlmProvider, ModelPrice};
use crate::error::{Result, ServiceError};

/// Retries after the first attempt (429, 5xx, timeouts, connection errors).
const MAX_RETRIES: u32 = 3;
const DEFAULT_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// A server-requested `Retry-After` is honoured up to this long.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Reasoning effort OpenRouter gets when the operator sets none.
const DEFAULT_REASONING_EFFORT: &str = "low";

static TRANSCRIPTION_VALIDATOR: LazyLock<std::result::Result<jsonschema::Validator, String>> =
    LazyLock::new(|| {
        jsonschema::validator_for(crate::llm::schemas::transcription_schema_ref())
            .map_err(|e| e.to_string())
    });

/// What one call cost: as the endpoint reports it, else from the configured
/// price table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: Decimal,
}

impl std::ops::Add for Usage {
    type Output = Usage;

    fn add(self, other: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cost_usd: self.cost_usd + other.cost_usd,
        }
    }
}

impl Usage {
    fn to_details(self) -> Value {
        json!({"usage": {
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cost_usd": self.cost_usd.to_string(),
        }})
    }

    /// The usage of a call that was billed but whose answer was unusable
    /// (`llm_refusal`, `llm_truncated`, `llm_invalid_json`,
    /// `llm_schema_mismatch`).
    pub fn from_error(err: &ServiceError) -> Option<Usage> {
        let usage = err.details.as_ref()?.get("usage")?;
        Some(Usage {
            input_tokens: u32::try_from(usage.get("input_tokens")?.as_u64()?).ok()?,
            output_tokens: u32::try_from(usage.get("output_tokens")?.as_u64()?).ok()?,
            cost_usd: usage.get("cost_usd")?.as_str()?.parse().ok()?,
        })
    }
}

/// One part of the user message.
pub enum Content {
    Text(String),
    /// An image with its media type, such as `image/webp` or `image/png`.
    Image {
        mime: &'static str,
        bytes: Vec<u8>,
    },
}

pub struct JsonRequest {
    pub model: String,
    pub system: String,
    pub user_content: Vec<Content>,
    pub schema_name: String,
    pub schema: Value,
    /// `None` leaves it to the endpoint (OpenRouter then gets `low`).
    pub reasoning_effort: Option<String>,
    pub max_tokens: u32,
}

pub struct LlmClient {
    endpoint: String,
    key: SecretString,
    http: reqwest::Client,
    backoff: Duration,
    provider: LlmProvider,
    prices: HashMap<String, ModelPrice>,
    /// Set after the first call whose cost could not be determined.
    cost_warned: AtomicBool,
    /// OpenRouter models known to reject `reasoning.effort = "none"` with 400.
    no_none_effort: Mutex<HashSet<String>>,
    /// Budget set aside for this client's calls in flight.
    reservations: crate::llm::budget::Reservations,
}

fn build_http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("the HTTP client can be built")
}

impl LlmClient {
    /// `base_url` is the API root (`https://openrouter.ai/api/v1`); its host
    /// picks the provider profile.
    pub fn new(base_url: &Url, key: SecretString) -> Self {
        Self {
            endpoint: format!(
                "{}/chat/completions",
                base_url.as_str().trim_end_matches('/')
            ),
            key,
            http: build_http(DEFAULT_TIMEOUT),
            backoff: DEFAULT_BACKOFF,
            provider: LlmProvider::from_base_url(base_url),
            prices: HashMap::new(),
            cost_warned: AtomicBool::new(false),
            no_none_effort: Mutex::new(HashSet::new()),
            reservations: crate::llm::budget::Reservations::default(),
        }
    }

    /// The budget held for calls still running; see [`crate::llm::budget::reserve`].
    pub fn reservations(&self) -> &crate::llm::budget::Reservations {
        &self.reservations
    }

    /// The client for the configured key; `None` when no key is set (the
    /// archive then runs on local recognition only).
    pub fn from_config(config: &Config) -> Option<Self> {
        let key = config.llm_api_key.clone()?;
        Some(
            Self::new(&config.llm_base_url, key)
                .with_provider(config.llm_provider)
                .with_prices(config.llm_prices.clone()),
        )
    }

    /// Overrides the profile derived from the base URL.
    pub fn with_provider(mut self, provider: LlmProvider) -> Self {
        self.provider = provider;
        self
    }

    /// Prices (USD per million tokens) for answers that carry no cost.
    pub fn with_prices(mut self, prices: HashMap<String, ModelPrice>) -> Self {
        self.prices = prices;
        self
    }

    /// First retry delay; doubles per retry, each jittered to 50-100 %.
    pub fn with_backoff(mut self, base: Duration) -> Self {
        self.backoff = base;
        self
    }

    pub fn provider(&self) -> LlmProvider {
        self.provider
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.http = build_http(timeout);
        self
    }

    /// Sends the request and returns the validated, parsed answer with what
    /// it cost. Failures are `Upstream` errors: `llm_unavailable` (retries
    /// exhausted), `llm_rejected` (4xx), `llm_bad_response` (not a
    /// chat-completions answer), and for a billed but unusable answer `llm_refusal`,
    /// `llm_truncated`, `llm_invalid_json` or `llm_schema_mismatch`.
    pub async fn chat_json<T: DeserializeOwned>(&self, mut req: JsonRequest) -> Result<(T, Usage)> {
        let bad_schema = || {
            ServiceError::internal(
                "llm_bad_schema",
                format!(
                    "the schema `{}` is not a valid JSON schema",
                    req.schema_name
                ),
            )
        };
        let owned_validator;
        let validator: &jsonschema::Validator = if req.schema_name
            == crate::llm::schemas::TRANSCRIPTION
            && &req.schema == crate::llm::schemas::transcription_schema_ref()
        {
            TRANSCRIPTION_VALIDATOR.as_ref().map_err(|_| bad_schema())?
        } else {
            owned_validator = jsonschema::validator_for(&req.schema).map_err(|_| bad_schema())?;
            &owned_validator
        };
        if self.provider == LlmProvider::OpenRouter
            && req.reasoning_effort.as_deref() == Some("none")
            && self
                .no_none_effort
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&req.model)
        {
            req.reasoning_effort = Some(DEFAULT_REASONING_EFFORT.to_string());
        }
        let mut body = request_body(&req, self.provider);
        let answer = match self.complete(&body).await {
            // Some OpenRouter endpoints (e.g. Sonnet/Opus 5.5) require
            // reasoning and reject `effort: "none"` with HTTP 400; fall back
            // once to the default effort rather than failing the document.
            Err(e)
                if self.provider == LlmProvider::OpenRouter
                    && req.reasoning_effort.as_deref() == Some("none")
                    && e.code == "llm_rejected"
                    && e.message.ends_with("(HTTP 400)") =>
            {
                self.no_none_effort
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(req.model.clone());
                tracing::info!(
                    model = %req.model,
                    "provider rejected reasoning effort `none`, retrying with `{DEFAULT_REASONING_EFFORT}`"
                );
                body["reasoning"]["effort"] = json!(DEFAULT_REASONING_EFFORT);
                self.complete(&body).await?
            }
            other => other?,
        };
        let usage = answer.usage;
        let fail = |code: &'static str, message: String| {
            ServiceError::upstream(code, message).with_details(usage.to_details())
        };

        if answer.finish_reason.as_deref() == Some("content_filter")
            || answer
                .refusal
                .as_deref()
                .is_some_and(|r| !r.trim().is_empty())
        {
            return Err(fail(
                "llm_refusal",
                "the model refused to answer".to_string(),
            ));
        }
        if answer.finish_reason.as_deref() == Some("length") {
            return Err(fail(
                "llm_truncated",
                "the answer was cut off by the token limit".to_string(),
            ));
        }
        let Some(content) = answer.content.filter(|c| !c.trim().is_empty()) else {
            return Err(fail(
                "llm_refusal",
                "the model returned an empty answer".to_string(),
            ));
        };
        let value: Value = serde_json::from_str(&content).map_err(|_| {
            fail(
                "llm_invalid_json",
                "the answer is not valid JSON".to_string(),
            )
        })?;
        if let Some(error) = validator.iter_errors(&value).next() {
            return Err(fail(
                "llm_schema_mismatch",
                format!(
                    "the answer does not match schema `{}` at `{}`",
                    req.schema_name,
                    error.instance_path()
                ),
            ));
        }
        let parsed = serde_json::from_value(value).map_err(|_| {
            fail(
                "llm_schema_mismatch",
                format!(
                    "the answer for schema `{}` cannot be read as the expected structure",
                    req.schema_name
                ),
            )
        })?;
        Ok((parsed, usage))
    }

    async fn complete(&self, body: &Value) -> Result<Answer> {
        let mut last = String::new();
        for attempt in 0..=MAX_RETRIES {
            match self.attempt(body).await {
                Attempt::Done(answer) => return Ok(answer),
                Attempt::Fail(error) => return Err(error),
                Attempt::Retry { reason, after } => {
                    tracing::warn!(attempt, reason = %reason, "llm request failed");
                    last = reason;
                    if attempt < MAX_RETRIES {
                        let wait = self.delay(attempt);
                        let wait = after.map_or(wait, |a| wait.max(a.min(MAX_RETRY_AFTER)));
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }
        Err(ServiceError::upstream(
            "llm_unavailable",
            format!(
                "{} did not answer after {} tries ({last})",
                self.provider.name(),
                MAX_RETRIES + 1
            ),
        ))
    }

    fn delay(&self, attempt: u32) -> Duration {
        self.backoff
            .saturating_mul(1 << attempt)
            .mul_f64(rand::random_range(0.5..1.0))
    }

    async fn attempt(&self, body: &Value) -> Attempt {
        let sent = self
            .http
            .post(&self.endpoint)
            .bearer_auth(self.key.expose_secret())
            .json(body)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(e) if e.is_builder() => {
                return Attempt::Fail(ServiceError::internal(
                    "llm_bad_request",
                    "the request could not be built",
                ));
            }
            Err(e) if e.is_timeout() => return Attempt::retry("timeout"),
            Err(_) => return Attempt::retry("network error"),
        };
        let status = response.status();
        let after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = match response.text().await {
            Ok(text) => text,
            Err(e) if e.is_timeout() => return Attempt::retry("timeout"),
            Err(_) => return Attempt::retry("network error"),
        };

        if is_retryable(status.as_u16()) {
            return Attempt::Retry {
                reason: format!("HTTP {}", status.as_u16()),
                after,
            };
        }
        if !status.is_success() {
            let name = self.provider.name();
            tracing::warn!(status = status.as_u16(), "{name} rejected the request");
            tracing::debug!(error = %provider_error(&text), "{name}'s reason");
            return Attempt::Fail(rejected(self.provider, status.as_u16()));
        }
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Attempt::Fail(bad_response(self.provider, "the body is not JSON"));
        };
        if value.get("choices").is_none() {
            // OpenRouter reports some upstream failures as a 200 with an
            // error envelope.
            let provider = self.provider;
            return match value["error"]["code"].as_u64() {
                Some(code) if is_retryable(code as u16) => Attempt::retry(format!("error {code}")),
                Some(code) => Attempt::Fail(rejected(provider, code as u16)),
                None => Attempt::Fail(bad_response(
                    provider,
                    "the body has neither choices nor an error",
                )),
            };
        }
        let choice = &value["choices"][0];
        if choice.is_null() {
            return Attempt::Fail(bad_response(self.provider, "the body has no choices"));
        }
        let finish_reason = choice["finish_reason"].as_str().map(str::to_string);
        if finish_reason.as_deref() == Some("error") {
            return Attempt::retry("the provider failed mid-answer");
        }
        let model = body["model"].as_str().unwrap_or_default();
        let (usage, cost_known) = parse_usage(&value["usage"], self.prices.get(model));
        if !cost_known && !self.cost_warned.swap(true, Ordering::Relaxed) {
            self.warn_unknown_cost();
        }
        Attempt::Done(Answer {
            content: message_text(&choice["message"]["content"]),
            refusal: choice["message"]["refusal"].as_str().map(str::to_string),
            finish_reason,
            usage,
        })
    }
}

impl LlmClient {
    /// One warning per client, not per call.
    fn warn_unknown_cost(&self) {
        match self.provider {
            LlmProvider::OpenRouter => {
                tracing::warn!("OpenRouter reported no cost for a call");
            }
            LlmProvider::Generic => tracing::warn!(
                "the LLM API reports no cost and PAPERJAM_LLM_PRICES has no entry for the \
                 model: its calls are recorded at zero cost, so PAPERJAM_LLM_MONTHLY_BUDGET_USD \
                 cannot protect this setup"
            ),
        }
    }
}

struct Answer {
    content: Option<String>,
    refusal: Option<String>,
    finish_reason: Option<String>,
    usage: Usage,
}

enum Attempt {
    Done(Answer),
    Retry {
        reason: String,
        after: Option<Duration>,
    },
    Fail(ServiceError),
}

impl Attempt {
    fn retry(reason: impl Into<String>) -> Self {
        Attempt::Retry {
            reason: reason.into(),
            after: None,
        }
    }
}

fn is_retryable(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

/// The provider's own error message and, when OpenRouter relays one, the
/// upstream's raw reason (together at most 300 characters), for the debug
/// log; empty when the body has none.
fn provider_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let error = &value["error"];
    let mut parts = Vec::new();
    if let Some(message) = error["message"].as_str() {
        parts.push(message.to_string());
    }
    match &error["metadata"]["raw"] {
        Value::String(raw) => parts.push(raw.clone()),
        Value::Null => {}
        other => parts.push(other.to_string()),
    }
    parts.join(": ").chars().take(300).collect()
}

fn rejected(provider: LlmProvider, status: u16) -> ServiceError {
    ServiceError::upstream(
        "llm_rejected",
        format!("{} rejected the request (HTTP {status})", provider.name()),
    )
}

fn bad_response(provider: LlmProvider, what: &str) -> ServiceError {
    ServiceError::upstream(
        "llm_bad_response",
        format!("{} sent an unusable answer: {what}", provider.name()),
    )
}

/// `message.content` is a string, or a list of text parts.
fn message_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Token counts and cost of one answer, and whether the cost is real. The
/// cost is the endpoint's own `usage.cost` (OpenRouter), else tokens times
/// the configured price of the model, else zero.
fn parse_usage(usage: &Value, price: Option<&ModelPrice>) -> (Usage, bool) {
    let tokens = |key: &str| {
        usage[key]
            .as_u64()
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let (input_tokens, output_tokens) = (tokens("prompt_tokens"), tokens("completion_tokens"));
    let reported = match &usage["cost"] {
        Value::Number(n) => decimal_from_number(n),
        _ => None,
    };
    let priced = price.map(|p| {
        (Decimal::from(input_tokens) * p.input_per_mtok
            + Decimal::from(output_tokens) * p.output_per_mtok)
            / Decimal::from(1_000_000)
    });
    let cost = reported.or(priced);
    let usage = Usage {
        input_tokens,
        output_tokens,
        cost_usd: cost.unwrap_or(Decimal::ZERO),
    };
    (usage, cost.is_some())
}

/// JSON numbers arrive as `0.0002` or, for small values, `2e-5`.
fn decimal_from_number(n: &Number) -> Option<Decimal> {
    let text = n.to_string();
    let value = text
        .parse::<Decimal>()
        .or_else(|_| Decimal::from_scientific(&text))
        .ok()?;
    Some(value.max(Decimal::ZERO))
}

fn request_body(req: &JsonRequest, provider: LlmProvider) -> Value {
    let parts: Vec<Value> = req
        .user_content
        .iter()
        .map(|part| match part {
            Content::Text(text) => json!({"type": "text", "text": text}),
            Content::Image { mime, bytes } => json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{}", STANDARD.encode(bytes))},
            }),
        })
        .collect();
    let system_message = if provider == LlmProvider::OpenRouter
        && req.model.starts_with("anthropic/")
        && req.system.len() >= 1024
    {
        json!({
            "role": "system",
            "content": [{
                "type": "text",
                "text": req.system,
                "cache_control": {"type": "ephemeral"},
            }],
        })
    } else {
        json!({"role": "system", "content": req.system})
    };
    let mut body = json!({
        "model": req.model,
        "messages": [
            system_message,
            {"role": "user", "content": parts},
        ],
        "max_tokens": req.max_tokens,
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": req.schema_name, "strict": true, "schema": req.schema},
        },
    });
    match provider {
        LlmProvider::OpenRouter => {
            let effort = req.reasoning_effort.as_deref();
            body["provider"] = json!({"require_parameters": true, "data_collection": "deny"});
            body["reasoning"] = json!({"effort": effort.unwrap_or(DEFAULT_REASONING_EFFORT)});
            // Deprecated by OpenRouter (usage is always returned), kept so
            // older gateways in front of it still report the cost.
            body["usage"] = json!({"include": true});
        }
        LlmProvider::Generic => {
            // Many models and servers reject the field, so it goes out only
            // when the operator asked for it.
            if let Some(effort) = &req.reasoning_effort {
                body["reasoning_effort"] = json!(effort);
            }
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_is_the_message_cut_short() {
        let body = r#"{"error":{"message":"schema is not supported","code":400}}"#;
        assert_eq!(provider_error(body), "schema is not supported");
        let long = format!(r#"{{"error":{{"message":"{}"}}}}"#, "ж".repeat(500));
        assert_eq!(provider_error(&long).chars().count(), 300);
        let relayed = r#"{"error":{"message":"Provider returned error","metadata":{"raw":"schema too complex"}}}"#;
        assert_eq!(
            provider_error(relayed),
            "Provider returned error: schema too complex"
        );
        assert_eq!(provider_error("<html>bad gateway</html>"), "");
        assert_eq!(provider_error(r#"{"error":"flat"}"#), "");
    }

    #[test]
    fn costs_parse_in_plain_and_scientific_notation() {
        let plain = Number::from_f64(0.0002).unwrap();
        assert_eq!(decimal_from_number(&plain).unwrap().to_string(), "0.0002");
        let tiny = Number::from_f64(0.00001).unwrap();
        assert_eq!(decimal_from_number(&tiny).unwrap().to_string(), "0.00001");
        let int = Number::from(3);
        assert_eq!(decimal_from_number(&int).unwrap().to_string(), "3");
    }

    #[test]
    fn usage_survives_a_roundtrip_through_error_details() {
        let usage = Usage {
            input_tokens: 12,
            output_tokens: 34,
            cost_usd: "0.0021".parse().unwrap(),
        };
        let err = ServiceError::upstream("llm_truncated", "x").with_details(usage.to_details());
        assert_eq!(Usage::from_error(&err), Some(usage));
        assert_eq!(
            Usage::from_error(&ServiceError::upstream("llm_unavailable", "x")),
            None
        );
    }

    #[test]
    fn content_parts_are_joined() {
        let parts = json!([{"type": "text", "text": "{\"a\":"}, {"type": "text", "text": "1}"}]);
        assert_eq!(message_text(&parts).as_deref(), Some("{\"a\":1}"));
        assert_eq!(message_text(&Value::Null), None);
    }

    fn price(input: &str, output: &str) -> ModelPrice {
        ModelPrice {
            input_per_mtok: input.parse().unwrap(),
            output_per_mtok: output.parse().unwrap(),
        }
    }

    #[test]
    fn usage_without_cost_counts_as_free_and_is_flagged() {
        let raw = json!({"prompt_tokens": 5, "completion_tokens": 2});
        let (usage, known) = parse_usage(&raw, None);
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cost_usd, Decimal::ZERO);
        assert!(!known);
        let (usage, known) = parse_usage(&Value::Null, None);
        assert_eq!((usage.input_tokens, usage.output_tokens), (0, 0));
        assert!(!known);
    }

    #[test]
    fn a_reported_cost_beats_the_price_table() {
        let raw = json!({"prompt_tokens": 1000, "completion_tokens": 200, "cost": 0.0002});
        let (usage, known) = parse_usage(&raw, Some(&price("100", "100")));
        assert_eq!(usage.cost_usd.to_string(), "0.0002");
        assert!(known);
    }

    #[test]
    fn without_a_reported_cost_the_price_table_prices_the_tokens() {
        let raw = json!({"prompt_tokens": 2000, "completion_tokens": 500});
        let (usage, known) = parse_usage(&raw, Some(&price("0.40", "1.60")));
        // 2000 * 0.40 / 1e6 + 500 * 1.60 / 1e6
        assert_eq!(usage.cost_usd, "0.0016".parse::<Decimal>().unwrap());
        assert!(known);
        let (usage, known) = parse_usage(&raw, Some(&price("0", "0")));
        assert_eq!(usage.cost_usd, Decimal::ZERO);
        assert!(known, "a free local model is a known cost");
    }

    fn effort_request(effort: Option<&str>) -> JsonRequest {
        JsonRequest {
            model: "m".to_string(),
            system: "s".to_string(),
            user_content: vec![Content::Text("t".to_string())],
            schema_name: "n".to_string(),
            schema: json!({"type": "object"}),
            reasoning_effort: effort.map(str::to_string),
            max_tokens: 64,
        }
    }

    #[test]
    fn the_openrouter_body_keeps_its_routing_fields() {
        let body = request_body(&effort_request(None), LlmProvider::OpenRouter);
        assert_eq!(body["provider"]["require_parameters"], true);
        assert_eq!(body["provider"]["data_collection"], "deny");
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["usage"]["include"], true);
        assert!(body.get("reasoning_effort").is_none());
        let body = request_body(&effort_request(Some("high")), LlmProvider::OpenRouter);
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn the_generic_body_has_only_standard_fields() {
        let body = request_body(&effort_request(None), LlmProvider::Generic);
        let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["max_tokens", "messages", "model", "response_format"]);
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let body = request_body(&effort_request(Some("medium")), LlmProvider::Generic);
        assert_eq!(body["reasoning_effort"], "medium");
        for banned in ["provider", "usage", "reasoning"] {
            assert!(body.get(banned).is_none(), "{banned} is OpenRouter-only");
        }
    }

    #[test]
    fn messages_name_the_provider_in_use() {
        let generic = rejected(LlmProvider::Generic, 401);
        assert_eq!(
            generic.message,
            "The LLM API rejected the request (HTTP 401)"
        );
        let router = bad_response(LlmProvider::OpenRouter, "the body is not JSON");
        assert!(router.message.starts_with("OpenRouter sent"));
    }

    #[test]
    fn debug_output_never_shows_the_key() {
        let client = LlmClient::new(
            &Url::parse("http://localhost/api/v1").unwrap(),
            SecretString::from("sk-very-secret"),
        );
        assert!(!format!("{:?}", client.key).contains("sk-very-secret"));
    }

    #[test]
    fn the_client_picks_its_profile_from_the_host() {
        let key = || SecretString::from("k");
        let router = LlmClient::new(&Url::parse("https://openrouter.ai/api/v1").unwrap(), key());
        assert_eq!(router.provider, LlmProvider::OpenRouter);
        let other = LlmClient::new(&Url::parse("http://localhost:11434/v1").unwrap(), key());
        assert_eq!(other.provider, LlmProvider::Generic);
        assert_eq!(
            other.with_provider(LlmProvider::OpenRouter).provider,
            LlmProvider::OpenRouter
        );
    }
}
