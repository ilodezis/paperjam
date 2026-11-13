//! The OAuth 2.1 authorization server, end to end against a running server.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{Server, TestEnv, stderr};
use paperjam::service::{Actor, Via};
use reqwest::{Client, StatusCode, redirect};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

const PASSWORD: &str = "correct horse battery";
const REDIRECT: &str = "https://client.example/callback";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A browser: keeps cookies, never follows redirects (the test reads them).
fn browser() -> Client {
    Client::builder()
        .cookie_store(true)
        .redirect(redirect::Policy::none())
        .build()
        .unwrap()
}

fn start_with(vars: &[(&str, &str)]) -> (TestEnv, Server) {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    let server = env.serve_with(vars);
    (env, server)
}

fn start() -> (TestEnv, Server) {
    start_with(&[])
}

async fn sign_in(browser: &Client, server: &Server) {
    let response = browser
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn register(server: &Server, redirect_uri: &str) -> String {
    let response = Client::new()
        .post(server.url("/oauth/register"))
        .json(&json!({
            "client_name": "Test client",
            "redirect_uris": [redirect_uri],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    body["client_id"].as_str().unwrap().to_string()
}

fn authorize_url(server: &Server, client_id: &str, redirect_uri: &str, method: &str) -> String {
    let mut url = Url::parse(&server.url("/oauth/authorize")).unwrap();
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", &challenge_of(VERIFIER))
        .append_pair("code_challenge_method", method)
        .append_pair("state", "xyz");
    url.to_string()
}

/// The value of the hidden input `name` on the consent page.
fn hidden(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("no {name} in {html}"))
        + marker.len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].replace("&amp;", "&")
}

fn location(response: &reqwest::Response) -> Url {
    assert!(
        response.status().is_redirection(),
        "expected a redirect, got {}",
        response.status()
    );
    // Relative redirects (to the login page) are resolved against a dummy base.
    Url::parse("http://paperjam.test/")
        .unwrap()
        .join(response.headers()["location"].to_str().unwrap())
        .unwrap()
}

fn param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// Opens the consent page and presses the button; returns the redirect back to the client.
async fn decide(
    browser: &Client,
    server: &Server,
    client_id: &str,
    redirect_uri: &str,
    decision: &str,
) -> Url {
    let page = browser
        .get(authorize_url(server, client_id, redirect_uri, "S256"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.unwrap();
    let response = browser
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("decision", decision),
            ("client_id", &hidden(&html, "client_id")),
            ("redirect_uri", &hidden(&html, "redirect_uri")),
            ("code_challenge", &hidden(&html, "code_challenge")),
            ("state", &hidden(&html, "state")),
            ("csrf", &hidden(&html, "csrf")),
        ])
        .send()
        .await
        .unwrap();
    location(&response)
}

async fn token_request(server: &Server, form: &[(&str, &str)]) -> (StatusCode, Value) {
    let response = Client::new()
        .post(server.url("/oauth/token"))
        .form(form)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

async fn exchange(server: &Server, client_id: &str, code: &str) -> (StatusCode, Value) {
    token_request(
        server,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
        ],
    )
    .await
}

/// Registers a client, approves it as alice and trades the code for tokens.
async fn connect(server: &Server) -> (String, Value) {
    let client_id = register(server, REDIRECT).await;
    let browser = browser();
    sign_in(&browser, server).await;
    let back = decide(&browser, server, &client_id, REDIRECT, "approve").await;
    let code = param(&back, "code").expect("a code");
    let (status, tokens) = exchange(server, &client_id, &code).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    (client_id, tokens)
}

async fn mcp_status(server: &Server, access_token: &str) -> StatusCode {
    Client::new()
        .post(server.url("/mcp"))
        .bearer_auth(access_token)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "get_stats", "arguments": {}},
        }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn metadata_documents_describe_the_server() {
    let (_env, server) = start();
    let http = Client::new();
    let meta: Value = http
        .get(server.url("/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["issuer"], server.base.trim_end_matches('/'));
    assert_eq!(meta["code_challenge_methods_supported"], json!(["S256"]));
    for endpoint in [
        "authorization_endpoint",
        "token_endpoint",
        "registration_endpoint",
    ] {
        assert!(meta[endpoint].as_str().unwrap().starts_with(&server.base));
    }
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let resource: Value = http
            .get(server.url(path))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resource["resource"], format!("{}/mcp", server.base));
        assert_eq!(resource["authorization_servers"][0], meta["issuer"]);
    }
    // The 401 of /mcp points clients at that document.
    let denied = http.post(server.url("/mcp")).send().await.unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let challenge = denied.headers()["www-authenticate"].to_str().unwrap();
    assert!(
        challenge.contains("/.well-known/oauth-protected-resource"),
        "{challenge}"
    );
}

#[tokio::test]
async fn full_pkce_flow_reaches_mcp() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    let browser = browser();

    // Not signed in: sent to the login page, not shown a consent page.
    let anonymous = browser
        .get(authorize_url(&server, &client_id, REDIRECT, "S256"))
        .send()
        .await
        .unwrap();
    let login = location(&anonymous);
    assert_eq!(login.path(), "/login");
    assert!(
        param(&login, "next")
            .unwrap()
            .starts_with("/oauth/authorize?")
    );

    sign_in(&browser, &server).await;
    let page = browser
        .get(authorize_url(&server, &client_id, REDIRECT, "S256"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let csp = page.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("font-src 'self'"), "the page's own fonts load");
    assert!(!csp.contains("script-src"), "and still no scripts");
    let html = page.text().await.unwrap();
    assert!(html.contains("Test client"), "the client's name is shown");
    assert!(
        html.contains("client.example"),
        "the redirect host is shown"
    );
    assert!(
        html.contains(REDIRECT),
        "and the whole address, not only the host"
    );
    assert!(
        html.contains("registered itself on"),
        "a self-registered client says so"
    );
    assert!(html.contains("alice"), "the user it acts as is shown");

    let back = decide(&browser, &server, &client_id, REDIRECT, "approve").await;
    assert_eq!(back.as_str().split('?').next().unwrap(), REDIRECT);
    assert_eq!(param(&back, "state").as_deref(), Some("xyz"));
    let code = param(&back, "code").expect("a code");

    let (status, tokens) = exchange(&server, &client_id, &code).await;
    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(mcp_status(&server, access).await, StatusCode::OK);

    // The code is single-use, and replaying it ends the grant.
    let (status, error) = exchange(&server, &client_id, &code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_grant");
    assert_eq!(mcp_status(&server, access).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_rotates_and_replay_ends_the_grant() {
    let (_env, server) = start();
    let (client_id, tokens) = connect(&server).await;
    let first = tokens["refresh_token"].as_str().unwrap().to_string();

    let (status, second) = token_request(
        &server,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
            ("refresh_token", &first),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let new_access = second["access_token"].as_str().unwrap();
    assert_ne!(second["refresh_token"], tokens["refresh_token"]);
    assert_eq!(mcp_status(&server, new_access).await, StatusCode::OK);

    // The old refresh token is spent; showing it again kills the whole grant.
    let (status, error) = token_request(
        &server,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
            ("refresh_token", &first),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_grant");
    assert_eq!(
        mcp_status(&server, new_access).await,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = token_request(
        &server,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
            ("refresh_token", second["refresh_token"].as_str().unwrap()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn plain_pkce_rejected() {
    let (_env, server) = start();
    // A client somebody approved is told the protocol's way.
    let (client_id, _) = connect(&server).await;
    let browser = browser();
    sign_in(&browser, &server).await;

    for method in ["plain", ""] {
        let response = browser
            .get(authorize_url(&server, &client_id, REDIRECT, method))
            .send()
            .await
            .unwrap();
        let back = location(&response);
        assert_eq!(
            param(&back, "error").as_deref(),
            Some("invalid_request"),
            "{method}"
        );
        assert_eq!(param(&back, "state").as_deref(), Some("xyz"));
        assert!(param(&back, "code").is_none());
    }
    // No challenge at all is no better.
    let mut url = Url::parse(&authorize_url(&server, &client_id, REDIRECT, "S256")).unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != "code_challenge")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    let response = browser.get(url).send().await.unwrap();
    assert_eq!(
        param(&location(&response), "error").as_deref(),
        Some("invalid_request")
    );
}

#[tokio::test]
async fn request_errors_of_an_unapproved_client_stay_on_a_page_here() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    // The error comes before sign-in, so the browser need not be signed in.
    let anonymous = browser();

    let mut urls = Vec::new();
    for method in ["plain", ""] {
        urls.push(authorize_url(&server, &client_id, REDIRECT, method));
    }
    let mut wrong_type = Url::parse(&authorize_url(&server, &client_id, REDIRECT, "S256")).unwrap();
    let pairs: Vec<(String, String)> = wrong_type
        .query_pairs()
        .map(|(k, v)| {
            let v = if k == "response_type" {
                "token".into()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    wrong_type.query_pairs_mut().clear().extend_pairs(pairs);
    urls.push(wrong_type.to_string());

    for url in urls {
        let response = anonymous.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        assert!(response.headers().get("location").is_none(), "{url}");
        let html = response.text().await.unwrap();
        assert!(
            !html.contains("client.example"),
            "no link to the client: {html}"
        );
    }
}

#[tokio::test]
async fn unapproved_client_cannot_get_token() {
    let (env, server) = start();
    let client_id = register(&server, REDIRECT).await;

    // Nobody approved it, so there is no code to trade...
    let (status, body) = exchange(&server, &client_id, "made-up-code").await;
    assert!(status.is_client_error(), "{body}");
    assert!(body.get("access_token").is_none());

    // ...and even a code smuggled into the database does not help.
    let app = env.app().await;
    let (user_id,): (String,) = sqlx::query_as("SELECT id FROM users")
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query(
        "INSERT INTO oauth_grants (id, client_id, user_id, created_at) VALUES ('g1', ?, ?, ?)",
    )
    .bind(&client_id)
    .bind(&user_id)
    .bind(now)
    .execute(&app.db.write)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_codes (code_hash, grant_id, redirect_uri, code_challenge, expires_at) \
         VALUES (?, 'g1', ?, ?, '2999-01-01T00:00:00.000Z')",
    )
    .bind(paperjam::hashing::sha256_hex(b"smuggled"))
    .bind(REDIRECT)
    .bind(challenge_of(VERIFIER))
    .execute(&app.db.write)
    .await
    .unwrap();
    let (status, body) = exchange(&server, &client_id, "smuggled").await;
    assert!(status.is_client_error(), "{body}");
    assert!(body.get("access_token").is_none());

    // Denying keeps it unapproved.
    let browser = browser();
    sign_in(&browser, &server).await;
    let back = decide(&browser, &server, &client_id, REDIRECT, "deny").await;
    assert_eq!(param(&back, "error").as_deref(), Some("access_denied"));
    assert!(param(&back, "code").is_none());
    let (approved_by,): (Option<String>,) =
        sqlx::query_as("SELECT approved_by FROM oauth_clients WHERE id = ?")
            .bind(&client_id)
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert!(approved_by.is_none());
}

#[tokio::test]
async fn revoked_grant_kills_access_and_refresh() {
    let (env, server) = start();
    let (client_id, tokens) = connect(&server).await;
    let access = tokens["access_token"].as_str().unwrap().to_string();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(mcp_status(&server, &access).await, StatusCode::OK);

    let app = env.app().await;
    let users = paperjam::service::users::list_users(&app.ctx(Actor::cli()))
        .await
        .unwrap();
    let ctx = app.ctx(Actor::user(users[0].id, Via::Web));
    let grants = paperjam::oauth::list_grants(&ctx).await.unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].client_name, "Test client");
    paperjam::oauth::revoke_grant(&ctx, &grants[0].id)
        .await
        .unwrap();
    assert!(paperjam::oauth::list_grants(&ctx).await.unwrap().is_empty());

    assert_eq!(mcp_status(&server, &access).await, StatusCode::UNAUTHORIZED);
    let (status, error) = token_request(
        &server,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
            ("refresh_token", &refresh),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_grant");
}

#[tokio::test]
async fn client_can_revoke_its_own_token() {
    let (_env, server) = start();
    let (client_id, tokens) = connect(&server).await;
    let access = tokens["access_token"].as_str().unwrap();
    let response = Client::new()
        .post(server.url("/oauth/revoke"))
        .form(&[
            ("client_id", client_id.as_str()),
            ("token", tokens["refresh_token"].as_str().unwrap()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(mcp_status(&server, access).await, StatusCode::UNAUTHORIZED);
    // Unknown tokens get the same answer: nothing to learn from it.
    let response = Client::new()
        .post(server.url("/oauth/revoke"))
        .form(&[("client_id", client_id.as_str()), ("token", "nonsense")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn dcr_rate_limited() {
    let (_env, server) = start();
    let http = Client::new();
    let mut statuses = Vec::new();
    for _ in 0..12 {
        let response = http
            .post(server.url("/oauth/register"))
            .json(&json!({"client_name": "x", "redirect_uris": [REDIRECT]}))
            .send()
            .await
            .unwrap();
        statuses.push(response.status());
    }
    assert!(
        statuses[..10].iter().all(|s| *s == StatusCode::CREATED),
        "{statuses:?}"
    );
    assert!(
        statuses[10..]
            .iter()
            .all(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn registration_is_bounded_in_size() {
    let (_env, server) = start();
    let http = Client::new();
    let register = |body: String| {
        http.post(server.url("/oauth/register"))
            .header("content-type", "application/json")
            .body(body)
            .send()
    };

    // The audit probe: one huge redirect URI. Refused before it is stored.
    let huge = json!({"redirect_uris": [format!("https://client.example/{}", "a".repeat(8 * 1024 * 1024))]});
    // The server may hang up while the body is still being sent.
    if let Ok(response) = register(huge.to_string()).await {
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    // Just over the body limit, with a short URI: still too large.
    let padded = json!({"redirect_uris": [REDIRECT], "client_name": "n".repeat(20 * 1024)});
    // The server may hang up while the body is still being sent.
    if let Ok(response) = register(padded.to_string()).await {
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    // A URI that fits the body but not the 2048-character cap.
    let long = json!({"redirect_uris": [format!("https://client.example/{}", "a".repeat(3000))]});
    let response = register(long.to_string()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["error"], "invalid_redirect_uri");

    // The cap itself is allowed.
    let room = 2048 - "https://client.example/".len();
    let edge = json!({"redirect_uris": [format!("https://client.example/{}", "a".repeat(room))]});
    let response = register(edge.to_string()).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn only_known_app_schemes_may_be_registered() {
    let (_env, server) = start();
    let http = Client::new();
    for uri in [
        "myapp://callback",
        "intent://cb#Intent;end",
        "ftp://client.example/cb",
    ] {
        let response = http
            .post(server.url("/oauth/register"))
            .json(&json!({"redirect_uris": [uri]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn registration_turns_secret_auth_methods_into_public_clients() {
    let (_env, server) = start();
    let http = Client::new();
    for method in ["client_secret_post", "client_secret_basic"] {
        let response = http
            .post(server.url("/oauth/register"))
            .json(&json!({
                "redirect_uris": [REDIRECT],
                "token_endpoint_auth_method": method,
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{method}");
        let reply: serde_json::Value = response.json().await.unwrap();
        assert_eq!(reply["token_endpoint_auth_method"], "none", "{method}");
        assert!(reply.get("client_secret").is_none(), "{method}");
    }
}

#[tokio::test]
async fn registration_rejects_unsafe_metadata() {
    let (_env, server) = start();
    let http = Client::new();
    for body in [
        json!({"redirect_uris": []}),
        json!({"client_name": "x"}),
        json!({"redirect_uris": ["http://evil.example/cb"]}),
        json!({"redirect_uris": ["javascript:alert(1)"]}),
        json!({"redirect_uris": ["https://ok.example/cb#frag"]}),
        json!({"redirect_uris": ["relative/path"]}),
        json!({"redirect_uris": [REDIRECT], "token_endpoint_auth_method": "private_key_jwt"}),
        json!({"redirect_uris": [REDIRECT], "grant_types": ["password"]}),
    ] {
        let response = http
            .post(server.url("/oauth/register"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    // The limit counts attempts, so the accepted shapes go in one registration.
    let response = http
        .post(server.url("/oauth/register"))
        .json(&json!({"redirect_uris": [
            "http://localhost:3118/callback",
            "http://127.0.0.1:9/cb",
            "cursor://anysphere.cursor-retrieval/oauth/callback",
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn redirect_uri_must_match_exactly() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    let browser = browser();
    sign_in(&browser, &server).await;

    for wrong in [
        "https://client.example/callback/",
        "https://client.example/callback?x=1",
        "https://client.example/other",
        "https://client.example:8443/callback",
        "http://client.example/callback",
        "https://evil.example/callback",
    ] {
        let response = browser
            .get(authorize_url(&server, &client_id, wrong, "S256"))
            .send()
            .await
            .unwrap();
        // An error page, never a redirect to the unregistered address.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{wrong}");
        assert!(response.headers().get("location").is_none(), "{wrong}");
    }

    // And the token endpoint holds the code to the URI it was issued for.
    let back = decide(&browser, &server, &client_id, REDIRECT, "approve").await;
    let code = param(&back, "code").unwrap();
    let (status, error) = token_request(
        &server,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("redirect_uri", "https://client.example/callback/"),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_grant");
}

#[tokio::test]
async fn wrong_verifier_is_refused() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    let browser = browser();
    sign_in(&browser, &server).await;
    let back = decide(&browser, &server, &client_id, REDIRECT, "approve").await;
    let code = param(&back, "code").unwrap();
    let (status, error) = token_request(
        &server,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", &"a".repeat(64)),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_grant");
}

#[tokio::test]
async fn consent_cannot_be_forged_or_replayed_across_users() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    let browser = browser();
    sign_in(&browser, &server).await;

    // A POST without the token the consent page issued grants nothing.
    let response = browser
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("decision", "approve"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT),
            ("code_challenge", challenge_of(VERIFIER).as_str()),
            ("state", "xyz"),
            ("csrf", "9999999999.00"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get("location").is_none());

    // A token for other parameters does not carry over.
    let page = browser
        .get(authorize_url(&server, &client_id, REDIRECT, "S256"))
        .send()
        .await
        .unwrap();
    let html = page.text().await.unwrap();
    let response = browser
        .post(server.url("/oauth/authorize"))
        .form(&[
            ("decision", "approve"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT),
            (
                "code_challenge",
                challenge_of("another-verifier-another-verifier-another-verifier").as_str(),
            ),
            ("state", "xyz"),
            ("csrf", hidden(&html, "csrf").as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn consent_page_escapes_the_client_name() {
    let (_env, server) = start();
    let response = Client::new()
        .post(server.url("/oauth/register"))
        .json(&json!({"client_name": "<script>alert(1)</script>{{fields}}", "redirect_uris": [REDIRECT]}))
        .send()
        .await
        .unwrap();
    let client_id = response.json::<Value>().await.unwrap()["client_id"]
        .as_str()
        .unwrap()
        .to_string();
    let browser = browser();
    sign_in(&browser, &server).await;
    let html = browser
        .get(authorize_url(&server, &client_id, REDIRECT, "S256"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
}

async fn consent_html(browser: &Client, server: &Server, client_id: &str) -> String {
    let response = browser
        .get(authorize_url(server, client_id, REDIRECT, "S256"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.text().await.unwrap()
}

/// The consent page says plainly that the application can also publish share links and start
/// reprocessing (which may reach the LLM), in the signed-in person's language.
#[tokio::test]
async fn consent_page_names_sharing_and_reprocessing() {
    let (_env, server) = start();
    let client_id = register(&server, REDIRECT).await;
    let browser = browser();
    sign_in(&browser, &server).await;

    let html = consent_html(&browser, &server, &client_id).await;
    assert!(
        html.contains("Create public share links to documents"),
        "{html}"
    );
    assert!(html.contains("Start reprocessing"), "{html}");
    assert!(
        html.contains("configured LLM unless it is local-only"),
        "{html}"
    );

    let changed = browser
        .patch(server.url("/api/v1/auth/session"))
        .json(&json!({"locale": "ru"}))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let html = consent_html(&browser, &server, &client_id).await;
    assert!(
        html.contains("Создавать публичные ссылки на документы"),
        "{html}"
    );
    assert!(html.contains("Запускать переобработку"), "{html}");
    assert!(html.contains("«только локально»"), "{html}");
}

#[tokio::test]
async fn static_client_needs_its_secret_and_no_approval() {
    let secret = "static-client-secret-0123456789-abcdefghij";
    let (_env, server) = start_with(&[
        ("PAPERJAM_OAUTH_STATIC_CLIENT_ID", "my-connector"),
        ("PAPERJAM_OAUTH_STATIC_CLIENT_SECRET", secret),
        ("PAPERJAM_OAUTH_STATIC_REDIRECT_URIS", REDIRECT),
    ]);
    let browser = browser();
    sign_in(&browser, &server).await;
    let back = decide(&browser, &server, "my-connector", REDIRECT, "approve").await;
    let code = param(&back, "code").unwrap();

    let form = |secret: Option<&str>| {
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("client_id", "my-connector".to_string()),
            ("code", code.clone()),
            ("redirect_uri", REDIRECT.to_string()),
            ("code_verifier", VERIFIER.to_string()),
        ];
        if let Some(secret) = secret {
            form.push(("client_secret", secret.to_string()));
        }
        form
    };
    let post = |form: Vec<(&'static str, String)>| {
        let url = server.url("/oauth/token");
        async move { Client::new().post(url).form(&form).send().await.unwrap() }
    };
    assert_eq!(post(form(None)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        post(form(Some("wrong"))).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let ok = post(form(Some(secret))).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let tokens: Value = ok.json().await.unwrap();
    assert_eq!(
        mcp_status(&server, tokens["access_token"].as_str().unwrap()).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn forged_and_api_tokens_do_not_pass_as_access_tokens() {
    let (_env, server) = start();
    let (_client_id, tokens) = connect(&server).await;
    let access = tokens["access_token"].as_str().unwrap();
    // Flip a character of the signed claims: the signature no longer fits.
    let mut forged = access.to_string();
    let at = "pjo_".len() + 3;
    let flipped = if forged.as_bytes()[at] == b'A' {
        "B"
    } else {
        "A"
    };
    forged.replace_range(at..=at, flipped);
    assert_eq!(mcp_status(&server, &forged).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        mcp_status(&server, "pjo_garbage.00").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn oauth_tokens_cannot_reach_passkeys() {
    let (_env, server) = start_with(&[("PAPERJAM_PUBLIC_URL", "http://localhost:8140")]);
    let (_client_id, tokens) = connect(&server).await;
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(mcp_status(&server, access).await, StatusCode::OK);

    // REST answers OAuth access tokens like any unknown bearer.
    let http = Client::new();
    for request in [
        http.get(server.url("/api/v1/passkeys")),
        http.post(server.url("/api/v1/passkeys/challenge")),
        http.post(server.url("/api/v1/passkeys")).json(&json!({})),
        http.delete(server.url("/api/v1/passkeys/0190d3a0-0000-7000-8000-000000000000")),
    ] {
        let response = request.bearer_auth(access).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["code"], "invalid_token");
    }

    // And MCP offers no passkey tool at all.
    let listed: Value = http
        .post(server.url("/mcp"))
        .bearer_auth(access)
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tools = listed["result"]["tools"].as_array().expect("tools");
    assert!(!tools.is_empty());
    assert!(
        tools
            .iter()
            .all(|t| !t["name"].as_str().unwrap().contains("passkey"))
    );
}

#[tokio::test]
async fn web_lists_and_disconnects_a_connected_client() {
    let (_env, server) = start();
    let (_client_id, tokens) = connect(&server).await;
    let access = tokens["access_token"].as_str().unwrap().to_string();
    let browser = browser();
    sign_in(&browser, &server).await;

    let list: Value = browser
        .get(server.url("/api/v1/connections"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let connections = list.as_array().expect("a list");
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["client_name"], "Test client");
    assert!(connections[0]["last_used_at"].is_string());

    let id = connections[0]["id"].as_str().unwrap();
    let gone = browser
        .delete(server.url(&format!("/api/v1/connections/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NO_CONTENT);
    assert_eq!(mcp_status(&server, &access).await, StatusCode::UNAUTHORIZED);
    let again = browser
        .delete(server.url(&format!("/api/v1/connections/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}
