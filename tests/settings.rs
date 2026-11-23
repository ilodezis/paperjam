//! Integration tests for runtime settings (`/api/v1/settings`,
//! `/api/v1/settings/summary`) and live propagation to LLM, calendar, and inbox.

mod common;

use std::time::Duration;

use common::{MockOpenRouter, MockReply, Server, TestEnv, stderr, stdout};
use paperjam::inbox::{Inbox, TickReport};
use paperjam::service::settings::{self, AdminSettingsPatch, InboxSettingsPatch, PatchOpt};
use paperjam::service::{Actor, App, Via};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "correct horse battery";

fn add_user(env: &TestEnv, username: &str, admin: bool) {
    let mut args = vec!["user", "add"];
    if admin {
        args.push("--admin");
    }
    args.push(username);
    let out = env.run_with_password(&args, PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
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

#[tokio::test]
async fn settings_auth_boundaries_are_enforced() {
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
    let alice_session = login(&server, "alice").await;
    let bob_session = login(&server, "bob").await;
    let anon = Client::new();

    // Unauthenticated callers get 401 on all settings routes.
    for resp in [
        anon.get(server.url("/api/v1/settings/summary"))
            .send()
            .await
            .unwrap(),
        anon.get(server.url("/api/v1/settings"))
            .send()
            .await
            .unwrap(),
        anon.patch(server.url("/api/v1/settings"))
            .json(&json!({"calendar": {"lead_days": 10}}))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // Non-admin user gets 200 on summary, 403 admin_required on GET/PATCH /settings.
    let bob_summary_cookie = bob_session
        .get(server.url("/api/v1/settings/summary"))
        .send()
        .await
        .unwrap();
    assert_eq!(bob_summary_cookie.status(), StatusCode::OK);

    let bob_summary_token = anon
        .get(server.url("/api/v1/settings/summary"))
        .bearer_auth(&bob_token)
        .send()
        .await
        .unwrap();
    assert_eq!(bob_summary_token.status(), StatusCode::OK);

    for resp in [
        bob_session
            .get(server.url("/api/v1/settings"))
            .send()
            .await
            .unwrap(),
        bob_session
            .patch(server.url("/api/v1/settings"))
            .json(&json!({"calendar": {"lead_days": 10}}))
            .send()
            .await
            .unwrap(),
        anon.get(server.url("/api/v1/settings"))
            .bearer_auth(&bob_token)
            .send()
            .await
            .unwrap(),
        anon.patch(server.url("/api/v1/settings"))
            .bearer_auth(&bob_token)
            .json(&json!({"calendar": {"lead_days": 10}}))
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

    // Admin Bearer token can read /settings but receives 403 session_required on PATCH /settings.
    let admin_token_get = anon
        .get(server.url("/api/v1/settings"))
        .bearer_auth(&alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_token_get.status(), StatusCode::OK);

    let admin_token_patch = anon
        .patch(server.url("/api/v1/settings"))
        .bearer_auth(&alice_token)
        .json(&json!({"calendar": {"lead_days": 10}}))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_token_patch.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        admin_token_patch.json::<Value>().await.unwrap()["code"],
        "session_required"
    );

    // Admin web session succeeds on PATCH /settings.
    let admin_session_patch = alice_session
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"calendar": {"lead_days": 15}}))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_session_patch.status(), StatusCode::OK);
    let updated: Value = admin_session_patch.json().await.unwrap();
    assert_eq!(updated["calendar"]["lead_days"], 15);
}

