//! The OpenRouter client against the mock server: request shape, retries,
//! error classification and usage/budget accounting.

mod common;

use std::time::Duration;

use common::{MockOpenRouter, MockReply, TestEnv};
use paperjam::config::{LlmProvider, ModelPrice};
use paperjam::error::ErrorKind;
use paperjam::ids::DocumentId;
use paperjam::llm::{Content, JsonRequest, LlmClient, Usage, budget};
use paperjam::service::{Actor, stats};
use rust_decimal::Decimal;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

#[derive(Debug, Deserialize, PartialEq)]
struct Answer {
    title: String,
    count: u32,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "count": {"type": "integer"}
        },
        "required": ["title", "count"],
        "additionalProperties": false
    })
}

fn request() -> JsonRequest {
    JsonRequest {
        model: "anthropic/claude-haiku-5.5".to_string(),
        system: "You extract data.".to_string(),
        user_content: vec![Content::Text("Some document text.".to_string())],
        schema_name: "answer".to_string(),
        schema: schema(),
        reasoning_effort: Some("low".to_string()),
        max_tokens: 2048,
    }
}

fn client(mock: &MockOpenRouter) -> LlmClient {
    LlmClient::new(
        &Url::parse(&mock.base_url).unwrap(),
        SecretString::from("test-key"),
    )
    .with_provider(LlmProvider::OpenRouter)
    .with_backoff(Duration::from_millis(5))
}

/// The same mock, treated as any other OpenAI-compatible endpoint.
fn generic_client(mock: &MockOpenRouter) -> LlmClient {
    client(mock).with_provider(LlmProvider::Generic)
}

fn good() -> Value {
    json!({"title": "Lease", "count": 3})
}

#[tokio::test]
async fn request_has_required_provider_flags_and_no_temperature() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok(&good()));
    let (answer, _usage) = client(&mock).chat_json::<Answer>(request()).await.unwrap();
    assert_eq!(
        answer,
        Answer {
            title: "Lease".to_string(),
            count: 3
        }
    );

    let sent = mock.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].authorization.as_deref(), Some("Bearer test-key"));
    let body = &sent[0].body;
    assert_eq!(body["model"], "anthropic/claude-haiku-5.5");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["name"], "answer");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(body["response_format"]["json_schema"]["schema"], schema());
    assert_eq!(body["provider"]["require_parameters"], true);
    assert_eq!(body["provider"]["data_collection"], "deny");
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["usage"]["include"], true);
    assert_eq!(body["max_tokens"], 2048);
    let object = body.as_object().unwrap();
    for banned in ["temperature", "top_p"] {
        assert!(!object.contains_key(banned), "{banned} must not be sent");
    }
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
}

#[tokio::test]
async fn generic_request_has_standard_fields_only() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok(&good()));
    let mut req = request();
    req.reasoning_effort = None;
    generic_client(&mock)
        .chat_json::<Answer>(req)
        .await
        .unwrap();

    let body = &mock.requests()[0].body;
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["max_tokens", "messages", "model", "response_format"]);
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);

    mock.enqueue(MockReply::ok(&good()));
    generic_client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap();
    let body = &mock.requests()[1].body;
    assert_eq!(body["reasoning_effort"], "low");
    for banned in ["provider", "reasoning", "usage"] {
        assert!(body.get(banned).is_none(), "{banned} is OpenRouter-only");
    }
}

