//! Integration tests for `POST /api/v1/ingest/email`.

mod common;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::{Server, TestEnv, stderr, stdout};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";
const EMAIL_SECRET: &str = "email-ingest-secret-at-least-32-bytes!";

const PNG_1X1: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae,
    0x42, 0x60, 0x82,
];

fn fake_pdf(tag: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
}

fn add_user(env: &TestEnv, username: &str, admin: bool) {
    let mut args = vec!["user", "add"];
    if admin {
        args.push("--admin");
    }
    args.push(username);
    let out = env.run_with_password(&args, PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
}

fn start_with(vars: &[(&str, &str)]) -> (TestEnv, Server) {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    let server = env.serve_with(vars);
    (env, server)
}

async fn login(server: &Server, username: &str) -> Client {
    let client = Client::builder().cookie_store(true).build().unwrap();
    let response = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": username, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    client
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

fn sample_mixed_email(from: &str, pdf_tag: &str) -> Vec<u8> {
    let pdf = fake_pdf(pdf_tag);
    format!(
        "From: {from}\r\n\
         Subject: Monthly utility papers\r\n\
         Date: Sat, 21 Nov 2026 11:00:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"mix\"\r\n\
         \r\n\
         --mix\r\n\
         Content-Type: multipart/related; boundary=\"rel\"\r\n\
         \r\n\
         --rel\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         \r\n\
         <p>Attached</p><img src=\"cid:logo\">\r\n\
         --rel\r\n\
         Content-Type: image/png; name=\"logo.png\"\r\n\
         Content-ID: <logo>\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {}\r\n\
         --rel--\r\n\
         --mix\r\n\
         Content-Type: application/pdf; name=\"bill.pdf\"\r\n\
         Content-Disposition: attachment; filename=\"bill.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {}\r\n\
         --mix\r\n\
         Content-Type: image/png; name=\"scan.png\"\r\n\
         Content-Disposition: attachment; filename=\"scan.png\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         \r\n\
         {}\r\n\
         --mix\r\n\
         Content-Type: text/calendar; name=\"invite.ics\"\r\n\
         Content-Disposition: attachment; filename=\"invite.ics\"\r\n\
         \r\n\
         BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n\
         --mix--\r\n",
        B64.encode(PNG_1X1),
        B64.encode(&pdf),
        B64.encode(PNG_1X1),
    )
    .into_bytes()
}

fn html_receipt_email(from: &str, subject: &str, text: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\n\
         Subject: {subject}\r\n\
         Date: Sat, 21 Nov 2026 10:00:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         \r\n\
         <html><body><h1>Receipt</h1><p>{text}</p></body></html>\r\n"
    )
    .into_bytes()
}

#[tokio::test]
async fn unconfigured_server_returns_service_unavailable() {
    let (_env, server) = start_with(&[]);
    let response = Client::new()
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "unconf"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "email_not_configured");
}

