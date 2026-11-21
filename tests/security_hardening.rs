//! Regression tests for the 2026-11 security audit: what an anonymous caller
//! may cost the server, and what an API token must not be able to do.

mod common;

use common::{Server, TestEnv, stderr, stdout};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

fn start() -> (TestEnv, Server) {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    let server = env.serve();
    (env, server)
}

fn browser() -> Client {
    Client::builder().cookie_store(true).build().unwrap()
}

async fn sign_in(server: &Server, client: &Client) {
    let response = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn login_status(server: &Server, username: &str, password: &str) -> StatusCode {
    Client::new()
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .unwrap()
        .status()
}

fn cli_token(env: &TestEnv, name: &str) -> String {
    let out = env.run(&["token", "create", "--user", "alice", name]);
    assert!(out.status.success(), "token create: {}", stderr(&out));
    stdout(&out).trim().to_string()
}

/// A tiny PDF padded with a comment to `size` bytes.
fn pdf_of(size: usize, tag: &str) -> Vec<u8> {
    let head = format!("%PDF-1.4\n% {tag}\n");
    let tail = "\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n";
    let pad = size.saturating_sub(head.len() + tail.len());
    format!("{head}{}{tail}", "x".repeat(pad)).into_bytes()
}

// --- 1. what an anonymous caller may cost ---------------------------------------------

fn is_early_close(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )
        {
            return true;
        }
        current = e.source();
    }
    false
}

#[tokio::test]
async fn a_login_body_over_the_default_limit_is_refused() {
    let (_env, server) = start();
    // A 5 MB username, as the audit probe sent (scaled down).
    let body = json!({"username": "a".repeat(5 * 1024 * 1024), "password": "x"}).to_string();
    let send_login = |payload: String| {
        Client::new()
            .post(server.url("/api/v1/auth/login"))
            .header("content-type", "application/json")
            .body(payload)
            .send()
    };
    let response = match send_login(body).await {
        Ok(resp) => resp,
        Err(err) if is_early_close(&err) => {
            // The server may close the socket after sending 413 before the
            // client finishes writing 5 MB; still verify the 413 JSON body.
            let smaller = json!({"username": "a".repeat(200 * 1024), "password": "x"}).to_string();
            send_login(smaller).await.unwrap()
        }
        Err(err) => panic!("unexpected login request error: {err}"),
    };
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["code"], "payload_too_large");

    // The server is still fine, and a normal sign-in goes through.
    assert_eq!(
        login_status(&server, "alice", PASSWORD).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn oversized_credentials_are_refused_before_the_limiter() {
    let (_env, server) = start();
    for _ in 0..15 {
        assert_eq!(
            login_status(&server, &"a".repeat(65), "wrong wrong wrong").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            login_status(&server, "alice", &"p".repeat(1025)).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    // Refusals are not failed sign-ins: the owner is not locked out by them.
    assert_eq!(
        login_status(&server, "alice", PASSWORD).await,
        StatusCode::OK
    );
    // The longest allowed name and password still reach the check (and lose).
    assert_eq!(
        login_status(&server, &"a".repeat(64), &"p".repeat(1024)).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn other_routes_keep_a_small_body_limit() {
    let (env, server) = start();
    let token = cli_token(&env, "agent");
    let big = json!({"name": "n".repeat(200 * 1024)}).to_string();
    // Signed in, a JSON endpoint still does not buffer a body this size.
    let response = Client::new()
        .post(server.url("/api/v1/tokens"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(big.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // Anonymous, on an open endpoint.
    let response = Client::new()
        .post(server.url("/api/v1/auth/passkey/challenge"))
        .header("content-type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn file_routes_and_mcp_keep_their_large_limit() {
    let (env, server) = start();
    let token = cli_token(&env, "agent");
    let http = browser();
    sign_in(&server, &http).await;

    // A document upload far above the small default.
    let form = Form::new().text("local_only", "true").part(
        "file",
        Part::bytes(pdf_of(300 * 1024, "upload"))
            .file_name("big.pdf")
            .mime_str("application/pdf")
            .unwrap(),
    );
    let response = http
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // So does the one-time upload link.
    let link: Value = Client::new()
        .post(server.url("/api/v1/upload-links"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let put = Client::new()
        .put(format!(
            "{}?filename=link.pdf&local_only=true",
            link["url"].as_str().unwrap()
        ))
        .body(pdf_of(300 * 1024, "link"))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::CREATED);

    // MCP takes base64 files, so it is not held to 64 KB either, but it has a ceiling of its own.
    let mcp = |body: Vec<u8>| {
        Client::new()
            .post(server.url("/mcp"))
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body)
            .send()
    };
    let medium = mcp(vec![b' '; 512 * 1024]).await.unwrap();
    assert_ne!(medium.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // The server refuses on the declared length and hangs up, so a client still
    // writing 17 MB may hit a broken pipe before it reads the answer. Declaring
    // the length and sending no body asks the same question without that race.
    let huge = declared_only(&server.url("/mcp"), &token, 17 * 1024 * 1024).await;
    assert!(
        huge.starts_with("HTTP/1.1 413"),
        "expected 413, got {huge:?}"
    );
}

/// Sends a POST whose headers declare `length` bytes of body but carries none,
/// and returns the status line of the answer.
async fn declared_only(url: &str, token: &str, length: usize) -> String {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: {host}\r\nauthorization: Bearer {token}\r\n\
         content-type: application/json\r\naccept: application/json, text/event-stream\r\n\
         content-length: {length}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    line
}

// --- 3. a token does not reach web-session powers ---------------------------------------

#[tokio::test]
async fn a_token_cannot_issue_tokens_or_create_users() {
    let (env, server) = start();
    let token = cli_token(&env, "agent");
    let http = Client::new();

    let minted = http
        .post(server.url("/api/v1/tokens"))
        .bearer_auth(&token)
        .json(&json!({"name": "spare"}))
        .send()
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::FORBIDDEN);
    let error: Value = minted.json().await.unwrap();
    assert_eq!(error["code"], "session_required");

    let user = http
        .post(server.url("/api/v1/users"))
        .bearer_auth(&token)
        .json(
            &json!({"username": "mallory", "password": "correct horse battery", "is_admin": true}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(user.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        login_status(&server, "mallory", "correct horse battery").await,
        StatusCode::UNAUTHORIZED
    );

    // The token still reads, and the owner's session does both.
    let stats = http
        .get(server.url("/api/v1/stats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(stats.status(), StatusCode::OK);

    let web = browser();
    sign_in(&server, &web).await;
    let issued = web
        .post(server.url("/api/v1/tokens"))
        .json(&json!({"name": "from the browser"}))
        .send()
        .await
        .unwrap();
    assert_eq!(issued.status(), StatusCode::CREATED);
    let created = web
        .post(server.url("/api/v1/users"))
        .json(&json!({"username": "bob", "password": "correct horse battery"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
}

#[test]
fn the_server_cli_still_creates_users_and_tokens() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(cli_token(&env, "ci").starts_with("pj_"));
}