#[tokio::test]
async fn generic_cost_comes_from_the_endpoint_or_the_price_table() {
    let mock = MockOpenRouter::start().await;
    let prices = || {
        let price = ModelPrice {
            input_per_mtok: "0.40".parse().unwrap(),
            output_per_mtok: "1.60".parse().unwrap(),
        };
        [("anthropic/claude-haiku-5.5".to_string(), price)].into()
    };

    // The endpoint's own figure wins.
    mock.enqueue(MockReply::ok_with_usage(&good(), 1000, 200, 0.5));
    let (_, usage) = generic_client(&mock)
        .with_prices(prices())
        .chat_json::<Answer>(request())
        .await
        .unwrap();
    assert_eq!(usage.cost_usd.to_string(), "0.5");

    // Without one the tokens are priced: 2000 * 0.40 / 1e6 + 500 * 1.60 / 1e6.
    mock.enqueue(MockReply::ok_with_usage(&good(), 2000, 500, 0.0).without_cost());
    let (_, usage) = generic_client(&mock)
        .with_prices(prices())
        .chat_json::<Answer>(request())
        .await
        .unwrap();
    assert_eq!(usage.cost_usd, "0.0016".parse::<Decimal>().unwrap());

    // No cost and no price: tokens are still counted, the cost is zero.
    mock.enqueue(MockReply::ok_with_usage(&good(), 2000, 500, 0.0).without_cost());
    let (_, usage) = generic_client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (2000, 500));
    assert_eq!(usage.cost_usd, Decimal::ZERO);
}

#[tokio::test]
async fn generic_errors_do_not_name_openrouter() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(401));
    let err = generic_client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.message, "The LLM API rejected the request (HTTP 401)");

    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(401));
    let err = client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.message, "OpenRouter rejected the request (HTTP 401)");
}

#[tokio::test]
async fn images_are_sent_as_png_data_urls() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok(&good()));
    let mut req = request();
    req.user_content.push(Content::Image {
        mime: "image/png",
        bytes: vec![0x89, b'P', b'N', b'G'],
    });
    client(&mock).chat_json::<Answer>(req).await.unwrap();

    let sent = mock.requests();
    assert_eq!(sent[0].image_count(), 1);
    let url = sent[0].body["messages"][1]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
}

#[tokio::test]
async fn retries_then_succeeds_on_503() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(503));
    mock.enqueue(MockReply::status(429));
    mock.enqueue(MockReply::ok(&good()));
    let (answer, _) = client(&mock).chat_json::<Answer>(request()).await.unwrap();
    assert_eq!(answer.count, 3);
    assert_eq!(mock.request_count(), 3);
}

#[tokio::test]
async fn error_envelope_in_a_200_is_retried_like_the_status() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::error_envelope(502));
    mock.enqueue(MockReply::ok(&good()));
    client(&mock).chat_json::<Answer>(request()).await.unwrap();
    assert_eq!(mock.request_count(), 2);
}

#[tokio::test]
async fn a_5xx_storm_gives_up_after_the_retries() {
    let mock = MockOpenRouter::start().await;
    mock.set_fallback(MockReply::status(503));
    let err = client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Upstream);
    assert_eq!(err.code, "llm_unavailable");
    // One try plus three retries.
    assert_eq!(mock.request_count(), 4);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let mock = MockOpenRouter::start().await;
    mock.set_fallback(MockReply::status(401));
    let err = client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Upstream);
    assert_eq!(err.code, "llm_rejected");
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn openrouter_none_effort_falls_back_to_low_on_http_400() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(400));
    mock.enqueue(MockReply::ok(&good()));
    let mut req = request();
    req.reasoning_effort = Some("none".to_string());
    let (answer, _) = client(&mock).chat_json::<Answer>(req).await.unwrap();
    assert_eq!(answer.title, "Lease");
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body["reasoning"]["effort"], "none");
    assert_eq!(requests[1].body["reasoning"]["effort"], "low");

    // Non-400 client errors (such as 401 invalid key) still do not retry.
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(401));
    let mut req = request();
    req.reasoning_effort = Some("none".to_string());
    let err = client(&mock).chat_json::<Answer>(req).await.unwrap_err();
    assert_eq!(err.code, "llm_rejected");
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn a_slow_answer_times_out_and_is_retried() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok(&good()).delayed(Duration::from_millis(600)));
    mock.enqueue(MockReply::ok(&good()));
    let client = client(&mock).with_timeout(Duration::from_millis(150));
    client.chat_json::<Answer>(request()).await.unwrap();
    assert_eq!(mock.request_count(), 2);
}