#[tokio::test]
async fn auth_rejects_wrong_secret_and_user_api_tokens_and_rate_limits() {
    let (env, server) = start_with(&[("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET)]);
    let out = env.run(&["token", "create", "--user", "alice", "api"]);
    assert!(out.status.success(), "token create: {}", stderr(&out));
    let api_token = stdout(&out).trim().to_string();

    let http = Client::new();
    let payload = sample_mixed_email("alice@example.org", "auth");

    // Missing secret
    let missing = http
        .post(server.url("/api/v1/ingest/email"))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        missing.json::<Value>().await.unwrap()["code"],
        "invalid_email_secret"
    );

    // Regular user API token must NOT work on the email webhook
    let user_token = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&api_token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(user_token.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        user_token.json::<Value>().await.unwrap()["code"],
        "invalid_email_secret"
    );

    // Exhaust FREE_FAILURES (5) wrong attempts -> 6th fails and 7th is rate-limited (429).
    let mut saw_429 = false;
    for _ in 0..8 {
        let resp = http
            .post(server.url("/api/v1/ingest/email"))
            .bearer_auth("wrong-secret")
            .body(payload.clone())
            .send()
            .await
            .unwrap();
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(
                resp.json::<Value>().await.unwrap()["code"],
                "too_many_attempts"
            );
            saw_429 = true;
            break;
        }
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(saw_429, "expected 429 after repeated bad secrets");
}

#[tokio::test]
async fn ingests_attachments_skips_noise_deduplicates_and_restores_from_trash() {
    let (env, server) = start_with(&[("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET)]);
    let web = login(&server, "alice").await;
    let alice_id = web
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let raw = sample_mixed_email("alice@example.org", "happy-path");
    let http = Client::new();
    let resp = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .header("content-type", "message/rfc822")
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let report: Value = resp.json().await.unwrap();
    assert_eq!(report["used_body_fallback"], false);
    let docs = report["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 2, "{report}");
    assert_eq!(docs[0]["filename"], "bill.pdf");
    assert_eq!(docs[0]["status"], "queued");
    assert_eq!(docs[0]["restored"], false);
    assert_eq!(docs[1]["filename"], "scan.png");
    assert!(report["duplicates"].as_array().unwrap().is_empty());
    assert_eq!(
        report["skipped"],
        json!([
            {"filename": "logo.png", "reason": "inline_asset"},
            {"filename": "invite.ics", "reason": "unsupported_type"},
        ])
    );

    let bill_id = docs[0]["id"].as_str().unwrap().to_string();
    let bill = web
        .get(server.url(&format!("/api/v1/documents/{bill_id}")))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(bill["added_by"], alice_id);
    assert_eq!(bill["file"]["original_name"], "bill.pdf");

    // Re-sending reports duplicates and never triggers body fallback.
    let dup_resp: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(raw.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(dup_resp["documents"].as_array().unwrap().is_empty());
    assert_eq!(dup_resp["duplicates"].as_array().unwrap().len(), 2);
    assert_eq!(dup_resp["duplicates"][0]["document_id"], bill_id);
    assert_eq!(dup_resp["used_body_fallback"], false);

    // Trash bill.pdf and re-send -> bill.pdf is restored, scan.png stays duplicate.
    let trashed = web
        .post(server.url(&format!("/api/v1/documents/{bill_id}/trash")))
        .send()
        .await
        .unwrap();
    assert_eq!(trashed.status(), StatusCode::NO_CONTENT);

    let restore_resp: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(raw)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restore_resp["documents"].as_array().unwrap().len(), 1);
    assert_eq!(restore_resp["documents"][0]["id"], bill_id);
    assert_eq!(restore_resp["documents"][0]["restored"], true);
    assert_eq!(restore_resp["duplicates"].as_array().unwrap().len(), 1);

    drop(server);
    let app = env.app().await;
    let vias: Vec<(String,)> = sqlx::query_as(
        "SELECT via FROM events WHERE document_id = ? AND action = 'document.upload'",
    )
    .bind(&bill_id)
    .fetch_all(&app.db.read)
    .await
    .unwrap();
    assert_eq!(vias, [("email".to_string(),)]);
}

#[tokio::test]
async fn allowed_senders_checks_mime_and_prefers_envelope_from() {
    let (_env, server) = start_with(&[
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        (
            "PAPERJAM_EMAIL_ALLOWED_SENDERS",
            "alice@example.org,@partner.example",
        ),
    ]);
    let http = Client::new();

    // Exact match via MIME From.
    let r1 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("Alice <alice@example.org>", "s1"))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    // Domain match via MIME From.
    let r2 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("billing@partner.example", "s2"))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);

    // Unknown sender rejected.
    let r3 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("stranger@other.example", "s3"))
        .send()
        .await
        .unwrap();
    assert_eq!(r3.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        r3.json::<Value>().await.unwrap()["code"],
        "sender_not_allowed"
    );

    // Allowed envelope sender permits a forwarded message whose From header is a bank.
    let r4 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .header("x-envelope-from", "alice@example.org")
        .body(sample_mixed_email("statements@bank.example", "s4"))
        .send()
        .await
        .unwrap();
    assert_eq!(r4.status(), StatusCode::OK);

    // Disallowed envelope sender overrides a spoofed MIME From.
    let r5 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .header("x-envelope-from", "spoof@evil.example")
        .body(sample_mixed_email("alice@example.org", "s5"))
        .send()
        .await
        .unwrap();
    assert_eq!(r5.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        r5.json::<Value>().await.unwrap()["code"],
        "sender_not_allowed"
    );
}

#[tokio::test]
async fn body_fallback_toggle_creates_searchable_markdown_or_skips() {
    let (_env, server) = start_with(&[("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET)]);
    let web = login(&server, "alice").await;
    let raw = html_receipt_email(
        "shop@example.org",
        "Flight Receipt",
        "Booking reference KLM9921 total 215.50 EUR",
    );

    let report: Value = Client::new()
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(raw.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["used_body_fallback"], true);
    let doc_id = report["documents"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(report["documents"][0]["filename"], "Flight Receipt.md");

    let doc = wait_until_ready(&server, &web, &doc_id).await;
    assert_eq!(doc["status"], "ready");

    let search: Value = web
        .post(server.url("/api/v1/search"))
        .json(&json!({"query": "KLM9921"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hits = search["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], doc_id);

    // With PAPERJAM_EMAIL_BODY_FALLBACK=false, an attachment-less email creates 0 documents.
    let (_env2, server2) = start_with(&[
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        ("PAPERJAM_EMAIL_BODY_FALLBACK", "false"),
    ]);
    let report2: Value = Client::new()
        .post(server2.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(raw)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report2["used_body_fallback"], false);
    assert!(report2["documents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn local_only_and_tag_options_are_applied_and_tag_is_reused() {
    let (_env, server) = start_with(&[
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        ("PAPERJAM_EMAIL_LOCAL_ONLY", "true"),
        ("PAPERJAM_EMAIL_TAG", "email"),
    ]);
    let web = login(&server, "alice").await;
    let http = Client::new();

    let first: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "tag-one"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id1 = first["documents"][0]["id"].as_str().unwrap();

    let second: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "tag-two"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id2 = second["documents"][0]["id"].as_str().unwrap();

    let doc1: Value = web
        .get(server.url(&format!("/api/v1/documents/{id1}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc1["local_only"], true);
    assert_eq!(doc1["tags"].as_array().unwrap().len(), 1);
    assert_eq!(doc1["tags"][0]["name"], "email");

    let doc2: Value = web
        .get(server.url(&format!("/api/v1/documents/{id2}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc2["tags"][0]["id"], doc1["tags"][0]["id"]);
}

#[tokio::test]
async fn multi_user_requires_configured_email_user() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    add_user(&env, "bob", false);

    // Without PAPERJAM_EMAIL_USER -> 503 email_user_unconfigured.
    let server = env.serve_with(&[("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET)]);
    let resp = Client::new()
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "multi-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "email_user_unconfigured"
    );
    drop(server);

    // With PAPERJAM_EMAIL_USER=bob -> succeeds attributed to bob.
    let server = env.serve_with(&[
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        ("PAPERJAM_EMAIL_USER", "bob"),
    ]);
    let web = login(&server, "bob").await;
    let bob_id = web
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let report: Value = Client::new()
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "multi-2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = report["documents"][0]["id"].as_str().unwrap();
    let doc: Value = web
        .get(server.url(&format!("/api/v1/documents/{doc_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc["added_by"], bob_id);
}

#[tokio::test]
async fn oversize_email_body_is_rejected_with_413() {
    let (_env, server) = start_with(&[
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        ("PAPERJAM_MAX_UPLOAD_BYTES", "1024"),
    ]);
    let big_body = html_receipt_email("alice@example.org", "Huge", &"x".repeat(4096));
    let resp = Client::new()
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(big_body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["code"],
        "file_too_large"
    );
}

#[tokio::test]
async fn settings_lifecycle_generate_ingest_rotate_patch_revoke() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    add_user(&env, "bob", false);
    add_user(&env, "carol", false);
    let out = env.run(&["user", "disable", "carol"]);
    assert!(out.status.success(), "disable carol: {}", stderr(&out));

    let server = env.serve();
    let alice = login(&server, "alice").await;
    let bob = login(&server, "bob").await;

    let users: Vec<Value> = alice
        .get(server.url("/api/v1/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let alice_id = users.iter().find(|u| u["username"] == "alice").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bob_id = users.iter().find(|u| u["username"] == "bob").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol_id = users.iter().find(|u| u["username"] == "carol").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 1. Default unconfigured state.
    let initial: Value = alice
        .get(server.url("/api/v1/settings/email"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["source"], "none");
    assert_eq!(initial["has_secret"], false);
    assert_eq!(initial["secret_source"], "none");
    assert!(initial["secret_prefix"].is_null());
    assert!(
        initial["endpoint_url"]
            .as_str()
            .unwrap()
            .ends_with("/api/v1/ingest/email")
    );
    assert_eq!(initial["allowed_senders"], json!([]));
    assert_eq!(initial["body_fallback"], true);
    assert_eq!(initial["local_only"], false);
    assert!(initial["tag"].is_null());
    assert_eq!(initial["received_count"], 0);

    // Enabling without any secret returns 422 email_secret_required.
    let enable_no_secret = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(enable_no_secret.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        enable_no_secret.json::<Value>().await.unwrap()["code"],
        "email_secret_required"
    );

    // 2. Generate DB secret via POST /settings/email/secret.
    let gen_resp = alice
        .post(server.url("/api/v1/settings/email/secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(gen_resp.status(), StatusCode::CREATED);
    let created: Value = gen_resp.json().await.unwrap();
    let secret1 = created["secret"].as_str().unwrap().to_string();
    assert!(secret1.starts_with("pje_"), "{secret1}");
    assert_eq!(secret1.len(), 44);
    assert_eq!(created["settings"]["enabled"], true);
    assert_eq!(created["settings"]["source"], "database");
    assert_eq!(created["settings"]["has_secret"], true);
    assert_eq!(created["settings"]["secret_source"], "database");
    assert_eq!(created["settings"]["secret_prefix"], &secret1[..12]);

    // Subsequent GET does not reveal the secret.
    let after_gen_resp = alice
        .get(server.url("/api/v1/settings/email"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_gen_resp.status(), StatusCode::OK);
    let after_gen_text = after_gen_resp.text().await.unwrap();
    assert!(!after_gen_text.contains(&secret1));
    let after_gen: Value = serde_json::from_str(&after_gen_text).unwrap();
    assert!(after_gen.get("secret").is_none());
    assert_eq!(after_gen["secret_prefix"], &secret1[..12]);

    // 3. Multi-user requires selecting a user; once set, ingest succeeds and updates counters.
    let http = Client::new();
    let missing_target = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(sample_mixed_email("alice@example.org", "db-no-user"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_target.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        missing_target.json::<Value>().await.unwrap()["code"],
        "email_user_unconfigured"
    );

    let set_alice = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"user_id": alice_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(set_alice.status(), StatusCode::OK);

    let ingest1 = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(sample_mixed_email("alice@example.org", "db-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(ingest1.status(), StatusCode::OK);

    let stats_view: Value = alice
        .get(server.url("/api/v1/settings/email"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats_view["received_count"], 1);
    assert!(stats_view["last_received_at"].is_string());

    // 4. Validation errors on PATCH /settings/email.
    let bad_sender = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"allowed_senders": ["not-a-valid-rule"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_sender.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        bad_sender.json::<Value>().await.unwrap()["code"],
        "invalid_sender_rule"
    );

    let bad_tag = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"tag": "x".repeat(65)}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_tag.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        bad_tag.json::<Value>().await.unwrap()["code"],
        "invalid_tag"
    );

    let missing_user = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"user_id": "0190d3a0-0000-7000-8000-000000000000"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_user.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing_user.json::<Value>().await.unwrap()["code"],
        "user_not_found"
    );

    let disabled_user = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"user_id": carol_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled_user.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        disabled_user.json::<Value>().await.unwrap()["code"],
        "invalid_email_user"
    );

    // 5. Live update via PATCH /settings/email takes effect immediately without restart.
    let patched: Value = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({
            "allowed_senders": ["Trusted@Example.com", "@partner.example"],
            "tag": "  mail-in  ",
            "body_fallback": false,
            "local_only": true,
            "user_id": bob_id,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        patched["allowed_senders"],
        json!(["trusted@example.com", "@partner.example"])
    );
    assert_eq!(patched["tag"], "mail-in");
    assert_eq!(patched["body_fallback"], false);
    assert_eq!(patched["local_only"], true);
    assert_eq!(patched["user_id"], bob_id);

    // Disallowed sender rejected at once.
    let blocked = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(sample_mixed_email("stranger@other.example", "db-blocked"))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        blocked.json::<Value>().await.unwrap()["code"],
        "sender_not_allowed"
    );

    // Body fallback disabled takes effect at once.
    let no_body_doc: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(html_receipt_email(
            "trusted@example.com",
            "Receipt",
            "Should be ignored when body_fallback is false",
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(no_body_doc["used_body_fallback"], false);
    assert!(no_body_doc["documents"].as_array().unwrap().is_empty());

    // Allowed domain sender ingests attributed to Bob, local_only=true, tag="mail-in".
    let allowed_ingest: Value = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(sample_mixed_email("billing@partner.example", "db-bob"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = allowed_ingest["documents"][0]["id"].as_str().unwrap();
    let doc: Value = bob
        .get(server.url(&format!("/api/v1/documents/{doc_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(doc["added_by"], bob_id);
    assert_eq!(doc["local_only"], true);
    assert_eq!(doc["tags"][0]["name"], "mail-in");

    // 6. Rotate secret: old secret fails 401, new secret succeeds, settings preserved.
    let rotated_resp = alice
        .post(server.url("/api/v1/settings/email/secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(rotated_resp.status(), StatusCode::CREATED);
    let rotated: Value = rotated_resp.json().await.unwrap();
    let secret2 = rotated["secret"].as_str().unwrap().to_string();
    assert_ne!(secret1, secret2);
    assert!(secret2.starts_with("pje_"));
    assert_eq!(
        rotated["settings"]["allowed_senders"],
        json!(["trusted@example.com", "@partner.example"])
    );
    assert_eq!(rotated["settings"]["tag"], "mail-in");
    assert_eq!(rotated["settings"]["local_only"], true);
    assert_eq!(rotated["settings"]["secret_prefix"], &secret2[..12]);

    let old_secret_resp = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret1)
        .body(sample_mixed_email("trusted@example.com", "db-old-sec"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_secret_resp.status(), StatusCode::UNAUTHORIZED);

    let new_secret_resp = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret2)
        .body(sample_mixed_email("trusted@example.com", "db-new-sec"))
        .send()
        .await
        .unwrap();
    assert_eq!(new_secret_resp.status(), StatusCode::OK);

    // Clearing tag and user_id via null / empty string works.
    let cleared: Value = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"tag": null, "user_id": ""}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(cleared["tag"].is_null());
    assert!(cleared["user_id"].is_null());

    // 7. Revoke secret disables intake and re-enabling without secret fails 422.
    let revoked: Value = alice
        .delete(server.url("/api/v1/settings/email/secret"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revoked["enabled"], false);
    assert_eq!(revoked["has_secret"], false);
    assert_eq!(revoked["secret_source"], "none");
    assert!(revoked["secret_prefix"].is_null());

    let after_revoke = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(&secret2)
        .body(sample_mixed_email("trusted@example.com", "db-revoked"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_revoke.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        after_revoke.json::<Value>().await.unwrap()["code"],
        "email_not_configured"
    );

    let reenable_no_secret = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        reenable_no_secret.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        reenable_no_secret.json::<Value>().await.unwrap()["code"],
        "email_secret_required"
    );
}

#[tokio::test]
async fn env_config_can_be_overridden_and_revoked_from_db() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    add_user(&env, "bob", false);

    let vars = [
        ("PAPERJAM_EMAIL_SECRET", EMAIL_SECRET),
        ("PAPERJAM_EMAIL_USER", "bob"),
        ("PAPERJAM_EMAIL_TAG", "env-tag"),
    ];
    let server = env.serve_with(&vars);
    let alice = login(&server, "alice").await;
    let users: Vec<Value> = alice
        .get(server.url("/api/v1/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_id = users.iter().find(|u| u["username"] == "bob").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let view: Value = alice
        .get(server.url("/api/v1/settings/email"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["enabled"], true);
    assert_eq!(view["source"], "env");
    assert_eq!(view["has_secret"], true);
    assert_eq!(view["secret_source"], "env");
    assert_eq!(view["user_id"], bob_id);
    assert_eq!(view["tag"], "env-tag");

    // Patching non-secret settings seeds DB row while keeping env secret active.
    let patched: Value = alice
        .patch(server.url("/api/v1/settings/email"))
        .json(&json!({"local_only": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["source"], "database");
    assert_eq!(patched["secret_source"], "env");
    assert_eq!(patched["local_only"], true);
    assert_eq!(patched["user_id"], bob_id);
    assert_eq!(patched["tag"], "env-tag");

    let http = Client::new();
    let ok = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "env-patched"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    // Revoking disables intake even though PAPERJAM_EMAIL_SECRET is set in env.
    let revoked = alice
        .delete(server.url("/api/v1/settings/email/secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let revoked_view: Value = revoked.json().await.unwrap();
    assert_eq!(revoked_view["enabled"], false);
    assert_eq!(revoked_view["has_secret"], false);

    let blocked = http
        .post(server.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "env-revoked"))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        blocked.json::<Value>().await.unwrap()["code"],
        "email_not_configured"
    );

    // Remains disabled across server restart even with the env var still present.
    drop(server);
    let server2 = env.serve_with(&vars);
    let blocked_after_restart = http
        .post(server2.url("/api/v1/ingest/email"))
        .bearer_auth(EMAIL_SECRET)
        .body(sample_mixed_email("alice@example.org", "env-restarted"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        blocked_after_restart.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn email_settings_endpoints_enforce_admin_and_session_gates() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    add_user(&env, "bob", false);
    let out = env.run(&["token", "create", "--user", "alice", "admin-tok"]);
    assert!(out.status.success(), "alice token: {}", stderr(&out));
    let alice_token = stdout(&out).trim().to_string();
    let out = env.run(&["token", "create", "--user", "bob", "bob-tok"]);
    assert!(out.status.success(), "bob token: {}", stderr(&out));
    let bob_token = stdout(&out).trim().to_string();

    let server = env.serve();
    let bob_session = login(&server, "bob").await;
    let anon = Client::new();

    // Unauthenticated callers get 401 on every settings endpoint.
    for resp in [
        anon.get(server.url("/api/v1/settings/email"))
            .send()
            .await
            .unwrap(),
        anon.patch(server.url("/api/v1/settings/email"))
            .json(&json!({"local_only": true}))
            .send()
            .await
            .unwrap(),
        anon.post(server.url("/api/v1/settings/email/secret"))
            .send()
            .await
            .unwrap(),
        anon.delete(server.url("/api/v1/settings/email/secret"))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // Non-admin user (both web session and bearer token) gets 403 admin_required.
    for resp in [
        bob_session
            .get(server.url("/api/v1/settings/email"))
            .send()
            .await
            .unwrap(),
        bob_session
            .patch(server.url("/api/v1/settings/email"))
            .json(&json!({"local_only": true}))
            .send()
            .await
            .unwrap(),
        bob_session
            .post(server.url("/api/v1/settings/email/secret"))
            .send()
            .await
            .unwrap(),
        bob_session
            .delete(server.url("/api/v1/settings/email/secret"))
            .send()
            .await
            .unwrap(),
        anon.get(server.url("/api/v1/settings/email"))
            .bearer_auth(&bob_token)
            .send()
            .await
            .unwrap(),
        anon.patch(server.url("/api/v1/settings/email"))
            .bearer_auth(&bob_token)
            .json(&json!({"local_only": true}))
            .send()
            .await
            .unwrap(),
        anon.post(server.url("/api/v1/settings/email/secret"))
            .bearer_auth(&bob_token)
            .send()
            .await
            .unwrap(),
        anon.delete(server.url("/api/v1/settings/email/secret"))
            .bearer_auth(&bob_token)
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            resp.json::<Value>().await.unwrap()["code"],
            "admin_required"
        );
    }

    // Admin API Bearer token gets 200 on GET and 403 session_required on writes.
    let token_get = anon
        .get(server.url("/api/v1/settings/email"))
        .bearer_auth(&alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(token_get.status(), StatusCode::OK);

    for resp in [
        anon.patch(server.url("/api/v1/settings/email"))
            .bearer_auth(&alice_token)
            .json(&json!({"local_only": true}))
            .send()
            .await
            .unwrap(),
        anon.post(server.url("/api/v1/settings/email/secret"))
            .bearer_auth(&alice_token)
            .send()
            .await
            .unwrap(),
        anon.delete(server.url("/api/v1/settings/email/secret"))
            .bearer_auth(&alice_token)
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            resp.json::<Value>().await.unwrap()["code"],
            "session_required"
        );
    }
}
