//! Response headers every reply carries.

use axum::Router;
use axum::http::{HeaderName, HeaderValue, header};
use tower_http::set_header::SetResponseHeaderLayer;

pub const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Adds the CSP, `nosniff`, a same-origin referrer policy, COOP, and a permissions policy
/// to every reply that does not already set them.
pub fn apply<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;

    #[tokio::test]
    async fn security_headers_applied_by_default() {
        let app = apply(Router::new().route("/test", get(|| async { "ok" })));
        let response = app
            .oneshot(Request::get("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["referrer-policy"], "same-origin");
        assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
        assert_eq!(
            headers["permissions-policy"],
            "camera=(), microphone=(), geolocation=()"
        );
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("base-uri 'none'"));
        assert!(csp.contains("form-action 'self'"));
    }

    #[tokio::test]
    async fn existing_headers_not_overwritten() {
        let app = apply(Router::new().route(
            "/custom",
            get(|| async {
                (
                    [
                        (header::REFERRER_POLICY, "no-referrer"),
                        (
                            HeaderName::from_static("cross-origin-opener-policy"),
                            "unsafe-none",
                        ),
                    ],
                    "custom",
                )
                    .into_response()
            }),
        ));
        let response = app
            .oneshot(Request::get("/custom").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let headers = response.headers();
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["cross-origin-opener-policy"], "unsafe-none");
        assert_eq!(headers["x-content-type-options"], "nosniff");
    }
}
