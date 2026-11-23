mod common;

use common::{TestEnv, stderr, stdout};
use paperjam::service::tokens::resolve_token;
use paperjam::service::{Actor, Via};

#[tokio::test]
async fn cli_user_add_then_token_create() {
    let env = TestEnv::new();

    let out = env.run_with_password(&["user", "add", "--admin", "alice"], "correct horse");
    assert!(out.status.success(), "user add failed: {}", stderr(&out));

    let out = env.run(&["user", "list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let listing = stdout(&out);
    assert!(listing.contains("alice"), "{listing}");
    assert!(listing.contains("admin"), "{listing}");

    let out = env.run(&["token", "create", "--user", "alice", "ci"]);
    assert!(
        out.status.success(),
        "token create failed: {}",
        stderr(&out)
    );
    let raw = stdout(&out).trim().to_string();
    assert!(raw.starts_with("pj_"), "unexpected token output: {raw:?}");
    // The raw value is printed once and never stored.
    assert!(!stderr(&out).contains(&raw));

    // The printed token authenticates as alice.
    let app = env.app().await;
    let actor = resolve_token(&app, &raw).await.expect("token resolves");
    assert_eq!(actor.via, Via::Token);
    let users = paperjam::service::users::list_users(&app.ctx(Actor::cli()))
        .await
        .unwrap();
    assert_eq!(actor.user_id, Some(users[0].id));
}

#[test]
fn user_add_without_password_source_fails_cleanly() {
    let env = TestEnv::new();
    // No PAPERJAM_NEW_PASSWORD and stdin is not a terminal.
    let out = env
        .command()
        .args(["user", "add", "alice"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("PAPERJAM_NEW_PASSWORD"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn user_add_rejects_a_weak_password() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "alice"], "short");
    assert!(!out.status.success());
    assert!(stderr(&out).contains("weak_password"), "{}", stderr(&out));
}

#[test]
fn token_create_for_unknown_user_fails() {
    let env = TestEnv::new();
    let out = env.run(&["token", "create", "--user", "ghost", "ci"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("user_not_found"), "{}", stderr(&out));
}

// --- HTTP server -------------------------------------------------------------

use std::time::Duration;

use common::{Server, fixture};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

/// An environment with an admin `alice` and a running server.
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

async fn login(server: &Server, client: &Client) {
    let response = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

fn pdf_part(bytes: Vec<u8>, name: &str) -> Part {
    Part::bytes(bytes)
        .file_name(name.to_string())
        .mime_str("application/pdf")
        .unwrap()
}

async fn upload_bytes(
    server: &Server,
    client: &Client,
    bytes: Vec<u8>,
    name: &str,
) -> reqwest::Response {
    let form = Form::new()
        .text("local_only", "true")
        .part("file", pdf_part(bytes, name));
    client
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap()
}

async fn upload_fixture(server: &Server, client: &Client, fixture_name: &str, name: &str) -> Value {
    let bytes = std::fs::read(fixture(fixture_name)).unwrap();
    let response = upload_bytes(server, client, bytes, name).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json().await.unwrap()
}

async fn wait_until_ready(server: &Server, client: &Client, id: &str) -> Value {
    for _ in 0..300 {
        let doc: Value = client
            .get(server.url(&format!("/api/v1/documents/{id}")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        match doc["status"].as_str() {
            Some("ready") => return doc,
            Some("failed") => panic!("processing failed: {doc}"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("document {id} was not ready within 30 s");
}

#[tokio::test]
async fn local_only_sent_before_the_file_marks_the_document_local_only() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let bytes = std::fs::read(fixture("contract_ru_digital.pdf")).unwrap();
    let form = Form::new()
        .text("local_only", "true")
        .part("file", pdf_part(bytes, "lease.pdf"));
    let response = client
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    let doc = wait_until_ready(&server, &client, created["id"].as_str().unwrap()).await;
    assert_eq!(doc["local_only"], true, "{doc}");
}

#[tokio::test]
async fn login_logout_flow() {
    let (_env, server) = start();
    let client = browser();

    let wrong = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": "wrong password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let error: Value = wrong.json().await.unwrap();
    assert_eq!(error["code"], "invalid_credentials");

    let ok = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let cookie = ok.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.starts_with("pj_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(
        !cookie.contains("Secure"),
        "loopback must not be Secure: {cookie}"
    );

    let session = client
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    let body: Value = session.json().await.unwrap();
    assert_eq!(body["user"]["username"], "alice");

    let out = client
        .post(server.url("/api/v1/auth/logout"))
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
    let after = client
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_user_sets_their_own_language() {
    let (_env, server) = start();
    let client = browser();
    let ok = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let changed = client
        .patch(server.url("/api/v1/auth/session"))
        .json(&json!({"locale": "ru"}))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let body: Value = changed.json().await.unwrap();
    assert_eq!(body["user"]["locale"], "ru");
    let session: Value = client
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["user"]["locale"], "ru");

    let unknown = client
        .patch(server.url("/api/v1/auth/session"))
        .json(&json!({"locale": "de"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: Value = unknown.json().await.unwrap();
    assert_eq!(error["code"], "invalid_locale");
}

#[tokio::test]
async fn repeated_failed_logins_are_rate_limited() {
    let (_env, server) = start();
    let client = Client::new();
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        last = client
            .post(server.url("/api/v1/auth/login"))
            .json(&json!({"username": "alice", "password": "nope nope nope"}))
            .send()
            .await
            .unwrap()
            .status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

async fn try_login(server: &Server, from: &str, username: &str, password: &str) -> StatusCode {
    Client::new()
        .post(server.url("/api/v1/auth/login"))
        // The test server listens on loopback, a trusted proxy by default.
        .header("x-forwarded-for", from)
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn whitespace_and_case_do_not_dodge_the_login_limit() {
    let (_env, server) = start();
    let from = "198.51.100.66";
    for name in [
        "alice",
        "alice ",
        " Alice",
        "ALICE\t",
        "  alice  ",
        "aLice ",
    ] {
        assert_eq!(
            try_login(&server, from, name, "nope nope nope").await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        try_login(&server, from, " alice\t", "nope nope nope").await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn failures_from_another_address_do_not_lock_the_owner_out() {
    let (_env, server) = start();
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        last = try_login(&server, "198.51.100.66", "alice", "nope nope nope").await;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        try_login(&server, "203.0.113.7", "alice", PASSWORD).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn cookie_requests_from_other_sites_are_refused() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let post = |site: Option<&str>, origin: Option<&str>| {
        let mut request = client.post(server.url("/api/v1/auth/logout"));
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send()
    };
    for (site, origin) in [
        (Some("same-site"), None),
        (Some("cross-site"), None),
        (None, Some("https://evil.example.com")),
    ] {
        let response = post(site, origin).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{site:?} {origin:?}"
        );
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["code"], "cross_site_request");
    }
    // Still signed in; a same-origin request goes through.
    let session = client
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);
    let out = post(Some("same-origin"), None).await.unwrap();
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn the_session_cookie_is_host_only_over_https() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    let server = env.serve_with(&[("PAPERJAM_PUBLIC_URL", "https://docs.example.org")]);
    let ok = Client::new()
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let cookie = ok.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.starts_with("__Host-pj_session="), "{cookie}");
    assert!(cookie.contains("Secure"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(!cookie.contains("Domain"), "{cookie}");
}

#[tokio::test]
async fn api_requires_auth() {
    let (_env, server) = start();
    let client = Client::new();
    for path in ["/api/v1/stats", "/api/v1/taxonomy", "/api/v1/tokens"] {
        let response = client.get(server.url(path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        let error: Value = response.json().await.unwrap();
        assert!(error["code"].is_string(), "{error}");
    }
    let health = client.get(server.url("/healthz")).send().await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(health.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        health.headers()["cross-origin-opener-policy"],
        "same-origin"
    );
    assert_eq!(
        health.headers()["permissions-policy"],
        "camera=(), microphone=(), geolocation=()"
    );
    let csp = health.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("base-uri 'none'"));
    assert!(csp.contains("form-action 'self'"));
}

#[tokio::test]
async fn bearer_token_works() {
    let (env, server) = start();
    let out = env.run(&["token", "create", "--user", "alice", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let token = stdout(&out).trim().to_string();

    let client = Client::new();
    let response = client
        .get(server.url("/api/v1/stats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stats: Value = response.json().await.unwrap();
    assert_eq!(stats["documents"], 0);

    let bad = client
        .get(server.url("/api/v1/stats"))
        .bearer_auth("pj_not-a-real-token")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_users_returns_503() {
    let env = TestEnv::new();
    let server = env.serve();
    let client = Client::new();
    let response = client
        .get(server.url("/api/v1/stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["code"], "no_users");
    let health = client.get(server.url("/healthz")).send().await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn derived_images_revalidate_with_an_etag() {
    let (env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "lease.pdf").await;
    let id = created["id"].as_str().unwrap().to_string();
    wait_until_ready(&server, &client, &id).await;
    let url = server.url(&format!("/api/v1/documents/{id}/thumbnail.webp"));

    let first = client.get(&url).send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    // Kept, but checked on every use: a reprocess shows up at once and an
    // unchanged picture costs a header-only answer.
    assert_eq!(first.headers()["cache-control"], "private, no-cache");
    let etag = first.headers()["etag"].to_str().unwrap().to_string();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
    let body = first.bytes().await.unwrap();

    let same = client
        .get(&url)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(same.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(same.headers()["etag"], etag.as_str());
    assert!(same.bytes().await.unwrap().is_empty());

    let listed = client
        .get(&url)
        .header("if-none-match", format!("\"other\", W/{etag}"))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::NOT_MODIFIED);
    let stale = client
        .get(&url)
        .header("if-none-match", "\"other\"")
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::OK);
    assert_eq!(stale.bytes().await.unwrap(), body);

    // New pixels, new tag.
    let file = env
        .data_dir()
        .join("derived")
        .join(&id)
        .join("thumbnail.webp");
    std::fs::write(&file, b"RIFF....WEBPnew picture").unwrap();
    let changed = client
        .get(&url)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()["etag"], etag.as_str());
}

#[tokio::test]
async fn the_original_page_is_served_beside_the_clean_one() {
    let (env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "lease.pdf").await;
    let id = created["id"].as_str().unwrap().to_string();
    wait_until_ready(&server, &client, &id).await;
    let derived = env.data_dir().join("derived").join(&id);
    let get = |name: &str| {
        let url = server.url(&format!("/api/v1/documents/{id}/pages/{name}"));
        let client = client.clone();
        async move {
            let response = client.get(url).send().await.unwrap();
            (response.status(), response.bytes().await.unwrap())
        }
    };

    // No clean copy: both names are the one picture.
    let (status, page) = get("1.webp").await;
    assert_eq!(status, StatusCode::OK);
    let (status, same) = get("1-original.webp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(same, page);

    // With one, the original is kept beside the clean page.
    std::fs::write(derived.join("page-1.webp"), b"RIFF....WEBPclean").unwrap();
    std::fs::write(derived.join("original-1.webp"), b"RIFF....WEBPoriginal").unwrap();
    assert_eq!(&get("1.webp").await.1[..], b"RIFF....WEBPclean");
    assert_eq!(&get("1-original.webp").await.1[..], b"RIFF....WEBPoriginal");

    assert_eq!(
        get("x-original.webp").await.0,
        StatusCode::NOT_FOUND,
        "a name that is no page"
    );
}

#[tokio::test]
async fn upload_then_get_ready_document() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;

    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "lease.pdf").await;
    assert_eq!(created["status"], "queued");
    let id = created["id"].as_str().unwrap().to_string();

    let doc = wait_until_ready(&server, &client, &id).await;
    assert_eq!(doc["title"], "lease");
    assert_eq!(doc["file"]["original_name"], "lease.pdf");

    let page = client
        .get(server.url(&format!("/api/v1/documents/{id}?text_page=1")))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        page["text"]["text"]
            .as_str()
            .unwrap()
            .contains("Договор аренды")
    );

    let thumb = client
        .get(server.url(&format!("/api/v1/documents/{id}/thumbnail.webp")))
        .send()
        .await
        .unwrap();
    assert_eq!(thumb.status(), StatusCode::OK);
    assert_eq!(thumb.headers()["content-type"], "image/webp");
    let render = client
        .get(server.url(&format!("/api/v1/documents/{id}/pages/1.webp")))
        .send()
        .await
        .unwrap();
    assert_eq!(render.status(), StatusCode::OK);

    let found: Value = client
        .post(server.url("/api/v1/search"))
        .json(&json!({"query": "договор"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found["hits"][0]["id"], id.as_str());

    let stats: Value = client
        .get(server.url("/api/v1/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["documents"], 1);
}

#[tokio::test]
async fn duplicate_upload_returns_409_with_id() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let first = upload_fixture(&server, &client, "contract_ru_digital.pdf", "a.pdf").await;

    let bytes = std::fs::read(fixture("contract_ru_digital.pdf")).unwrap();
    let second = upload_bytes(&server, &client, bytes, "b.pdf").await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let error: Value = second.json().await.unwrap();
    assert_eq!(error["code"], "duplicate");
    assert_eq!(error["document_id"], first["id"]);
}

#[tokio::test]
async fn download_preserves_unicode_filename() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let name = "Договор №5 — копия 😀.pdf";
    let bytes = std::fs::read(fixture("contract_ru_digital.pdf")).unwrap();
    let created: Value = upload_bytes(&server, &client, bytes.clone(), name)
        .await
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let response = client
        .get(server.url(&format!("/api/v1/documents/{id}/original")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let disposition = response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(
        disposition
            .contains("filename*=UTF-8''%D0%94%D0%BE%D0%B3%D0%BE%D0%B2%D0%BE%D1%80%20%E2%84%965"),
        "{disposition}"
    );
    assert!(disposition.contains("%F0%9F%98%80.pdf"), "{disposition}");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.bytes().await.unwrap().to_vec(), bytes);

    let doc: Value = client
        .get(server.url(&format!("/api/v1/documents/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc["file"]["original_name"], name);
}

fn files_named(dir: &std::path::Path, needle: &str, found: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().contains(needle) {
            found.push(path.clone());
        }
        if path.is_dir() {
            files_named(&path, needle, found);
        }
    }
}

#[tokio::test]
async fn filename_cannot_escape_store() {
    let (env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let bytes = std::fs::read(fixture("contract_ru_digital.pdf")).unwrap();
    let created: Value = upload_bytes(&server, &client, bytes, "../../etc/passwd")
        .await
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let doc: Value = client
        .get(server.url(&format!("/api/v1/documents/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The name is display text only; it is kept as the user gave it.
    assert_eq!(doc["file"]["original_name"], "../../etc/passwd");
    let response = client
        .get(server.url(&format!("/api/v1/documents/{id}/original")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let disposition = response.headers()["content-disposition"].to_str().unwrap();
    // An untitled document is named after the uploaded name's last part only.
    assert!(
        disposition.contains("filename*=UTF-8''passwd"),
        "{disposition}"
    );
    assert!(!disposition.contains(".."), "{disposition}");

    let mut found = Vec::new();
    files_named(env.data_dir(), "passwd", &mut found);
    assert!(
        found.is_empty(),
        "a file was created from the name: {found:?}"
    );
    let parent = env.data_dir().parent().unwrap();
    assert!(!parent.join("etc").exists());
}

#[tokio::test]
async fn purge_with_bearer_is_403() {
    let (env, server) = start();
    let out = env.run(&["token", "create", "--user", "alice", "agent"]);
    let token = stdout(&out).trim().to_string();
    let client = browser();
    login(&server, &client).await;
    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "a.pdf").await;
    let id = created["id"].as_str().unwrap();
    wait_until_ready(&server, &client, id).await;

    let trash = client
        .post(server.url(&format!("/api/v1/documents/{id}/trash")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(trash.status(), StatusCode::NO_CONTENT);

    let purge = Client::new()
        .delete(server.url(&format!("/api/v1/documents/{id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(purge.status(), StatusCode::FORBIDDEN);
    let empty = Client::new()
        .delete(server.url("/api/v1/trash"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::FORBIDDEN);

    // The same call with the cookie session works.
    let purge = client
        .delete(server.url(&format!("/api/v1/documents/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(purge.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn reprocess_model_whitelist_http() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;
    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "a.pdf").await;
    let id = created["id"].as_str().unwrap();
    wait_until_ready(&server, &client, id).await;

    let bad = client
        .post(server.url(&format!("/api/v1/documents/{id}/reprocess")))
        .json(&json!({"model": "evil/model"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: Value = bad.json().await.unwrap();
    assert_eq!(error["code"], "model_not_allowed");

    let ok = client
        .post(server.url(&format!("/api/v1/documents/{id}/reprocess")))
        .json(&json!({"model": "anthropic/claude-haiku-5.5"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn clearing_local_only_with_bearer_is_403() {
    let (env, server) = start();
    let out = env.run(&["token", "create", "--user", "alice", "agent"]);
    let token = stdout(&out).trim().to_string();
    let client = browser();
    login(&server, &client).await;
    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "a.pdf").await;
    let id = created["id"].as_str().unwrap();
    wait_until_ready(&server, &client, id).await;

    let bearer_client = Client::new();

    let set_true = bearer_client
        .patch(server.url(&format!("/api/v1/documents/{id}")))
        .bearer_auth(&token)
        .json(&json!({"local_only": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(set_true.status(), StatusCode::OK);

    let clear_false = bearer_client
        .patch(server.url(&format!("/api/v1/documents/{id}")))
        .bearer_auth(&token)
        .json(&json!({"local_only": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(clear_false.status(), StatusCode::FORBIDDEN);

    let session_clear = client
        .patch(server.url(&format!("/api/v1/documents/{id}")))
        .json(&json!({"local_only": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(session_clear.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthcheck_command_exits_zero() {
    let (env, server) = start();
    let out = env.run(&["healthcheck", "--url", &server.url("/healthz")]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = env.run(&["healthcheck", "--url", "http://127.0.0.1:1/healthz"]);
    assert!(!out.status.success());
}

/// The routes of the plan's interface list.
const ROUTES: &[(&str, &str)] = &[
    ("get", "/setup"),
    ("post", "/setup"),
    ("post", "/auth/login"),
    ("post", "/auth/logout"),
    ("get", "/auth/session"),
    ("patch", "/auth/session"),
    ("post", "/auth/password"),
    ("post", "/auth/passkey/challenge"),
    ("post", "/auth/passkey"),
    ("post", "/documents"),
    ("get", "/documents/{id}"),
    ("patch", "/documents/{id}"),
    ("delete", "/documents/{id}"),
    ("get", "/documents/{id}/original"),
    ("post", "/documents/{id}/download-link"),
    ("post", "/upload-links"),
    ("get", "/documents/{id}/pages/{file}"),
    ("get", "/documents/{id}/thumbnail.webp"),
    ("get", "/documents/{id}/events"),
    ("post", "/documents/{id}/reprocess"),
    ("post", "/documents/{id}/trash"),
    ("post", "/documents/{id}/restore"),
    ("delete", "/trash"),
    ("post", "/search"),
    ("get", "/taxonomy"),
    ("post", "/people"),
    ("patch", "/people/{id}"),
    ("delete", "/people/{id}"),
    ("post", "/people/{id}/aliases"),
    ("delete", "/people/{id}/aliases"),
    ("post", "/people/{id}/merge"),
    ("post", "/people/{id}/relink"),
    ("post", "/correspondents"),
    ("patch", "/correspondents/{id}"),
    ("delete", "/correspondents/{id}"),
    ("post", "/correspondents/{id}/aliases"),
    ("delete", "/correspondents/{id}/aliases"),
    ("post", "/correspondents/{id}/merge"),
    ("post", "/tags"),
    ("patch", "/tags/{id}"),
    ("delete", "/tags/{id}"),
    ("post", "/types"),
    ("patch", "/types/{id}"),
    ("get", "/users"),
    ("post", "/users"),
    ("patch", "/users/{id}"),
    ("delete", "/users/{id}"),
    ("get", "/calendar-feed"),
    ("post", "/calendar-feed"),
    ("delete", "/calendar-feed"),
    ("get", "/tokens"),
    ("post", "/tokens"),
    ("delete", "/tokens/{id}"),
    ("get", "/connections"),
    ("delete", "/connections/{id}"),
    ("get", "/passkeys"),
    ("post", "/passkeys"),
    ("post", "/passkeys/challenge"),
    ("delete", "/passkeys/{id}"),
    ("get", "/stats"),
    ("get", "/settings/email"),
    ("patch", "/settings/email"),
    ("post", "/settings/email/secret"),
    ("delete", "/settings/email/secret"),
    ("get", "/dl/{token}"),
    ("put", "/ul/{token}"),
    ("post", "/ingest/email"),
    ("get", "/documents/{id}/download"),
    ("post", "/documents/{id}/shares"),
    ("get", "/documents/{id}/shares"),
    ("delete", "/shares/{id}"),
    ("get", "/shared/{token}/pages/{file}"),
    ("get", "/shared/{token}/download"),
    ("get", "/openapi.json"),
];

#[tokio::test]
async fn openapi_lists_every_route() {
    let (_env, server) = start();
    let spec: Value = Client::new()
        .get(server.url("/api/v1/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let paths = spec["paths"].as_object().expect("paths object");
    let strip = |p: &str| p.strip_prefix("/api/v1").unwrap_or(p).to_string();
    let documented: std::collections::BTreeMap<String, &Value> =
        paths.iter().map(|(p, v)| (strip(p), v)).collect();
    for (method, path) in ROUTES {
        let item = documented
            .get(*path)
            .unwrap_or_else(|| panic!("{path} is not in the OpenAPI document"));
        assert!(
            item.get(method).is_some(),
            "{method} {path} is not documented"
        );
    }
    // And every documented operation is a route that exists.
    let client = Client::new();
    for (path, item) in &documented {
        for method in ["get", "post", "patch", "delete", "put"] {
            if item.get(method).is_none() {
                continue;
            }
            let concrete = path
                .replace("{id}", "0190d3a0-0000-7000-8000-000000000000")
                .replace("{file}", "1.webp");
            let response = client
                .request(
                    method.to_uppercase().parse().unwrap(),
                    server.url(&format!("/api/v1{concrete}")),
                )
                .send()
                .await
                .unwrap();
            assert!(
                !matches!(
                    response.status(),
                    StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                ) || path == "/openapi.json",
                "{method} {path} answered {}",
                response.status()
            );
        }
    }
}

#[tokio::test]
async fn openapi_describes_the_search_request_and_answer() {
    let (_env, server) = start();
    let spec: Value = Client::new()
        .get(server.url("/api/v1/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let schemas = &spec["components"]["schemas"];
    let body =
        &spec["paths"]["/search"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(body["$ref"], "#/components/schemas/SearchRequest", "{body}");
    let request = schemas["SearchRequest"]["properties"]
        .as_object()
        .expect("SearchRequest has properties");
    for name in ["query", "filters", "sort", "limit", "cursor"] {
        assert!(request.contains_key(name), "no `{name}` in SearchRequest");
    }
    let filters = schemas["Filters"]["properties"]
        .as_object()
        .expect("Filters has properties");
    for name in [
        "types",
        "people",
        "correspondents",
        "tags",
        "date_from",
        "date_to",
        "has_attention",
        "local_only",
        "fields",
        "include_trashed",
    ] {
        assert!(filters.contains_key(name), "no `{name}` in Filters");
    }
    // The answer says whether it is a relaxed one.
    let page = &schemas["SearchPage"];
    assert!(page["properties"]["partial"].is_object(), "{page}");
    assert!(
        page["required"]
            .as_array()
            .unwrap()
            .contains(&json!("partial"))
    );
}

#[tokio::test]
async fn web_page_is_served_with_spa_fallback() {
    let (_env, server) = start();
    let client = Client::new();

    for path in ["/", "/documents/some-client-side-route"] {
        let response = client.get(server.url(path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{path}"
        );
        assert!(
            response.text().await.unwrap().contains("paperjam"),
            "{path}"
        );
    }
    // Missing files and unknown API paths are real 404s, not the app shell.
    for path in [
        "/assets/missing.js",
        "/favicon-missing.ico",
        "/api/v1/nothing-here",
    ] {
        let response = client.get(server.url(path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn people_link_to_existing_documents_and_can_be_relinked_on_request() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;

    let form = Form::new().part(
        "file",
        Part::bytes(b"Passport of Ivan Testov, born 1990".to_vec())
            .file_name("passport.txt")
            .mime_str("text/plain")
            .unwrap(),
    );
    let uploaded: Value = client
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = uploaded["id"].as_str().unwrap().to_string();
    wait_until_ready(&server, &client, &doc_id).await;

    // A person created afterwards is matched in the background.
    let person: Value = client
        .post(server.url("/api/v1/people"))
        .json(&json!({"name": "Ivan Testov"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let person_id = person["id"].as_str().unwrap().to_string();
    let documents_of_person = |client: Client| {
        let url = server.url("/api/v1/search");
        let person_id = person_id.clone();
        async move {
            let page: Value = client
                .post(url)
                .json(&json!({"filters": {"people": [person_id]}}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            page["hits"].as_array().unwrap().len()
        }
    };
    let mut linked = 0;
    for _ in 0..100 {
        linked = documents_of_person(client.clone()).await;
        if linked == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(linked, 1, "the document was not linked to the new person");

    // The explicit action queues the same matching and says so.
    let relink = client
        .post(server.url(&format!("/api/v1/people/{person_id}/relink")))
        .send()
        .await
        .unwrap();
    assert_eq!(relink.status(), StatusCode::ACCEPTED);
    let view: Value = relink.json().await.unwrap();
    assert_eq!(view["id"], person_id.as_str());

    let missing = client
        .post(server.url("/api/v1/people/0190d3a0-0000-7000-8000-000000000000/relink"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_person_and_correspondent_via_api() {
    let (_env, server) = start();
    let client = browser();
    login(&server, &client).await;

    // Create person
    let person_res = client
        .post(server.url("/api/v1/people"))
        .json(&json!({"name": "Mihai Eminescu", "aliases": ["Eminescu"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(person_res.status(), StatusCode::CREATED);
    let person_val: Value = person_res.json().await.unwrap();
    let person_id = person_val["id"].as_str().unwrap();

    // Verify present in taxonomy
    let tax: Value = client
        .get(server.url("/api/v1/taxonomy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tax["people"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == person_id)
    );

    // Delete person
    let del_person = client
        .delete(server.url(&format!("/api/v1/people/{person_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del_person.status(), StatusCode::NO_CONTENT);

    // Second delete returns 404
    let del_again = client
        .delete(server.url(&format!("/api/v1/people/{person_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del_again.status(), StatusCode::NOT_FOUND);

    // Absent from taxonomy
    let tax: Value = client
        .get(server.url("/api/v1/taxonomy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !tax["people"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == person_id)
    );

    // Create correspondent
    let corr_res = client
        .post(server.url("/api/v1/correspondents"))
        .json(&json!({"name": "Moldtelecom", "aliases": ["MTC"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(corr_res.status(), StatusCode::CREATED);
    let corr_val: Value = corr_res.json().await.unwrap();
    let corr_id = corr_val["id"].as_str().unwrap();

    // Verify present in taxonomy
    let tax: Value = client
        .get(server.url("/api/v1/taxonomy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tax["correspondents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == corr_id)
    );

    // Delete correspondent
    let del_corr = client
        .delete(server.url(&format!("/api/v1/correspondents/{corr_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del_corr.status(), StatusCode::NO_CONTENT);

    // Second delete returns 404
    let del_corr_again = client
        .delete(server.url(&format!("/api/v1/correspondents/{corr_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del_corr_again.status(), StatusCode::NOT_FOUND);

    // Absent from taxonomy
    let tax: Value = client
        .get(server.url("/api/v1/taxonomy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !tax["correspondents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == corr_id)
    );
}

#[tokio::test]
async fn admin_manages_users_and_users_change_own_password() {
    let (env, server) = start();
    let admin = browser();
    login(&server, &admin).await;

    // Create a member account.
    let created: Value = admin
        .post(server.url("/api/v1/users"))
        .json(&json!({
            "username": "bob",
            "display_name": "Bob",
            "password": "initial password 1",
            "locale": "en",
            "is_admin": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_id = created["id"].as_str().unwrap().to_string();

    // Bob signs in on two sessions.
    let bob_one = browser();
    let bob_two = browser();
    for client in [&bob_one, &bob_two] {
        let login = client
            .post(server.url("/api/v1/auth/login"))
            .json(&json!({"username": "bob", "password": "initial password 1"}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
    }

    // Bob cannot manage users.
    let forbidden = bob_one
        .delete(server.url(&format!("/api/v1/users/{bob_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // Wrong current password is refused.
    let wrong = bob_one
        .post(server.url("/api/v1/auth/password"))
        .json(&json!({"current_password": "wrong password", "new_password": "new password 2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        wrong.json::<Value>().await.unwrap()["code"],
        "wrong_password"
    );

    // Bob changes his own password: current session stays alive, other session ends.
    let changed = bob_one
        .post(server.url("/api/v1/auth/password"))
        .json(&json!({"current_password": "initial password 1", "new_password": "new password 2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        bob_one
            .get(server.url("/api/v1/auth/session"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        bob_two
            .get(server.url("/api/v1/auth/session"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Token cannot change password.
    let tok_out = env.run(&["token", "create", "--user", "bob", "bob-tok"]);
    assert!(tok_out.status.success(), "{}", stderr(&tok_out));
    let token = stdout(&tok_out).trim().to_string();
    let tok_resp = Client::new()
        .post(server.url("/api/v1/auth/password"))
        .bearer_auth(&token)
        .json(&json!({"current_password": "new password 2", "new_password": "third password 3"}))
        .send()
        .await
        .unwrap();
    assert_eq!(tok_resp.status(), StatusCode::FORBIDDEN);

    // Admin disables, re-enables, resets password, and deletes Bob.
    let disabled = admin
        .patch(server.url(&format!("/api/v1/users/{bob_id}")))
        .json(&json!({"disabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert!(disabled.json::<Value>().await.unwrap()["disabled_at"].is_string());
    assert_eq!(
        bob_one
            .get(server.url("/api/v1/auth/session"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let enabled = admin
        .patch(server.url(&format!("/api/v1/users/{bob_id}")))
        .json(&json!({"disabled": false, "password": "reset password 4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    assert!(enabled.json::<Value>().await.unwrap()["disabled_at"].is_null());

    let relogin = Client::new()
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "bob", "password": "reset password 4"}))
        .send()
        .await
        .unwrap();
    assert_eq!(relogin.status(), StatusCode::OK);

    // Admin cannot delete themselves.
    let me: Value = admin
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let admin_id = me["user"]["id"].as_str().unwrap();
    let self_del = admin
        .delete(server.url(&format!("/api/v1/users/{admin_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(self_del.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Deleting Bob succeeds and removes him from the list.
    let deleted = admin
        .delete(server.url(&format!("/api/v1/users/{bob_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let users: Vec<Value> = admin
        .get(server.url("/api/v1/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!users.iter().any(|u| u["id"] == bob_id));
}

#[test]
fn cli_user_lifecycle_commands_work() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], "correct horse");
    assert!(out.status.success(), "{}", stderr(&out));
    let out = env.run_with_password(&["user", "add", "bob"], "battery staple");
    assert!(out.status.success(), "{}", stderr(&out));

    for args in [
        &["user", "disable", "bob"][..],
        &["user", "enable", "bob"][..],
    ] {
        let out = env.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    }
    let out = env.run_with_password(&["user", "passwd", "bob"], "new battery staple");
    assert!(out.status.success(), "{}", stderr(&out));

    let out = env.run(&["user", "delete", "bob"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let list = env.run(&["user", "list"]);
    assert!(list.status.success());
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(listed.contains("alice\tadmin\tactive"), "{listed}");
    assert!(!listed.contains("bob"), "{listed}");
}

#[tokio::test]
async fn an_http_public_url_gets_cookies_that_work_without_tls() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    // A home network: listening beyond loopback, reached by plain HTTP.
    let server = env.serve_with(&[
        ("PAPERJAM_BIND", "0.0.0.0:0"),
        ("PAPERJAM_PUBLIC_URL", "http://192.0.2.10:8140"),
    ]);
    let ok = Client::new()
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let cookie = ok.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.starts_with("pj_session="), "{cookie}");
    assert!(!cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn document_events_endpoint_returns_newest_first_history() {
    let (_env, server) = start();
    let missing_id = "0190d3a0-0000-7000-8000-000000000000";

    let anon = Client::new()
        .get(server.url(&format!("/api/v1/documents/{missing_id}/events")))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let client = browser();
    login(&server, &client).await;

    let missing = client
        .get(server.url(&format!("/api/v1/documents/{missing_id}/events")))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_body: Value = missing.json().await.unwrap();
    assert_eq!(missing_body["code"], "document_not_found");

    let created = upload_fixture(&server, &client, "contract_ru_digital.pdf", "lease.pdf").await;
    let id = created["id"].as_str().unwrap();
    wait_until_ready(&server, &client, id).await;

    let patched = client
        .patch(server.url(&format!("/api/v1/documents/{id}")))
        .json(&json!({"title": "Updated lease"}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let resp = client
        .get(server.url(&format!("/api/v1/documents/{id}/events")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let events: Vec<Value> = resp.json().await.unwrap();
    assert!(events.len() >= 2, "{events:?}");
    assert!(
        events
            .windows(2)
            .all(|w| w[0]["id"].as_i64().unwrap() > w[1]["id"].as_i64().unwrap()),
        "expected newest-first ordering: {events:?}"
    );

    let update_idx = events
        .iter()
        .position(|e| e["action"] == "document.update")
        .expect("document.update event present");
    let upload_idx = events
        .iter()
        .position(|e| e["action"] == "document.upload")
        .expect("document.upload event present");
    assert!(
        update_idx < upload_idx,
        "events not newest first: {events:?}"
    );
    assert_eq!(events[update_idx]["via"], "web");
    assert_eq!(events[update_idx]["actor_name"], "alice");
    assert_eq!(events[upload_idx]["via"], "web");
    assert_eq!(events[upload_idx]["actor_name"], "alice");

    let limited = client
        .get(server.url(&format!("/api/v1/documents/{id}/events?limit=1")))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::OK);
    let limited_events: Vec<Value> = limited.json().await.unwrap();
    assert_eq!(limited_events.len(), 1);
    assert_eq!(limited_events[0]["id"], events[0]["id"]);
}