#[tokio::test]
async fn llm_settings_encrypt_api_key_audit_keys_only_and_activate_live_enrichment() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    let server = env.serve();
    let alice = login(&server, "alice").await;

    // Initially unconfigured: summary reports llm_configured=false, llm_enabled=false.
    let initial_summary: Value = alice
        .get(server.url("/api/v1/settings/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial_summary["llm_configured"], false);
    assert_eq!(initial_summary["llm_enabled"], false);

    let initial_admin: Value = alice
        .get(server.url("/api/v1/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial_admin["llm"]["has_api_key"], false);
    assert_eq!(initial_admin["llm"]["api_key_source"], "none");
    assert_eq!(initial_admin["llm"]["has_overrides"], false);

    // Configure LLM via PATCH /settings pointing to MockOpenRouter.
    let mock = MockOpenRouter::start().await;
    mock.route(
        "classification",
        MockReply::ok(&json!({
            "type_key": "other",
            "title": "Runtime Enriched Note",
            "document_date": "2026-05-01",
            "organizations": [],
            "new_tags": [],
            "summary": "Enriched through hot-reloaded LLM client.",
            "language": "en"
        })),
    );
    mock.route(
        "field_extraction",
        MockReply::ok(&json!({"fields": {}, "facts": []})),
    );

    let secret_key = "sk-live-secret-key-for-test";
    let patched_resp = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({
            "llm": {
                "enabled": true,
                "api_key": secret_key,
                "base_url": mock.base_url,
                "provider": "auto",
                "model": "custom/model-a",
                "vision_model": "custom/model-a",
                "handwriting_enabled": false,
                "monthly_budget_usd": "5.00",
                "prices": "custom/model-a=0.50:1.50",
                "reasoning_effort": "medium"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched_resp.status(), StatusCode::OK);
    let patched_text = patched_resp.text().await.unwrap();
    assert!(
        !patched_text.contains(secret_key),
        "plaintext api key leaked in PATCH response"
    );
    let patched: Value = serde_json::from_str(&patched_text).unwrap();
    assert_eq!(patched["llm"]["has_api_key"], true);
    assert_eq!(patched["llm"]["api_key_source"], "database");
    assert_eq!(patched["llm"]["effective_provider"], "generic");
    assert_eq!(patched["llm"]["model"], "custom/model-a");
    assert_eq!(patched["llm"]["monthly_budget_usd"], "5.00");
    assert_eq!(patched["llm"]["prices"], "custom/model-a=0.50:1.50");
    assert_eq!(patched["llm"]["reasoning_effort"], "medium");
    assert_eq!(patched["llm"]["has_overrides"], true);

    // GET /settings never exposes the plaintext key.
    let admin_after = alice
        .get(server.url("/api/v1/settings"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !admin_after.contains(secret_key),
        "plaintext api key leaked in GET /settings"
    );
    let admin_json: Value = serde_json::from_str(&admin_after).unwrap();
    assert_eq!(admin_json["llm"]["api_key_source"], "database");

    // GET /settings/summary reflects active settings.
    let summary_after: Value = alice
        .get(server.url("/api/v1/settings/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary_after["llm_enabled"], true);
    assert_eq!(summary_after["llm_configured"], true);
    assert_eq!(summary_after["llm_model"], "custom/model-a");
    assert_eq!(summary_after["llm_handwriting_enabled"], false);
    assert_eq!(
        summary_after["llm_allowed_models"],
        json!(["custom/model-a"])
    );
    assert_eq!(summary_after["llm_monthly_budget_usd"], "5.00");

    // Inspect SQLite directly: llm.api_key is AES-256-GCM encrypted and audit log records keys only.
    {
        let app = env.app().await;
        let (stored_val, encrypted): (String, i64) =
            sqlx::query_as("SELECT value, encrypted FROM app_settings WHERE key = 'llm.api_key'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(encrypted, 1);
        assert!(stored_val.starts_with("v1:"), "{stored_val}");
        assert!(!stored_val.contains(secret_key));

        let diff_str: String = sqlx::query_scalar(
            "SELECT diff FROM events WHERE action = 'settings.update' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        assert!(!diff_str.contains(secret_key), "secret leaked in audit log");
        assert!(
            !diff_str.contains(&mock.base_url),
            "setting value leaked in audit log: {diff_str}"
        );
        let diff: Value = serde_json::from_str(&diff_str).unwrap();
        let keys = diff["keys"].as_array().unwrap();
        assert!(keys.contains(&json!("llm.api_key")), "{diff}");
        assert!(keys.contains(&json!("llm.model")), "{diff}");
    }

    // Upload a digital document without local_only and verify the hot-reloaded LLM client runs!
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(
            b"Agreement between Alpha Corp and Beta LLC dated 2026-05-01.".to_vec(),
        )
        .file_name("note.txt")
        .mime_str("text/plain")
        .unwrap(),
    );
    let created: Value = alice
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = created["id"].as_str().unwrap();
    let doc = wait_until_ready(&server, &alice, doc_id).await;
    assert_eq!(doc["title"], "Runtime Enriched Note");
    assert_eq!(doc["summary"], "Enriched through hot-reloaded LLM client.");

    let sent = mock.requests();
    assert!(!sent.is_empty(), "mock should have received LLM requests");
    assert_eq!(
        sent[0].authorization.as_deref(),
        Some("Bearer sk-live-secret-key-for-test")
    );
    assert_eq!(sent[0].body["model"], "custom/model-a");
    assert_eq!(sent[0].body["reasoning_effort"], "medium");

    // Resetting LLM clears all overrides and restores defaults.
    let reset: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"reset_llm": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reset["llm"]["has_api_key"], false);
    assert_eq!(reset["llm"]["api_key_source"], "none");
    assert_eq!(reset["llm"]["has_overrides"], false);
    assert_eq!(reset["llm"]["model"], "anthropic/claude-haiku-5.5");
}

#[tokio::test]
async fn calendar_lead_days_updates_feed_live() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    {
        let app = env.app().await;
        let now = paperjam::time::now_ts();
        let doc_id = paperjam::ids::DocumentId::new();
        sqlx::query(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?, 'Synthetic lease', ?, ?)",
        )
        .bind(doc_id)
        .bind(&now)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) \
             VALUES (?, 'expiry_date', '{\"kind\":\"date\",\"value\":\"2031-02-03\"}', 'manual', ?)",
        )
        .bind(doc_id)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    let server = env.serve();
    let alice = login(&server, "alice").await;

    let created_feed: Value = alice
        .post(server.url("/api/v1/calendar-feed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let secret = created_feed["secret"].as_str().unwrap();

    let feed_before: Value = alice
        .get(server.url("/api/v1/calendar-feed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(feed_before["lead_days"], 60);

    let ics_before = Client::new()
        .get(server.url(&format!("/calendar/{secret}.ics")))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(ics_before.matches("BEGIN:VEVENT").count(), 2);
    assert!(ics_before.contains("-renew@paperjam"));

    // Setting calendar.lead_days = 0 updates both /calendar-feed and the .ics output immediately.
    let patched: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"calendar": {"lead_days": 0}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["calendar"]["lead_days"], 0);
    assert_eq!(patched["calendar"]["has_overrides"], true);

    let feed_after: Value = alice
        .get(server.url("/api/v1/calendar-feed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(feed_after["lead_days"], 0);

    let summary: Value = alice
        .get(server.url("/api/v1/settings/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["calendar_lead_days"], 0);

    let ics_after = Client::new()
        .get(server.url(&format!("/calendar/{secret}.ics")))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(ics_after.matches("BEGIN:VEVENT").count(), 1);
    assert!(!ics_after.contains("-renew@paperjam"));

    // Reset restores default 60 days.
    let reset: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"reset_calendar": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reset["calendar"]["lead_days"], 60);
    assert_eq!(reset["calendar"]["has_overrides"], false);
}

#[tokio::test]
async fn inbox_failed_files_listing_and_live_pause_behavior() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    let inbox_dir = tempfile::tempdir().unwrap();
    let failed_dir = inbox_dir.path().join(".failed");
    std::fs::create_dir_all(&failed_dir).unwrap();
    std::fs::write(failed_dir.join("bad.exe"), b"MZ\x90\x00").unwrap();
    std::fs::write(
        failed_dir.join("bad.exe.reason.txt"),
        "unsupported_type: unsupported file type\n",
    )
    .unwrap();

    let inbox_path = inbox_dir.path().to_string_lossy().into_owned();
    let server = env.serve_with(&[("PAPERJAM_INBOX_DIR", inbox_path.as_str())]);
    let alice = login(&server, "alice").await;

    let admin_view: Value = alice
        .get(server.url("/api/v1/settings"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin_view["inbox"]["configured"], true);
    assert_eq!(admin_view["inbox"]["paused"], false);
    let failed = admin_view["inbox"]["failed_files"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "bad.exe");
    assert_eq!(
        failed[0]["reason"],
        "unsupported_type: unsupported file type"
    );
    assert_eq!(failed[0]["size"], 4);
    assert!(failed[0]["failed_at"].is_string());

    // Pause via HTTP and confirm summary reflects it.
    let paused_view: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"inbox": {"paused": true, "interval_secs": 5}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(paused_view["inbox"]["paused"], true);
    assert_eq!(paused_view["inbox"]["interval_secs"], 5);

    let summary: Value = alice
        .get(server.url("/api/v1/settings/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["inbox_configured"], true);
    assert_eq!(summary["inbox_paused"], true);
    drop(server);

    // Drive Inbox::tick directly on an in-process App against the same DB to verify pause stops ingestion.
    let mut cfg = env.config_with(&[("PAPERJAM_INBOX_DIR", inbox_path.as_str())]);
    cfg.inbox_dir = Some(inbox_dir.path().to_path_buf());
    let app = std::sync::Arc::new(App::open(cfg).await.unwrap());
    let alice_id = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'")
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    let mut inbox = Inbox::new(std::sync::Arc::clone(&app), inbox_dir.path().to_path_buf())
        .with_min_age(Duration::ZERO);
    let cancel = CancellationToken::new();

    let scan_path = inbox_dir.path().join("queued.pdf");
    std::fs::write(
        &scan_path,
        b"%PDF-1.4\n% paused-check\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n",
    )
    .unwrap();

    // Still paused from the HTTP PATCH above: ticks do nothing and keep the file in place.
    for _ in 0..3 {
        assert_eq!(inbox.tick(&cancel).await, TickReport::default());
    }
    assert!(scan_path.exists());

    // Unpause via settings service and verify ingestion resumes immediately.
    settings::update_admin_settings(
        &app.ctx(Actor::user(alice_id, Via::Web)),
        AdminSettingsPatch {
            inbox: Some(InboxSettingsPatch {
                paused: Some(false),
                user_id: PatchOpt::Unset,
                interval_secs: None,
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let first = inbox.tick(&cancel).await;
    assert_eq!(first.waiting, 1);
    let second = inbox.tick(&cancel).await;
    assert_eq!(second.ingested, 1);
    assert!(!scan_path.exists());
}

#[tokio::test]
async fn allowed_models_controls_reprocess_allowlist_live() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    let server = env.serve();
    let alice = login(&server, "alice").await;

    let form = reqwest::multipart::Form::new()
        .text("local_only", "true")
        .part(
            "file",
            reqwest::multipart::Part::bytes(b"Plain note for reprocess test.".to_vec())
                .file_name("note.txt")
                .mime_str("text/plain")
                .unwrap(),
        );
    let created: Value = alice
        .post(server.url("/api/v1/documents"))
        .multipart(form)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = created["id"].as_str().unwrap();
    wait_until_ready(&server, &alice, doc_id).await;

    // Initially custom/new-model is rejected.
    let denied = alice
        .post(server.url(&format!("/api/v1/documents/{doc_id}/reprocess")))
        .json(&json!({"model": "custom/new-model", "local": false, "llm": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["code"],
        "model_not_allowed"
    );

    // Override allowed_models via PATCH /settings.
    let patched = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"llm": {"allowed_models": ["custom/new-model"]}}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let accepted = alice
        .post(server.url(&format!("/api/v1/documents/{doc_id}/reprocess")))
        .json(&json!({"model": "custom/new-model", "local": false, "llm": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);

    let old_denied = alice
        .post(server.url(&format!("/api/v1/documents/{doc_id}/reprocess")))
        .json(&json!({"model": "anthropic/claude-haiku-5.5", "local": false, "llm": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(old_denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        old_denied.json::<Value>().await.unwrap()["code"],
        "model_not_allowed"
    );

    // Sending empty allowed_models restores auto-derivation.
    let restored: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"llm": {"allowed_models": []}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        restored["llm"]["allowed_models"],
        json!(["anthropic/claude-haiku-5.5", "anthropic/claude-sonnet-5.5"])
    );

    let default_ok = alice
        .post(server.url(&format!("/api/v1/documents/{doc_id}/reprocess")))
        .json(&json!({"model": "anthropic/claude-haiku-5.5", "local": false, "llm": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(default_ok.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn validation_errors_clearable_fields_and_env_precedence() {
    let env = TestEnv::new();
    add_user(&env, "alice", true);
    add_user(&env, "bob", false);

    let server = env.serve_with(&[
        ("PAPERJAM_LLM_API_KEY", "env-key-123"),
        ("PAPERJAM_LLM_REASONING_EFFORT", "high"),
        ("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "25.00"),
        ("PAPERJAM_LLM_PRICES", "gpt-5=1.00:4.00"),
    ]);
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

    // Disable Bob to test invalid_inbox_user.
    let disable = alice
        .patch(server.url(&format!("/api/v1/users/{bob_id}")))
        .json(&json!({"disabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(disable.status(), StatusCode::OK);

    // Validation checks.
    let invalid_cases = [
        (
            json!({"llm": {"base_url": "ftp://example.org/v1"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_base_url",
        ),
        (
            json!({"llm": {"provider": "azure"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_provider",
        ),
        (
            json!({"llm": {"model": "   "}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_model",
        ),
        (
            json!({"llm": {"allowed_models": ["ok/model", "bad\nmodel"]}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_model",
        ),
        (
            json!({"llm": {"reasoning_effort": "extreme"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reasoning_effort",
        ),
        (
            json!({"llm": {"monthly_budget_usd": "-5.00"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_budget",
        ),
        (
            json!({"llm": {"prices": "broken-entry"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_llm_prices",
        ),
        (
            json!({"calendar": {"lead_days": 3651}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_calendar_lead_days",
        ),
        (
            json!({"inbox": {"interval_secs": 0}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_inbox_interval",
        ),
        (
            json!({"inbox": {"interval_secs": 86401}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_inbox_interval",
        ),
        (
            json!({"inbox": {"user_id": bob_id}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_inbox_user",
        ),
        (
            json!({"inbox": {"user_id": "0190d3a0-0000-7000-8000-000000000000"}}),
            StatusCode::NOT_FOUND,
            "user_not_found",
        ),
    ];

    for (payload, expected_status, expected_code) in invalid_cases {
        let resp = alice
            .patch(server.url("/api/v1/settings"))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), expected_status, "{payload}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["code"], expected_code, "{payload} => {body}");
    }

    // Clear env-provided reasoning_effort, monthly_budget_usd, and prices via null/empty.
    let cleared: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({
            "llm": {
                "enabled": false,
                "api_key": "db-key-override",
                "reasoning_effort": null,
                "monthly_budget_usd": "",
                "prices": null
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleared["llm"]["enabled"], false);
    assert_eq!(cleared["llm"]["api_key_source"], "database");
    assert!(cleared["llm"]["reasoning_effort"].is_null());
    assert!(cleared["llm"]["monthly_budget_usd"].is_null());
    assert_eq!(cleared["llm"]["prices"], "");

    // Omitting api_key or sending blank api_key preserves DB key; clear_api_key falls back to env.
    let kept: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"llm": {"api_key": "   "}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(kept["llm"]["api_key_source"], "database");

    let env_fallback: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"llm": {"clear_api_key": true}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(env_fallback["llm"]["api_key_source"], "env");

    // reset_llm restores env values for reasoning_effort, budget, prices, and enabled=true.
    let reset: Value = alice
        .patch(server.url("/api/v1/settings"))
        .json(&json!({"reset_llm": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reset["llm"]["enabled"], true);
    assert_eq!(reset["llm"]["api_key_source"], "env");
    assert_eq!(reset["llm"]["reasoning_effort"], "high");
    assert_eq!(reset["llm"]["monthly_budget_usd"], "25.00");
    assert_eq!(reset["llm"]["prices"], "gpt-5=1.00:4.00");
    assert_eq!(reset["llm"]["has_overrides"], false);
}
