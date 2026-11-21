//! A local stand-in for OpenRouter's chat completions endpoint.
//!
//! Tests script its answers and read back what the client sent; no real key
//! and no network access outside loopback is involved.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// One scripted answer.
#[derive(Clone)]
pub struct MockReply {
    status: u16,
    body: String,
    delay: Duration,
}

impl MockReply {
    fn completion(message: Value, finish_reason: &str, usage: Value) -> Self {
        let body = json!({
            "id": "gen-mock",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "finish_reason": finish_reason,
                "message": message,
            }],
            "usage": usage,
        });
        Self {
            status: 200,
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }

    fn usage(prompt: u32, completion: u32, cost: f64) -> Value {
        json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
            "cost": cost,
        })
    }

    /// A well-formed answer carrying `content` as the model's JSON.
    pub fn ok(content: &Value) -> Self {
        Self::ok_with_usage(content, 1000, 200, 0.0002)
    }

    pub fn ok_with_usage(content: &Value, prompt: u32, completion: u32, cost: f64) -> Self {
        Self::completion(
            json!({"role": "assistant", "content": content.to_string()}),
            "stop",
            Self::usage(prompt, completion, cost),
        )
    }

    /// The model's text verbatim (not necessarily JSON).
    pub fn raw(content: &str) -> Self {
        Self::completion(
            json!({"role": "assistant", "content": content}),
            "stop",
            Self::usage(1000, 50, 0.0001),
        )
    }

    pub fn refusal(text: &str) -> Self {
        Self::completion(
            json!({"role": "assistant", "content": null, "refusal": text}),
            "stop",
            Self::usage(1000, 10, 0.0001),
        )
    }

    pub fn empty() -> Self {
        Self::completion(
            json!({"role": "assistant", "content": ""}),
            "stop",
            Self::usage(1000, 0, 0.0001),
        )
    }

    /// Cut off by the token limit in the middle of the JSON.
    pub fn truncated(partial: &str) -> Self {
        Self::completion(
            json!({"role": "assistant", "content": partial}),
            "length",
            Self::usage(1000, 4096, 0.0021),
        )
    }

    /// A bare HTTP status with an error body.
    pub fn status(code: u16) -> Self {
        Self {
            status: code,
            body: json!({"error": {"code": code, "message": "mock failure"}}).to_string(),
            delay: Duration::ZERO,
        }
    }

    /// HTTP 200 whose body is an OpenRouter error envelope.
    pub fn error_envelope(code: u16) -> Self {
        Self {
            status: 200,
            body: json!({"error": {"code": code, "message": "mock failure"}}).to_string(),
            delay: Duration::ZERO,
        }
    }

    /// The same answer as an endpoint that reports tokens but no cost, which
    /// is what every provider other than OpenRouter does.
    pub fn without_cost(mut self) -> Self {
        let mut body: Value = serde_json::from_str(&self.body).expect("a JSON body");
        if let Some(usage) = body.get_mut("usage").and_then(Value::as_object_mut) {
            usage.remove("cost");
        }
        self.body = body.to_string();
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// What the mock saw in one request.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub body: Value,
    pub authorization: Option<String>,
}

impl RecordedRequest {
    /// The `json_schema.name` the request asked for (identifies the step).
    pub fn schema_name(&self) -> Option<&str> {
        self.body["response_format"]["json_schema"]["name"].as_str()
    }

    /// Number of image parts in the user message.
    pub fn image_count(&self) -> usize {
        self.body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter(|part| part["type"] == "image_url")
            .count()
    }
}

#[derive(Default)]
struct MockState {
    queue: Mutex<VecDeque<MockReply>>,
    routes: Mutex<HashMap<String, MockReply>>,
    model_routes: Mutex<HashMap<String, MockReply>>,
    fallback: Mutex<Option<MockReply>>,
    requests: Mutex<Vec<RecordedRequest>>,
    active_requests: AtomicUsize,
    max_concurrent_requests: AtomicUsize,
}

/// Answers come from, in order: a per-`json_schema.name` route, the queue
/// (one entry per request), the fallback; with none of them the mock answers
/// 500. Every request is recorded.
pub struct MockOpenRouter {
    /// Base URL in the shape of `PAPERJAM_OPENROUTER_BASE_URL`.
    pub base_url: String,
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}

impl MockOpenRouter {
    pub async fn start() -> Self {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;

        let state = Arc::new(MockState::default());
        let app = axum::Router::new()
            .route(
                "/api/v1/chat/completions",
                axum::routing::post(
                    |State(state): State<Arc<MockState>>,
                     headers: HeaderMap,
                     axum::Json(body): axum::Json<Value>| async move {
                        let authorization = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let model = body["model"].as_str().map(str::to_string);
                        let name = body["response_format"]["json_schema"]["name"]
                            .as_str()
                            .map(str::to_string);
                        state.requests.lock().unwrap().push(RecordedRequest {
                            body,
                            authorization,
                        });
                        let by_model =
                            model.and_then(|m| state.model_routes.lock().unwrap().get(&m).cloned());
                        let routed = by_model.or_else(|| {
                            name.and_then(|n| state.routes.lock().unwrap().get(&n).cloned())
                        });
                        let queued = state.queue.lock().unwrap().pop_front();
                        let fallback = state.fallback.lock().unwrap().clone();
                        let reply = routed
                            .or(queued)
                            .or(fallback)
                            .unwrap_or_else(|| MockReply::status(500));
                        struct InFlight<'a>(&'a AtomicUsize);
                        impl Drop for InFlight<'_> {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, Ordering::AcqRel);
                            }
                        }
                        let cur = state.active_requests.fetch_add(1, Ordering::AcqRel) + 1;
                        state
                            .max_concurrent_requests
                            .fetch_max(cur, Ordering::AcqRel);
                        let _guard = InFlight(&state.active_requests);
                        if !reply.delay.is_zero() {
                            tokio::time::sleep(reply.delay).await;
                        }
                        (
                            StatusCode::from_u16(reply.status).unwrap(),
                            [("content-type", "application/json")],
                            reply.body,
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock");
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/api/v1"),
            state,
            task,
        }
    }

    /// Answers the next request that no route claims.
    pub fn enqueue(&self, reply: MockReply) {
        self.state.queue.lock().unwrap().push_back(reply);
    }

    /// Answers every request for this schema name (until replaced).
    pub fn route(&self, schema_name: &str, reply: MockReply) {
        self.state
            .routes
            .lock()
            .unwrap()
            .insert(schema_name.to_string(), reply);
    }

    /// Answers every request targeting this model name (takes precedence over schema routes).
    pub fn route_model(&self, model: &str, reply: MockReply) {
        self.state
            .model_routes
            .lock()
            .unwrap()
            .insert(model.to_string(), reply);
    }

    pub fn max_concurrent_requests(&self) -> usize {
        self.state.max_concurrent_requests.load(Ordering::Acquire)
    }

    /// Answers whatever nothing else claims, without being consumed.
    pub fn set_fallback(&self, reply: MockReply) {
        *self.state.fallback.lock().unwrap() = Some(reply);
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }
}

impl Drop for MockOpenRouter {
    fn drop(&mut self) {
        self.task.abort();
    }
}
