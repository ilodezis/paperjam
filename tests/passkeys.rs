//! Passkeys over HTTP: a software authenticator registers a passkey from a
//! signed-in web session and signs in with it, ending in the same cookie
//! session the password login opens.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{Server, TestEnv, stderr, stdout};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use url::Url;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};
use webauthn_rs_proto::AllowCredentials;

const PASSWORD: &str = "correct horse battery";
/// Plain http on localhost keeps the session cookie usable over the test's
/// http connection; the authenticator signs for this origin.
const PUBLIC_URL: &str = "http://localhost:8140";

fn start() -> (TestEnv, Server) {
    let env = TestEnv::new();
    for name in ["alice", "bob"] {
        let out = env.run_with_password(&["user", "add", name], PASSWORD);
        assert!(out.status.success(), "user add failed: {}", stderr(&out));
    }
    let server = env.serve_with(&[("PAPERJAM_PUBLIC_URL", PUBLIC_URL)]);
    (env, server)
}

fn browser() -> Client {
    Client::builder().cookie_store(true).build().unwrap()
}

fn origin() -> Url {
    Url::parse(PUBLIC_URL).unwrap()
}

/// A software authenticator that reports user verification.
fn authenticator() -> SoftPasskey {
    SoftPasskey::new(true)
}

async fn signed_in(server: &Server, username: &str) -> Client {
    let client = browser();
    let response = client
        .post(server.url("/api/v1/auth/login"))
        .json(&json!({"username": username, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    client
}

async fn send(request: reqwest::RequestBuilder) -> (StatusCode, Value) {
    let response = request.send().await.unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// A registration challenge and the authenticator's answer to it.
async fn registration(
    server: &Server,
    client: &Client,
    key: &mut SoftPasskey,
) -> (String, RegisterPublicKeyCredential) {
    let (status, challenge) = send(client.post(server.url("/api/v1/passkeys/challenge"))).await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let options = CreationChallengeResponse {
        public_key: serde_json::from_value(challenge["options"].clone()).unwrap(),
    };
    let credential = key.do_registration(origin(), options).unwrap();
    (
        challenge["ceremony"].as_str().unwrap().to_string(),
        credential,
    )
}

async fn add_passkey(
    server: &Server,
    client: &Client,
    key: &mut SoftPasskey,
    label: &str,
) -> Value {
    let (ceremony, credential) = registration(server, client, key).await;
    let (status, view) = send(client.post(server.url("/api/v1/passkeys")).json(&json!({
        "ceremony": ceremony,
        "label": label,
        "credential": credential,
    })))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    view
}

async fn login_challenge(
    server: &Server,
    username: Option<&str>,
) -> (String, RequestChallengeResponse) {
    let body = match username {
        Some(name) => json!({"username": name}),
        None => json!({}),
    };
    let (status, challenge) = send(
        Client::new()
            .post(server.url("/api/v1/auth/passkey/challenge"))
            .json(&body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let options = RequestChallengeResponse {
        public_key: serde_json::from_value(challenge["options"].clone()).unwrap(),
        mediation: None,
    };
    (challenge["ceremony"].as_str().unwrap().to_string(), options)
}

/// Signs a discoverable challenge the way a phone does: the soft key needs to
/// be told which credential to use, and answers with the user handle it was
/// registered under (the handle is not part of the signed data).
fn sign_discoverable(
    key: &mut SoftPasskey,
    mut options: RequestChallengeResponse,
    credential_id: &str,
    user_handle: &[u8],
) -> PublicKeyCredential {
    assert!(options.public_key.allow_credentials.is_empty());
    options.public_key.allow_credentials = vec![AllowCredentials {
        type_: "public-key".to_string(),
        id: URL_SAFE_NO_PAD.decode(credential_id).unwrap(),
        transports: None,
    }];
    let mut credential = key.do_authentication(origin(), options).unwrap();
    credential.response.user_handle = Some(user_handle.to_vec());
    credential
}

async fn finish_login(
    server: &Server,
    client: &Client,
    ceremony: &str,
    credential: &PublicKeyCredential,
) -> (StatusCode, Value) {
    send(
        client
            .post(server.url("/api/v1/auth/passkey"))
            .json(&json!({"ceremony": ceremony, "credential": credential})),
    )
    .await
}

async fn list(server: &Server, client: &Client) -> Value {
    let (status, body) = send(client.get(server.url("/api/v1/passkeys"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn handle_of(listing: &Value) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(listing["user_handle"].as_str().unwrap())
        .unwrap()
}

#[tokio::test]
async fn register_then_sign_in_opens_the_cookie_session() {
    let (env, server) = start();
    let alice = signed_in(&server, "alice").await;
    let mut key = authenticator();

    let view = add_passkey(&server, &alice, &mut key, "Phone").await;
    assert_eq!(view["label"], "Phone");
    assert!(view["last_used_at"].is_null());
    let listing = list(&server, &alice).await;
    assert_eq!(listing["available"], true);
    assert_eq!(listing["rp_id"], "localhost");
    assert_eq!(listing["passkeys"].as_array().unwrap().len(), 1);
    assert!(listing["passkeys"][0].get("passkey").is_none());

    // Username first: a fresh browser signs in and gets the pj_session cookie.
    let (ceremony, options) = login_challenge(&server, Some("alice")).await;
    assert_eq!(options.public_key.allow_credentials.len(), 1);
    let phone = browser();
    let credential = key.do_authentication(origin(), options).unwrap();
    let response = phone
        .post(server.url("/api/v1/auth/passkey"))
        .json(&json!({"ceremony": ceremony, "credential": credential}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("pj_session="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["user"]["username"], "alice");

    let (status, session) = send(phone.get(server.url("/api/v1/auth/session"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["user"]["username"], "alice");
    assert_eq!(session["via"], "web");

    // Without a username, the browser picks the discoverable passkey.
    let (ceremony, options) = login_challenge(&server, None).await;
    let credential_id = view["credential_id"].as_str().unwrap();
    let credential = sign_discoverable(&mut key, options, credential_id, &handle_of(&listing));
    let laptop = browser();
    let (status, body) = finish_login(&server, &laptop, &ceremony, &credential).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send(laptop.get(server.url("/api/v1/auth/session"))).await;
    assert_eq!(status, StatusCode::OK);

    // The use is recorded, and so is the registration in the audit log.
    assert!(list(&server, &alice).await["passkeys"][0]["last_used_at"].is_string());
    let app = env.app().await;
    let actions: Vec<(String, String)> =
        sqlx::query_as("SELECT action, via FROM events WHERE action LIKE 'passkey.%'")
            .fetch_all(&app.db.read)
            .await
            .unwrap();
    assert_eq!(
        actions,
        [("passkey.register".to_string(), "web".to_string())]
    );
}

#[tokio::test]
async fn a_challenge_is_answered_once() {
    let (_env, server) = start();
    let alice = signed_in(&server, "alice").await;
    let mut key = authenticator();

    // Registration: the same ceremony cannot store a second key.
    let (ceremony, credential) = registration(&server, &alice, &mut key).await;
    let body = json!({"ceremony": ceremony, "label": "Phone", "credential": credential});
    let (status, _) = send(alice.post(server.url("/api/v1/passkeys")).json(&body)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, error) = send(alice.post(server.url("/api/v1/passkeys")).json(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "ceremony_expired");

    // Sign-in: replaying the accepted answer is refused...
    let (ceremony, options) = login_challenge(&server, Some("alice")).await;
    let credential = key.do_authentication(origin(), options.clone()).unwrap();
    let (status, _) = finish_login(&server, &browser(), &ceremony, &credential).await;
    assert_eq!(status, StatusCode::OK);
    let (status, error) = finish_login(&server, &browser(), &ceremony, &credential).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "ceremony_expired");
    // ...and so is a freshly signed answer to the spent challenge.
    let again = key.do_authentication(origin(), options).unwrap();
    let (status, error) = finish_login(&server, &browser(), &ceremony, &again).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "ceremony_expired");

    // An answer to a different challenge does not fit a fresh ceremony either.
    let (ceremony, _) = login_challenge(&server, Some("alice")).await;
    let (status, error) = finish_login(&server, &browser(), &ceremony, &again).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["code"], "invalid_passkey");

    // A ceremony id that was never issued.
    let (status, error) = finish_login(&server, &browser(), "not-a-ceremony", &again).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["code"], "ceremony_expired");
}

#[tokio::test]
async fn another_users_credential_is_refused() {
    let (_env, server) = start();
    let alice = signed_in(&server, "alice").await;
    let bob = signed_in(&server, "bob").await;
    let mut alice_key = authenticator();
    let mut bob_key = authenticator();
    let alice_view = add_passkey(&server, &alice, &mut alice_key, "Alice's phone").await;
    let bob_view = add_passkey(&server, &bob, &mut bob_key, "Bob's phone").await;
    let alice_handle = handle_of(&list(&server, &alice).await);

    // Signing in as alice with bob's key.
    let (ceremony, mut options) = login_challenge(&server, Some("alice")).await;
    options.public_key.allow_credentials = vec![AllowCredentials {
        type_: "public-key".to_string(),
        id: URL_SAFE_NO_PAD
            .decode(bob_view["credential_id"].as_str().unwrap())
            .unwrap(),
        transports: None,
    }];
    let credential = bob_key.do_authentication(origin(), options).unwrap();
    let (status, error) = finish_login(&server, &browser(), &ceremony, &credential).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["code"], "invalid_passkey");

    // Bob's key claiming to be alice's account.
    let (ceremony, options) = login_challenge(&server, None).await;
    let credential = sign_discoverable(
        &mut bob_key,
        options,
        bob_view["credential_id"].as_str().unwrap(),
        &alice_handle,
    );
    let (status, error) = finish_login(&server, &browser(), &ceremony, &credential).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["code"], "invalid_passkey");

    // Bob neither sees nor removes alice's passkey.
    let bob_list = list(&server, &bob).await;
    assert_eq!(bob_list["passkeys"].as_array().unwrap().len(), 1);
    assert_eq!(bob_list["passkeys"][0]["label"], "Bob's phone");
    let id = alice_view["id"].as_str().unwrap();
    let (status, error) = send(bob.delete(server.url(&format!("/api/v1/passkeys/{id}")))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "passkey_not_found");
    assert_eq!(
        list(&server, &alice).await["passkeys"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn revoked_passkey_no_longer_signs_in() {
    let (env, server) = start();
    let alice = signed_in(&server, "alice").await;
    let mut key = authenticator();
    let view = add_passkey(&server, &alice, &mut key, "Phone").await;
    let id = view["id"].as_str().unwrap();

    let (status, _) = send(alice.delete(server.url(&format!("/api/v1/passkeys/{id}")))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        list(&server, &alice).await["passkeys"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = send(alice.delete(server.url(&format!("/api/v1/passkeys/{id}")))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The key still holds the credential; paperjam no longer knows it.
    let (ceremony, mut options) = login_challenge(&server, Some("alice")).await;
    assert!(options.public_key.allow_credentials.is_empty());
    options.public_key.allow_credentials = vec![AllowCredentials {
        type_: "public-key".to_string(),
        id: URL_SAFE_NO_PAD
            .decode(view["credential_id"].as_str().unwrap())
            .unwrap(),
        transports: None,
    }];
    let credential = key.do_authentication(origin(), options).unwrap();
    let (status, error) = finish_login(&server, &browser(), &ceremony, &credential).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["code"], "unknown_passkey");

    let app = env.app().await;
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM events WHERE action = 'passkey.revoke'")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn api_tokens_are_refused_on_passkey_endpoints() {
    let (env, server) = start();
    let out = env.run(&["token", "create", "--user", "alice", "agent"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let token = stdout(&out).trim().to_string();
    let alice = signed_in(&server, "alice").await;
    let mut key = authenticator();
    let view = add_passkey(&server, &alice, &mut key, "Phone").await;
    let id = view["id"].as_str().unwrap();

    // A well-formed registration answer, sent with the token instead of the cookie.
    let (ceremony, credential) = registration(&server, &alice, &mut authenticator()).await;
    let body = json!({"ceremony": ceremony, "label": "agent", "credential": credential});

    let http = Client::new();
    for request in [
        http.get(server.url("/api/v1/passkeys")),
        http.post(server.url("/api/v1/passkeys/challenge")),
        http.post(server.url("/api/v1/passkeys")).json(&body),
        http.delete(server.url(&format!("/api/v1/passkeys/{id}"))),
    ] {
        let (status, error) = send(request.bearer_auth(&token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
        assert_eq!(error["code"], "session_required");
    }
    // Nothing changed, and the refused request did not spend the ceremony.
    assert_eq!(
        list(&server, &alice).await["passkeys"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (status, _) = send(alice.post(server.url("/api/v1/passkeys")).json(&body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn passkeys_are_unavailable_without_a_public_url() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "alice"], PASSWORD);
    assert!(out.status.success(), "{}", stderr(&out));
    let server = env.serve();
    let alice = signed_in(&server, "alice").await;

    let listing = list(&server, &alice).await;
    assert_eq!(listing["available"], false);
    assert!(listing["rp_id"].is_null());
    for request in [
        alice.post(server.url("/api/v1/passkeys/challenge")),
        Client::new()
            .post(server.url("/api/v1/auth/passkey/challenge"))
            .json(&json!({})),
    ] {
        let (status, error) = send(request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["code"], "passkeys_unavailable");
    }
    // The password still signs in.
    let (status, _) = send(alice.get(server.url("/api/v1/auth/session"))).await;
    assert_eq!(status, StatusCode::OK);
}
