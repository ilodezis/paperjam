//! The calendar feed over HTTP: management for the signed-in user, and the
//! public `.ics` address that only its secret opens.

mod common;

use common::{Server, TestEnv, stderr, stdout};
use paperjam::ids::DocumentId;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

/// An environment with `alice`, one document due in 2031, and a running server.
async fn start() -> (TestEnv, Server) {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    {
        let app = env.app().await;
        let now = paperjam::time::now_ts();
        let id = DocumentId::new();
        sqlx::query(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?, 'Synthetic lease', ?, ?)",
        )
        .bind(id)
        .bind(&now)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) \
             VALUES (?, 'expiry_date', '{\"kind\":\"date\",\"value\":\"2031-02-03\"}', 'manual', ?)",
        )
        .bind(id)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
    }
    let server = env.serve();
    (env, server)
}

async fn login(server: &Server) -> Client {
    let client = Client::builder().cookie_store(true).build().unwrap();
    let response = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    client
}

async fn create(server: &Server, client: &Client) -> Value {
    let response = client
        .post(server.url("/api/v1/calendar-feed"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json().await.unwrap()
}

#[tokio::test]
async fn the_management_endpoints_need_a_signed_in_user() {
    let (_env, server) = start().await;
    let client = Client::new();
    for request in [
        client.get(server.url("/api/v1/calendar-feed")),
        client.post(server.url("/api/v1/calendar-feed")),
        client.delete(server.url("/api/v1/calendar-feed")),
    ] {
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn create_rotate_and_revoke_the_feed() {
    let (_env, server) = start().await;
    let client = login(&server).await;
    let url = server.url("/api/v1/calendar-feed");

    let state: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(state["active"], false);
    assert_eq!(state["lead_days"], 60);

    let first = create(&server, &client).await;
    let secret = first["secret"].as_str().unwrap().to_string();
    assert_eq!(first["path"], format!("/calendar/{secret}.ics"));
    // No public URL is configured, so the client builds the address.
    assert!(first["url"].is_null());

    let state: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(state["active"], true);
    assert!(!state.to_string().contains(&secret));

    let rotated = create(&server, &client).await;
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_ne!(new_secret, secret);
    let anonymous = Client::new();
    let old = anonymous
        .get(server.url(&format!("/calendar/{secret}.ics")))
        .send()
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::NOT_FOUND);
    let new = anonymous
        .get(server.url(&format!("/calendar/{new_secret}.ics")))
        .send()
        .await
        .unwrap();
    assert_eq!(new.status(), StatusCode::OK);

    let revoked = client.delete(&url).send().await.unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let gone = anonymous
        .get(server.url(&format!("/calendar/{new_secret}.ics")))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    let again = client.delete(&url).send().await.unwrap();
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_feed_is_calendar_text_without_a_session() {
    let (_env, server) = start().await;
    let client = login(&server).await;
    let secret = create(&server, &client).await["secret"]
        .as_str()
        .unwrap()
        .to_string();

    let response = Client::new()
        .get(server.url(&format!("/calendar/{secret}.ics")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "private, no-cache");
    let body = response.text().await.unwrap();
    assert!(body.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(body.contains("SUMMARY:Synthetic lease expires\r\n"));
    assert!(body.contains("DTSTART;VALUE=DATE:20310203\r\n"));
}

#[tokio::test]
async fn wrong_secrets_and_other_credentials_get_a_plain_404() {
    let (env, server) = start().await;
    let client = login(&server).await;
    let secret = create(&server, &client).await["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let out = env.run(&["token", "create", "--user", "alice", "ci"]);
    let token = stdout(&out).trim().to_string();

    let anonymous = Client::new();
    for path in [
        "/calendar/pjc_doesnotexistdoesnotexistdoesnotex.ics".to_string(),
        format!("/calendar/{secret}"),
        format!("/calendar/{token}.ics"),
        "/calendar/.ics".to_string(),
    ] {
        let response = anonymous.get(server.url(&path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store", "{path}");
        assert_eq!(
            response.headers()["referrer-policy"],
            "no-referrer",
            "{path}"
        );
        assert!(response.text().await.unwrap().is_empty());
    }

    // The calendar secret is no bearer token.
    let api = anonymous
        .get(server.url("/api/v1/stats"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_public_url_turns_into_the_full_address() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success());
    let server = env.serve_with(&[("PAPERJAM_PUBLIC_URL", "https://docs.example.org")]);
    let client = login(&server).await;
    let created = create(&server, &client).await;
    let secret = created["secret"].as_str().unwrap();
    assert_eq!(
        created["url"],
        format!("https://docs.example.org/calendar/{secret}.ics")
    );
}
