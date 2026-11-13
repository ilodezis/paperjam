//! First run: the secret key kept in the data directory, the setup page's
//! endpoint with its one-time code, and the administrator created from the
//! environment.

mod common;

use common::{SECRET, TestEnv, stderr};
use paperjam::service::setup::code_for_key;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

fn code() -> String {
    code_for_key(SECRET.as_bytes())
}

#[tokio::test]
async fn the_first_administrator_is_created_with_the_setup_code() {
    let env = TestEnv::new();
    let server = env.serve();
    let client = Client::builder().cookie_store(true).build().unwrap();

    let status: Value = client
        .get(server.url("/api/v1/setup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["needed"], true);

    let wrong = client
        .post(server.url("/api/v1/setup"))
        .json(&json!({"code": "0000-0000-0000", "username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: Value = wrong.json().await.unwrap();
    assert_eq!(error["code"], "invalid_setup_code");

    // Case, spaces and dashes in the code do not matter.
    let typed = code().to_uppercase().replace('-', " ");
    let created = client
        .post(server.url("/api/v1/setup"))
        .json(&json!({"code": typed, "username": "alice", "password": PASSWORD, "locale": "en"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let body: Value = created.json().await.unwrap();
    assert_eq!(body["user"]["username"], "alice");
    assert_eq!(body["user"]["is_admin"], true);

    // The setup signed the new administrator in.
    let session = client
        .get(server.url("/api/v1/auth/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);

    // It works once.
    let status: Value = client
        .get(server.url("/api/v1/setup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["needed"], false);
    let again = client
        .post(server.url("/api/v1/setup"))
        .json(&json!({"code": code(), "username": "mallory", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let error: Value = again.json().await.unwrap();
    assert_eq!(error["code"], "already_set_up");
}

#[tokio::test]
async fn racing_setup_requests_create_one_administrator() {
    let env = TestEnv::new();
    let server = env.serve();
    let client = Client::new();
    let attempts = (0..6).map(|i| {
        let client = client.clone();
        let url = server.url("/api/v1/setup");
        async move {
            client
                .post(url)
                .json(
                    &json!({"code": code(), "username": format!("user{i}"), "password": PASSWORD}),
                )
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    let statuses = futures_util::future::join_all(attempts).await;
    let created = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(created, 1, "{statuses:?}");
    assert!(
        statuses
            .iter()
            .all(|s| *s == StatusCode::OK || *s == StatusCode::CONFLICT),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn a_weak_password_does_not_use_up_the_setup() {
    let env = TestEnv::new();
    let server = env.serve();
    let client = Client::new();
    let weak = client
        .post(server.url("/api/v1/setup"))
        .json(&json!({"code": code(), "username": "alice", "password": "short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(weak.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let status: Value = client
        .get(server.url("/api/v1/setup"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["needed"], true);
}

#[tokio::test]
async fn wrong_codes_are_rate_limited() {
    let env = TestEnv::new();
    let server = env.serve();
    let client = Client::new();
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        last = client
            .post(server.url("/api/v1/setup"))
            .json(&json!({"code": "ffff-ffff-ffff", "username": "alice", "password": PASSWORD}))
            .send()
            .await
            .unwrap()
            .status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_environment_creates_the_first_administrator_once() {
    let env = TestEnv::new();
    let vars = [
        ("PAPERJAM_ADMIN_USER", "alice"),
        ("PAPERJAM_ADMIN_PASSWORD", PASSWORD),
    ];
    {
        let server = env.serve_with(&vars);
        let ok = Client::new()
            .post(server.url("/api/v1/auth/login"))
            .json(&json!({"username": "alice", "password": PASSWORD}))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let status: Value = Client::new()
            .get(server.url("/api/v1/setup"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["needed"], false);
    }
    // Another start with different values leaves the account alone.
    let server = env.serve_with(&[
        ("PAPERJAM_ADMIN_USER", "bob"),
        ("PAPERJAM_ADMIN_PASSWORD", PASSWORD),
    ]);
    let bob = Client::new()
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": "bob", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(bob.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn the_secret_key_is_generated_in_the_data_directory_and_shared_with_the_cli() {
    let env = TestEnv::new();
    let run = |args: &[&str]| {
        let mut cmd = env.command();
        cmd.env_remove("PAPERJAM_SECRET_KEY")
            .env("PAPERJAM_NEW_PASSWORD", PASSWORD)
            .args(args);
        cmd.output().unwrap()
    };
    let out = run(&["user", "add", "--admin", "alice"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let key = std::fs::read_to_string(env.data_dir().join("secret.key")).unwrap();
    assert!(key.trim().len() >= 32);
    // A second process reads the same key instead of making another.
    let out = run(&["token", "create", "--user", "alice", "ci"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(env.data_dir().join("secret.key")).unwrap(),
        key
    );
}

#[test]
fn a_backup_carries_the_generated_secret_key_to_another_machine() {
    let source = TestEnv::new();
    let run = |env: &TestEnv, args: &[&str]| {
        let mut cmd = env.command();
        cmd.env_remove("PAPERJAM_SECRET_KEY")
            .env("PAPERJAM_NEW_PASSWORD", PASSWORD)
            .args(args);
        cmd.output().unwrap()
    };
    let out = run(&source, &["user", "add", "--admin", "alice"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let key = std::fs::read_to_string(source.data_dir().join("secret.key")).unwrap();

    let snapshot = tempfile::tempdir().unwrap();
    let dir = snapshot.path().join("snap");
    let out = run(&source, &["backup", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));

    let target = TestEnv::new();
    let out = run(&target, &["restore", dir.to_str().unwrap(), "--force"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(target.data_dir().join("secret.key")).unwrap(),
        key
    );
}
