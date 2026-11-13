//! The HTTP adapter: REST under `/api/v1`, health check, security headers.
//! Handlers only translate requests; all logic lives in the service layer.

pub mod api;
pub mod auth;
pub mod calendar;
pub mod client;
mod compressible;
mod csrf;
pub mod limiter;
pub mod openapi;
pub mod security;
pub mod share;
pub mod web;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::error::{ErrorKind, ServiceError};
use crate::service::App;

/// Room for multipart framing around the largest allowed upload.
const MULTIPART_OVERHEAD: u64 = 1024 * 1024;

/// What any request body may weigh unless its route says otherwise. Every
/// JSON, form and query endpoint is far below this; only the routes that take
/// a file (document upload, the one-time upload link) and MCP ask for more.
/// Without it an anonymous caller could park megabytes per request in memory.
pub const DEFAULT_BODY_LIMIT: usize = 64 * 1024;

/// The body ceiling of the routes that carry a file: the configured maximum
/// upload plus room for the multipart framing around it.
pub fn upload_body_limit(max_upload_bytes: u64) -> usize {
    usize::try_from(max_upload_bytes + MULTIPART_OVERHEAD).unwrap_or(usize::MAX)
}

#[derive(Clone)]
pub struct HttpState {
    pub app: Arc<App>,
    pub limiter: Arc<limiter::LoginLimiter>,
    /// Finds the client address behind the configured proxies.
    pub client_addr: Arc<client::ClientAddr>,
    pub registrations: Arc<crate::oauth::dcr::RegistrationLimiter>,
    /// Whether the session cookie is marked `Secure`.
    pub secure_cookies: bool,
    users_exist: Arc<AtomicBool>,
}

impl HttpState {
    pub fn new(app: Arc<App>) -> Self {
        // The public address says how people reach the server, so it decides:
        // an `http://` address (a home network without TLS) gets cookies that
        // work over plain HTTP. Without one, a server that listens beyond
        // loopback is assumed to sit behind a TLS proxy.
        let secure_cookies = match &app.config.public_url {
            Some(url) => url.scheme() == "https",
            None => !app.config.bind.ip().is_loopback(),
        };
        Self {
            secure_cookies,
            limiter: Arc::new(limiter::LoginLimiter::default()),
            client_addr: Arc::new(client::ClientAddr::new(
                app.config.trusted_proxies.clone(),
                app.config.client_ip_header.clone(),
            )),
            registrations: Arc::new(crate::oauth::dcr::RegistrationLimiter::default()),
            users_exist: Arc::new(AtomicBool::new(false)),
            app,
        }
    }
}

/// A [`ServiceError`] on its way out as JSON: `{"code", "message", "details"}`.
pub struct ApiError(pub ServiceError);

pub type ApiResult<T> = Result<T, ApiError>;

impl From<ServiceError> for ApiError {
    fn from(e: ServiceError) -> Self {
        ApiError(e)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError(e.into())
    }
}

fn rejection(code: &'static str, what: impl std::fmt::Display) -> ApiError {
    ApiError(ServiceError::invalid(code, what.to_string()))
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return ApiError(ServiceError::payload_too_large(
                "payload_too_large",
                "the request body is too large",
            ));
        }
        rejection("invalid_body", e.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(e: PathRejection) -> Self {
        rejection("invalid_path", e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        rejection("invalid_query", e.body_text())
    }
}

pub fn status_of(kind: ErrorKind) -> StatusCode {
    match kind {
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::Conflict => StatusCode::CONFLICT,
        ErrorKind::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
        ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorKind::Forbidden => StatusCode::FORBIDDEN,
        ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ErrorKind::Upstream => StatusCode::BAD_GATEWAY,
        ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        ErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorKind::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
    }
}

/// The JSON error body returned on failure: `{"code", "message", "details"}`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let e = self.0;
        let mut body = json!({
            "code": e.code,
            "message": e.message,
            "details": e.details.clone().unwrap_or(Value::Null),
        });
        // A duplicate upload also names the existing document at the top level.
        if e.code == "duplicate"
            && let Some(id) = e.details.as_ref().and_then(|d| d.get("document_id"))
        {
            body["document_id"] = id.clone();
        }
        (status_of(e.kind), Json(body)).into_response()
    }
}

/// `Json` that reports a malformed body in the API's error format.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

impl<T: serde::Serialize> IntoResponse for ApiJson<T> {
    fn into_response(self) -> Response {
        Json(self.0).into_response()
    }
}

/// `Path` with the API's error format.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct ApiPath<T>(pub T);

/// `Query` with the API's error format.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct ApiQuery<T>(pub T);

pub fn router(state: HttpState) -> Router {
    let upload_limit = upload_body_limit(state.app.config.max_upload_bytes);
    let (api, openapi) = api::router(upload_limit).split_for_parts();
    let api = api.layer(axum::Extension(Arc::new(openapi)));

    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/s/{token}", get(share::page))
        .route("/calendar/{file}", get(calendar::feed))
        .nest("/api/v1", api)
        .merge(crate::mcp::router(state.clone()))
        .merge(crate::oauth::router())
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(state.clone(), require_users))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf::refuse_cross_site,
        ))
        // The routes that take files (and MCP) set their own, larger limit
        // closer to the handler, which wins over this one.
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
        .layer(middleware::from_fn(upload_link_cors));
    security::apply(router).with_state(state)
}

/// The upload widget of the MCP app runs in a sandboxed frame on another
/// origin and `PUT`s the file to a one-time link. The link is the credential
/// (it is in the path, no cookie is involved), so any origin may use it.
async fn upload_link_cors(request: Request, next: Next) -> Response {
    use axum::http::{HeaderValue, Method, header};

    // The widget also loads the app's fonts from the same origin.
    if request.uri().path().starts_with("/fonts/") {
        let mut response = next.run(request).await;
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        return response;
    }
    if !request.uri().path().starts_with("/api/v1/ul/") {
        return next.run(request).await;
    }
    let preflight = request.method() == Method::OPTIONS;
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    if preflight {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("PUT, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
    }
    response
}

async fn healthz(State(state): State<HttpState>) -> ApiResult<Json<Value>> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.app.db.read)
        .await?;
    Ok(Json(json!({"status": "ok"})))
}

fn is_service_path(path: &str) -> bool {
    // The setup endpoints are how the first user comes to exist.
    if path == "/api/v1/setup" {
        return false;
    }
    path.starts_with("/api/") || path == "/mcp" || path.starts_with("/mcp/")
}

/// Until the first user exists the API refuses to serve (the health check and
/// the web page are exempt): an instance nobody can log into must not look healthy and open.
async fn require_users(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    if is_service_path(request.uri().path()) && !state.users_exist.load(Ordering::Relaxed) {
        let count: Result<i64, _> = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&state.app.db.read)
            .await;
        match count {
            Ok(n) if n > 0 => state.users_exist.store(true, Ordering::Relaxed),
            Ok(_) => {
                return ApiError(ServiceError::unavailable(
                    "no_users",
                    "no user exists yet: open the web page and enter the setup code from the \
                     server log, or run `paperjam user add --admin <name>`",
                ))
                .into_response();
            }
            Err(e) => return ApiError(e.into()).into_response(),
        }
    }
    next.run(request).await
}
