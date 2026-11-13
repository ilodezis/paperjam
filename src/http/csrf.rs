//! Cross-site request refusal for cookie sessions.
//!
//! `SameSite=Lax` treats every subdomain of the same registrable domain as one
//! site, so a page on a neighbouring subdomain could send "simple" requests
//! (a multipart upload, a bodiless `POST`) with the session cookie. A request
//! that changes something and carries the session cookie must therefore come
//! from paperjam itself: `Sec-Fetch-Site` is `same-origin` (or `none`, typed
//! by the user), or, from browsers that do not send it, `Origin` is ours.
//! Requests without a cookie (bearer tokens, MCP, one-time upload links) are
//! not affected.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;

use crate::error::ServiceError;
use crate::http::auth::{INSECURE_SESSION_COOKIE, SESSION_COOKIE};
use crate::http::{ApiError, HttpState};

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn origin_of(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

/// Whether a cookie-carrying, state-changing request comes from paperjam.
fn same_origin(headers: &HeaderMap, public_url: Option<&url::Url>) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return matches!(site.to_str(), Ok("same-origin" | "none"));
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        // Neither header: not a browser that could be tricked cross-site.
        return true;
    };
    if let Some(url) = public_url {
        return origin == origin_of(url);
    }
    // No public URL configured: compare with the Host the request was sent to.
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    url::Url::parse(origin)
        .ok()
        .and_then(|o| {
            let port = o.port().map(|p| format!(":{p}")).unwrap_or_default();
            o.host_str().map(|h| format!("{h}{port}"))
        })
        .is_some_and(|o| Some(o.as_str()) == host)
}

pub async fn refuse_cross_site(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let has_session = {
        let jar = CookieJar::from_headers(headers);
        jar.get(SESSION_COOKIE).is_some() || jar.get(INSECURE_SESSION_COOKIE).is_some()
    };
    if is_safe(request.method())
        || !has_session
        || headers.contains_key(header::AUTHORIZATION)
        || same_origin(headers, state.app.config.public_url.as_ref())
    {
        return next.run(request).await;
    }
    ApiError(ServiceError::forbidden(
        "cross_site_request",
        "this request did not come from paperjam",
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(*k, v.parse().unwrap());
        }
        map
    }

    #[test]
    fn fetch_metadata_decides_when_present() {
        let url = url::Url::parse("https://docs.example.com").unwrap();
        let u = Some(&url);
        assert!(same_origin(
            &headers(&[("sec-fetch-site", "same-origin")]),
            u
        ));
        assert!(same_origin(&headers(&[("sec-fetch-site", "none")]), u));
        assert!(!same_origin(
            &headers(&[("sec-fetch-site", "same-site")]),
            u
        ));
        assert!(!same_origin(
            &headers(&[("sec-fetch-site", "cross-site")]),
            u
        ));
        // A matching Origin does not rescue a same-site fetch.
        assert!(!same_origin(
            &headers(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://docs.example.com")
            ]),
            u
        ));
    }

    #[test]
    fn origin_is_compared_without_fetch_metadata() {
        let url = url::Url::parse("https://docs.example.com/").unwrap();
        let u = Some(&url);
        assert!(same_origin(
            &headers(&[("origin", "https://docs.example.com")]),
            u
        ));
        assert!(!same_origin(
            &headers(&[("origin", "https://evil.example.com")]),
            u
        ));
        assert!(same_origin(&headers(&[]), u));
        // Without a public URL the Host header is the reference.
        assert!(same_origin(
            &headers(&[
                ("origin", "http://localhost:8140"),
                ("host", "localhost:8140")
            ]),
            None
        ));
        assert!(!same_origin(
            &headers(&[("origin", "http://other:8140"), ("host", "localhost:8140")]),
            None
        ));
    }
}