#[tokio::test]
async fn truncated_response_is_upstream_error() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::truncated(r#"{"title": "Lea"#));
    let err = client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Upstream);
    assert_eq!(err.code, "llm_truncated");
    // The tokens were billed even though the answer is useless.
    let usage = Usage::from_error(&err).expect("usage travels with the error");
    assert_eq!(usage.output_tokens, 4096);
    assert_eq!(usage.cost_usd.to_string(), "0.0021");
}

#[tokio::test]
async fn refusals_and_empty_answers_are_llm_refusal() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::refusal("I can't help with that."));
    mock.enqueue(MockReply::empty());
    for _ in 0..2 {
        let err = client(&mock)
            .chat_json::<Answer>(request())
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Upstream);
        assert_eq!(err.code, "llm_refusal");
    }
}

#[tokio::test]
async fn invalid_json_is_upstream_error() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::raw("Sure! Here is the JSON you asked for: {"));
    let err = client(&mock)
        .chat_json::<Answer>(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Upstream);
    assert_eq!(err.code, "llm_invalid_json");
    assert!(
        !err.message.contains("Sure!"),
        "the model's text must not leak into the error"
    );
}

#[tokio::test]
async fn schema_mismatch_is_upstream_error() {
    let mock = MockOpenRouter::start().await;
    // Missing property, wrong type, extra property.
    mock.enqueue(MockReply::ok(&json!({"title": "Lease"})));
    mock.enqueue(MockReply::ok(&json!({"title": "Lease", "count": "three"})));
    mock.enqueue(MockReply::ok(
        &json!({"title": "Lease", "count": 3, "extra": true}),
    ));
    for _ in 0..3 {
        let err = client(&mock)
            .chat_json::<Answer>(request())
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Upstream);
        assert_eq!(err.code, "llm_schema_mismatch");
    }
}

#[tokio::test]
async fn enum_violations_are_schema_mismatches() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok(&json!({"kind": "invented"})));
    let mut req = request();
    req.schema = json!({
        "type": "object",
        "properties": {"kind": {"type": "string", "enum": ["a", "b"]}},
        "required": ["kind"],
        "additionalProperties": false
    });
    let err = client(&mock).chat_json::<Value>(req).await.unwrap_err();
    assert_eq!(err.code, "llm_schema_mismatch");
    assert!(
        !err.message.contains("invented"),
        "answer values must not leak into the error"
    );
}

#[tokio::test]
async fn usage_recorded_with_cost() {
    let env = TestEnv::new();
    let app = env.app().await;
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::ok_with_usage(&good(), 1500, 300, 0.000045));
    let (_, usage) = client(&mock).chat_json::<Answer>(request()).await.unwrap();
    assert_eq!(usage.input_tokens, 1500);
    assert_eq!(usage.output_tokens, 300);
    assert_eq!(usage.cost_usd.to_string(), "0.000045");

    let doc = insert_document(&app).await;
    let mut tx = app.db.write.begin().await.unwrap();
    budget::record(
        &mut tx,
        doc,
        "anthropic/claude-haiku-5.5",
        "classify",
        &usage,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let row: (String, String, i64, i64, String) =
        sqlx::query_as("SELECT model, step, input_tokens, output_tokens, cost_usd FROM llm_usage")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "anthropic/claude-haiku-5.5".to_string(),
            "classify".to_string(),
            1500,
            300,
            "0.000045".to_string()
        )
    );
    let stats = stats::get_stats(&app.ctx(Actor::system())).await.unwrap();
    assert_eq!(stats.llm_month_usd, "0.000045");
}

