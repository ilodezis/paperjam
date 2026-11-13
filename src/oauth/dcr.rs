//! RFC 7591 dynamic client registration. Open, because MCP clients expect to
//! register themselves, but harmless: a registered client is inert until a
//! signed-in user approves it, registrations are limited per address, and
//! unapproved ones disappear after a day.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::http::HttpState;
use crate::oauth::{OAuthError, no_store, purge_stale};
use crate::{random, time};

const WINDOW: Duration = Duration::from_secs(3600);
const MAX_REGISTRATIONS: usize = 10;
const MAX_REDIRECT_URIS: usize = 10;
const MAX_NAME_CHARS: usize = 100;
/// Longest redirect URI accepted. It is stored and shown on the consent page.
pub const MAX_REDIRECT_URI_CHARS: usize = 2048;
/// Largest registration request. The few fields it may carry fit many times over.
pub const MAX_REGISTRATION_BYTES: usize = 16 * 1024;
/// Schemes of desktop apps that finish an OAuth sign-in through a deep link
/// (the MCP clients that have one). Any other non-web scheme is refused: a
/// registered address is where a browser is sent with an authorization code.
const APP_SCHEMES: &[&str] = &[
    "claude",
    "cursor",
    "goose",
    "raycast",
    "vscode",
    "vscode-insiders",
    "windsurf",
    "zed",
];

/// Counts registrations per address.
#[derive(Default)]
pub struct RegistrationLimiter {
    hits: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl RegistrationLimiter {
    /// Records an attempt; false when the address has used up its hour.
    pub fn allow(&self, ip: IpAddr) -> bool {
        let mut map = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() > 10_000 {
            map.retain(|_, times| times.iter().any(|t| t.elapsed() < WINDOW));
        }
        let times = map.entry(ip).or_default();
        times.retain(|t| t.elapsed() < WINDOW);
        if times.len() >= MAX_REGISTRATIONS {
            return false;
        }
        times.push(Instant::now());
        true
    }
}

#[derive(Deserialize)]
pub struct Registration {
    client_name: Option<String>,
    redirect_uris: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
}

fn invalid_metadata(what: impl Into<String>) -> OAuthError {
    OAuthError::bad_request("invalid_client_metadata", what)
}

/// A redirect URI a client may register: HTTPS, plain HTTP only to the local
/// machine, or the deep-link scheme of a known desktop app; never with a
/// fragment.
pub fn check_redirect_uri(raw: &str) -> Result<(), OAuthError> {
    if raw.chars().count() > MAX_REDIRECT_URI_CHARS {
        return Err(OAuthError::bad_request(
            "invalid_redirect_uri",
            format!("a redirect URI is at most {MAX_REDIRECT_URI_CHARS} characters"),
        ));
    }
    let invalid =
        |why: &str| OAuthError::bad_request("invalid_redirect_uri", format!("{raw}: {why}"));
    let url = Url::parse(raw).map_err(|_| invalid("not an absolute URI"))?;
    if url.fragment().is_some() {
        return Err(invalid("must not contain a fragment"));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" => match url.host_str() {
            Some("localhost" | "127.0.0.1" | "[::1]") => Ok(()),
            _ => Err(invalid("http is only allowed for localhost")),
        },
        scheme if APP_SCHEMES.contains(&scheme) => Ok(()),
        _ => Err(invalid("this scheme is not allowed")),
    }
}

/// `POST /oauth/register`
pub async fn register(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Json<Registration>, JsonRejection>,
) -> Result<Response, OAuthError> {
    if !state
        .registrations
        .allow(state.client_addr.resolve(peer, &headers))
    {
        return Err(OAuthError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many registrations from this address, try again later",
        ));
    }
    let Json(body) = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            OAuthError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_client_metadata",
                "the registration request is too large",
            )
        } else {
            invalid_metadata(e.body_text())
        }
    })?;

    let uris = body.redirect_uris.unwrap_or_default();
    if uris.is_empty() {
        return Err(invalid_metadata("redirect_uris is required"));
    }
    if uris.len() > MAX_REDIRECT_URIS {
        return Err(invalid_metadata("too many redirect_uris"));
    }
    for uri in &uris {
        check_redirect_uri(uri)?;
    }
    // Registered clients are always public. A client asking for a secret-based
    // method (the MCP SDKs default to client_secret_post) is told `none` in the
    // reply, which RFC 7591 §3.2.1 allows; other methods cannot be honoured.
    if body
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|m| !matches!(m, "none" | "client_secret_post" | "client_secret_basic"))
    {
        return Err(invalid_metadata(
            "unsupported token_endpoint_auth_method: registered clients are public",
        ));
    }
    for (given, supported, what) in [
        (
            &body.grant_types,
            ["authorization_code", "refresh_token"],
            "grant_types",
        ),
        (&body.response_types, ["code", "code"], "response_types"),
    ] {
        if let Some(given) = given
            && let Some(bad) = given.iter().find(|g| !supported.contains(&g.as_str()))
        {
            return Err(invalid_metadata(format!("unsupported {what} {bad:?}")));
        }
    }
    let name: String = body
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Unnamed client")
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();

    purge_stale(&state.app).await?;
    let id = format!("pjc_{}", random::hex(16));
    sqlx::query(
        "INSERT INTO oauth_clients (id, name, redirect_uris, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(json!(uris).to_string())
    .bind(time::now_ts())
    .execute(&state.app.db.write)
    .await?;

    let reply = json!({
        "client_id": id,
        "client_name": name,
        "redirect_uris": uris,
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "client_id_issued_at": chrono::Utc::now().timestamp(),
    });
    let mut response = (StatusCode::CREATED, Json(reply)).into_response();
    no_store(&mut response);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris_are_capped_in_length() {
        let long = |n: usize| format!("https://client.example/{}", "a".repeat(n));
        let room = MAX_REDIRECT_URI_CHARS - "https://client.example/".len();
        assert!(check_redirect_uri(&long(room)).is_ok());
        let err = check_redirect_uri(&long(room + 1)).unwrap_err();
        assert_eq!(err.error, "invalid_redirect_uri");
        assert!(check_redirect_uri(&long(8 * 1024 * 1024)).is_err());
    }

    #[test]
    fn only_known_app_schemes_are_allowed() {
        for ok in [
            "https://client.example/cb",
            "http://localhost:3118/cb",
            "http://127.0.0.1:9/cb",
            "cursor://anysphere.cursor-retrieval/oauth/callback",
            "vscode://ms-vscode.mcp/cb",
            "claude://callback",
        ] {
            assert!(check_redirect_uri(ok).is_ok(), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "ftp://client.example/cb",
            "myapp://callback",
            "intent://callback#Intent;scheme=x;end",
            "tel:+10000000",
            "http://client.example/cb",
            "https://ok.example/cb#frag",
        ] {
            assert!(check_redirect_uri(bad).is_err(), "{bad}");
        }
    }
}
