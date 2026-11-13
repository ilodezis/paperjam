//! `paperjam mcp-stdio`: lets clients that only speak MCP over stdio use a
//! running server. It forwards JSON-RPC messages between stdin/stdout and the
//! server's `/mcp` endpoint and keeps no state of its own: no database, no files.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use url::Url;

/// Used for requests sent before the server named a version.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A tool call may legitimately take a while (a big upload, a search).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// JSON-RPC error codes used for failures of the proxy itself.
const PARSE_ERROR: i64 = -32700;
const UNAUTHORIZED: i64 = -32001;
const SERVER_ERROR: i64 = -32000;

/// The `/mcp` endpoint for a server base URL (`https://host` or `https://host/mcp`).
pub fn endpoint(base: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(base).with_context(|| format!("--url {base:?} is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("--url must start with http:// or https://");
    }
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/mcp") {
        url.set_path(&path);
    } else {
        url.set_path(&format!("{path}/mcp"));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// `paperjam mcp-stdio --url <base> --token <pj_…>`
pub async fn run(url: String, token: Option<String>) -> anyhow::Result<()> {
    let token = token
        .or_else(|| std::env::var("PAPERJAM_TOKEN").ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .context("an API token is required: pass --token or set PAPERJAM_TOKEN")?;
    let endpoint = endpoint(&url)?;
    proxy(
        endpoint,
        token,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

/// Forwards every line of `input` to the server and writes what comes back to
/// `output`, one JSON message per line. Returns when `input` ends and every
/// request in flight has been answered.
pub async fn proxy<R, W>(
    endpoint: Url,
    token: String,
    input: R,
    mut output: W,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("paperjam-mcp-stdio/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let upstream = Arc::new(Upstream {
        client,
        endpoint,
        token,
        protocol_version: Mutex::new(DEFAULT_PROTOCOL_VERSION.to_string()),
    });

    // One task owns stdout so replies from concurrent requests never interleave.
    let (replies, mut queue) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            let mut line = message.to_string();
            line.push('\n');
            if output.write_all(line.as_bytes()).await.is_err() || output.flush().await.is_err() {
                break;
            }
        }
    });

    let mut lines = input.lines();
    let mut in_flight = JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let upstream = Arc::clone(&upstream);
        let replies = replies.clone();
        in_flight.spawn(async move { upstream.forward(&line, &replies).await });
    }
    while in_flight.join_next().await.is_some() {}
    drop(replies);
    let _ = writer.await;
    Ok(())
}

struct Upstream {
    client: Client,
    endpoint: Url,
    token: String,
    protocol_version: Mutex<String>,
}

fn error_reply(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

impl Upstream {
    fn protocol_version(&self) -> String {
        self.protocol_version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Remembers the version the server picked, to name it on later requests.
    fn note_version(&self, reply: &Value) {
        if let Some(version) = reply["result"]["protocolVersion"].as_str() {
            *self
                .protocol_version
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = version.to_string();
        }
    }

    /// Sends one client message and queues whatever the server answers.
    async fn forward(&self, line: &str, replies: &mpsc::UnboundedSender<Value>) {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(_) => {
                let _ = replies.send(error_reply(Value::Null, PARSE_ERROR, "parse error"));
                return;
            }
        };
        // A message without an id is a notification: nobody waits for an answer.
        let id = message.get("id").cloned();
        let is_initialize = message["method"] == "initialize";

        let mut request = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(line.to_string());
        if !is_initialize {
            request = request.header("MCP-Protocol-Version", self.protocol_version());
        }

        let fail = |code: i64, text: String| match &id {
            Some(id) => {
                let _ = replies.send(error_reply(id.clone(), code, text));
            }
            None => eprintln!("paperjam mcp-stdio: {text}"),
        };

        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                fail(
                    SERVER_ERROR,
                    format!("the paperjam server did not answer: {e}"),
                );
                return;
            }
        };
        let status = response.status();
        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = match response.text().await {
            Ok(body) => body,
            Err(e) => {
                fail(
                    SERVER_ERROR,
                    format!("the paperjam server cut the reply short: {e}"),
                );
                return;
            }
        };

        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or_default();
            let code = if status == StatusCode::UNAUTHORIZED {
                UNAUTHORIZED
            } else {
                SERVER_ERROR
            };
            fail(code, format!("HTTP {status}: {detail}"));
            return;
        }
        // 202 Accepted for notifications and client responses: nothing to relay.
        if body.trim().is_empty() {
            return;
        }
        let payloads: Vec<String> = if is_sse { sse_data(&body) } else { vec![body] };
        for payload in payloads {
            match serde_json::from_str::<Value>(&payload) {
                // A JSON-RPC batch is relayed as separate messages.
                Ok(Value::Array(batch)) => {
                    for item in batch {
                        self.note_version(&item);
                        let _ = replies.send(item);
                    }
                }
                Ok(reply) => {
                    self.note_version(&reply);
                    let _ = replies.send(reply);
                }
                Err(_) => fail(
                    SERVER_ERROR,
                    "the paperjam server sent a reply that is not JSON".to_string(),
                ),
            }
        }
    }
}

/// The `data:` payload of every event in a Server-Sent Events body.
fn sse_data(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            if !current.is_empty() {
                events.push(current.join("\n"));
                current.clear();
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            current.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if !current.is_empty() {
        events.push(current.join("\n"));
    }
    events.retain(|e| !e.trim().is_empty());
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_adds_mcp_once() {
        for base in [
            "https://docs.example.org",
            "https://docs.example.org/",
            "https://docs.example.org/mcp",
            "https://docs.example.org/mcp/",
        ] {
            assert_eq!(
                endpoint(base).unwrap().as_str(),
                "https://docs.example.org/mcp",
                "{base}"
            );
        }
        assert_eq!(
            endpoint("http://127.0.0.1:8140?x=1").unwrap().as_str(),
            "http://127.0.0.1:8140/mcp"
        );
        assert_eq!(
            endpoint("https://example.org/archive").unwrap().as_str(),
            "https://example.org/archive/mcp"
        );
        assert!(endpoint("ftp://example.org").is_err());
        assert!(endpoint("not a url").is_err());
    }

    #[test]
    fn sse_events_are_split_on_blank_lines() {
        let body = "event: message\ndata: {\"a\":1}\n\ndata: {\"b\":\ndata: 2}\n\n: keep-alive\n\ndata:\n\n";
        assert_eq!(sse_data(body), vec!["{\"a\":1}", "{\"b\":\n2}"]);
    }
}