#[tokio::test]
async fn budget_exhausted_returns_zero() {
    let env = TestEnv::new();
    let app = paperjam::service::App::open(
        env.config_with(&[("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "0.01")]),
    )
    .await
    .unwrap();
    assert_eq!(
        budget::remaining(&app).await.unwrap(),
        Some("0.01".parse::<Decimal>().unwrap())
    );

    let doc = insert_document(&app).await;
    spend(&app, doc, "0.004").await;
    assert_eq!(
        budget::remaining(&app).await.unwrap(),
        Some("0.006".parse::<Decimal>().unwrap())
    );

    spend(&app, doc, "0.02").await;
    assert_eq!(budget::remaining(&app).await.unwrap(), Some(Decimal::ZERO));
}

#[tokio::test]
async fn without_a_budget_nothing_is_limited() {
    let env = TestEnv::new();
    let app = env.app().await;
    let doc = insert_document(&app).await;
    spend(&app, doc, "1000").await;
    assert_eq!(budget::remaining(&app).await.unwrap(), None);
}

async fn spend(app: &paperjam::service::App, doc: DocumentId, cost: &str) {
    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        cost_usd: cost.parse().unwrap(),
    };
    let mut tx = app.db.write.begin().await.unwrap();
    budget::record(&mut tx, doc, "m", "classify", &usage)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn insert_document(app: &paperjam::service::App) -> DocumentId {
    let id = DocumentId::new();
    let now = paperjam::time::now_ts();
    sqlx::query("INSERT INTO documents (id, created_at, updated_at) VALUES (?, ?, ?)")
        .bind(id)
        .bind(&now)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn openrouter_none_effort_rejection_is_cached_per_model() {
    let mock = MockOpenRouter::start().await;
    mock.enqueue(MockReply::status(400));
    mock.enqueue(MockReply::ok(&good()));
    mock.enqueue(MockReply::ok(&good()));
    mock.enqueue(MockReply::ok(&good()));

    let llm = client(&mock);

    let mut req1 = request();
    req1.reasoning_effort = Some("none".to_string());
    llm.chat_json::<Answer>(req1).await.unwrap();

    // Second request on the same model goes straight to `low` without another 400.
    let mut req2 = request();
    req2.reasoning_effort = Some("none".to_string());
    llm.chat_json::<Answer>(req2).await.unwrap();

    // Another model still probes `none` on its own first call.
    let mut req3 = request();
    req3.model = "openai/gpt-5-mini".to_string();
    req3.reasoning_effort = Some("none".to_string());
    llm.chat_json::<Answer>(req3).await.unwrap();

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].body["reasoning"]["effort"], "none");
    assert_eq!(requests[1].body["reasoning"]["effort"], "low");
    assert_eq!(requests[2].body["reasoning"]["effort"], "low");
    assert_eq!(requests[3].body["reasoning"]["effort"], "none");
}

#[tokio::test]
async fn openrouter_anthropic_prompt_cache_marks_large_system_messages() {
    let mock = MockOpenRouter::start().await;
    for _ in 0..4 {
        mock.enqueue(MockReply::ok(&good()));
    }

    let large_system = "x".repeat(1024);

    // 1. OpenRouter + anthropic/* + >= 1024 bytes => ephemeral cache block.
    let mut cached_req = request();
    cached_req.system = large_system.clone();
    client(&mock).chat_json::<Answer>(cached_req).await.unwrap();

    // 2. Short system prompt => plain string.
    client(&mock).chat_json::<Answer>(request()).await.unwrap();

    // 3. Non-anthropic model on OpenRouter => plain string.
    let mut other_model = request();
    other_model.model = "google/gemini-2.5-flash".to_string();
    other_model.system = large_system.clone();
    client(&mock)
        .chat_json::<Answer>(other_model)
        .await
        .unwrap();

    // 4. Generic provider with anthropic model => plain string.
    let mut generic_req = request();
    generic_req.system = large_system;
    generic_client(&mock)
        .chat_json::<Answer>(generic_req)
        .await
        .unwrap();

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    let cached_content = &requests[0].body["messages"][0]["content"];
    assert!(cached_content.is_array(), "{cached_content}");
    assert_eq!(
        cached_content[0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(requests[1].body["messages"][0]["content"].is_string());
    assert!(requests[2].body["messages"][0]["content"].is_string());
    assert!(requests[3].body["messages"][0]["content"].is_string());
}
