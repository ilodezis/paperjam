//! The two discovery documents MCP clients start from.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use serde_json::{Value, json};

use crate::http::HttpState;
use crate::service::links;

fn base(state: &HttpState, headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    links::base_url(&state.app, host)
}

/// RFC 8414: where to register, authorize and get tokens.
pub async fn authorization_server(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<Value> {
    let base = base(&state, &headers);
    let auth_methods = ["none", "client_secret_post", "client_secret_basic"];
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": auth_methods,
        "revocation_endpoint_auth_methods_supported": auth_methods,
        "authorization_response_iss_parameter_supported": true,
    }))
}

/// RFC 9728: `/mcp` is a protected resource and this server is its authorization server.
pub async fn protected_resource(State(state): State<HttpState>, headers: HeaderMap) -> Json<Value> {
    let base = base(&state, &headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "resource_name": "paperjam",
    }))
}
