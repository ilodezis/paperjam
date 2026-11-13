//! The MCP adapter: a streamable-HTTP server at `/mcp`, stateless, with JSON
//! responses. Tools are declared in `tools.json` and implemented in `tools.rs`.

pub mod server;
pub mod stdio_proxy;
pub mod tools;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

use crate::http::HttpState;
use crate::service::{Actor, links, tokens};

/// Largest request body: 10 MB of file as base64 plus the JSON around it.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Headroom above [`MAX_REQUEST_BYTES`] so the service's own limit answers first.
const FRAMING_SLACK: usize = 64 * 1024;

/// The caller behind an authenticated `/mcp` request; set by [`authenticate`].
#[derive(Debug, Clone, Copy)]
pub struct McpActor(pub Actor);

/// The `/mcp` router, behind bearer authentication.
pub fn router(state: HttpState) -> Router<HttpState> {
    let app = Arc::clone(&state.app);
    // Every request carries a bearer credential and nothing here trusts the
    // network position, so DNS-rebinding protection by `Host` adds nothing:
    // the public host name is whatever the operator put in front.
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts();
    config.max_request_body_bytes = MAX_REQUEST_BYTES;
    let service = StreamableHttpService::new(
        move || Ok(server::PaperjamMcp::new(Arc::clone(&app))),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(state, authenticate))
        // The service enforces the same ceiling while it reads; this keeps
        // the body from being read at all past it, whatever the framing.
        .layer(RequestBodyLimitLayer::new(
            MAX_REQUEST_BYTES + FRAMING_SLACK,
        ))
}

fn unauthorized(state: &HttpState, request: &Request, presented: bool) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    let base = links::base_url(&state.app, host);
    let mut challenge =
        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\"");
    if presented {
        challenge.push_str(", error=\"invalid_token\"");
    }
    let mut response = (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({
            "code": if presented { "invalid_token" } else { "authentication_required" },
            "message": "send `Authorization: Bearer <API token or OAuth access token>`",
            "details": null,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

/// Accepts a bearer API token and records who is calling.
async fn authenticate(
    State(state): State<HttpState>,
    mut request: Request,
    next: Next,
) -> Response {
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string());
    let Some(raw) = bearer else {
        return unauthorized(&state, &request, false);
    };
    let resolved = if raw.starts_with(crate::oauth::ACCESS_PREFIX) {
        crate::oauth::resolve_access_token(&state.app, &raw).await
    } else {
        tokens::resolve_token(&state.app, &raw).await
    };
    match resolved {
        Ok(actor) => {
            request.extensions_mut().insert(McpActor(actor));
            next.run(request).await
        }
        Err(e) if e.kind == crate::error::ErrorKind::Unauthorized => {
            unauthorized(&state, &request, true)
        }
        Err(e) => crate::http::ApiError(e).into_response(),
    }
}
