//! The dogfood web page, embedded in the binary.
//!
//! `web/dist` is optional at build time: without it (for example in the Rust
//! gate, which does not need Node) a placeholder page is served instead.
//!
//! `build.rs` embeds the text assets gzipped (a third of the size, in the
//! binary and on the wire). A client that accepts gzip gets them as they are;
//! the rare one that does not gets them inflated.

use std::borrow::Cow;
use std::io::Read as _;

use axum::body::Body;
use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, VARY,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use super::compressible::is_compressible;

// `ASSETS`, sorted by path: what `build.rs` found in `web/dist`, text assets
// gzipped under their own names. Empty when the web build is absent.
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

fn asset(path: &str) -> Option<&'static [u8]> {
    ASSETS
        .binary_search_by_key(&path, |(name, _)| name)
        .ok()
        .map(|i| ASSETS[i].1)
}

/// The media type of an asset, by its extension.
fn mimetype(path: &str) -> &'static str {
    match path.rsplit_once('.').map_or("", |(_, ext)| ext) {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

const PLACEHOLDER: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>paperjam</title></head><body><h1>paperjam</h1>\
<p>The web interface is not built into this binary. The API is at <code>/api/v1</code>.</p>\
</body></html>";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// True when `accept_encoding` (an `Accept-Encoding` value) allows gzip: it
/// names `gzip` or `*` without a zero quality.
fn accepts_gzip(accept_encoding: Option<&HeaderValue>) -> bool {
    let Some(value) = accept_encoding.and_then(|v| v.to_str().ok()) else {
        return false;
    };
    value.split(',').any(|item| {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        if !(coding.eq_ignore_ascii_case("gzip") || coding == "*") {
            return false;
        }
        parts
            .filter_map(|p| p.trim().strip_prefix("q="))
            .all(|q| q.trim().parse::<f32>().is_ok_and(|q| q > 0.0))
    })
}

/// A response with `stored` as its body. `gzipped` says the bytes are gzip;
/// they go out as they are when `gzip_ok`, inflated otherwise.
fn deliver(
    stored: Cow<'static, [u8]>,
    gzipped: bool,
    mimetype: &str,
    cache: &'static str,
    gzip_ok: bool,
) -> Response {
    let (bytes, encoded) = match (gzipped, gzip_ok) {
        (false, _) => (stored, false),
        (true, true) => (stored, true),
        (true, false) => {
            let mut inflated = Vec::new();
            if flate2::read::GzDecoder::new(stored.as_ref())
                .read_to_end(&mut inflated)
                .is_err()
            {
                tracing::error!("an embedded web asset is not valid gzip");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            (Cow::Owned(inflated), false)
        }
    };
    let length = bytes.len();
    let body = match bytes {
        // Embedded bytes are served without a copy.
        Cow::Borrowed(bytes) => Body::from(axum::body::Bytes::from_static(bytes)),
        Cow::Owned(bytes) => Body::from(bytes),
    };
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mimetype)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
    if gzipped {
        // The same URL answers differently by what the client accepts.
        headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    if encoded {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    response
}

fn file(path: &str, cache: &'static str, gzip_ok: bool) -> Option<Response> {
    // Stored under their own names, gzipped or not: the type is the file's.
    Some(deliver(
        Cow::Borrowed(asset(path)?),
        is_compressible(path),
        mimetype(path),
        cache,
        gzip_ok,
    ))
}

fn index(gzip_ok: bool) -> Response {
    file("index.html", "no-cache", gzip_ok).unwrap_or_else(|| {
        let mut response = Response::new(Body::from(PLACEHOLDER));
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
    })
}

/// Everything the API does not claim: build output, then the single-page
/// app's entry for client-side routes.
pub async fn serve(headers: HeaderMap, uri: Uri) -> Response {
    let gzip_ok = accepts_gzip(headers.get(ACCEPT_ENCODING));
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path == "healthz" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.is_empty() {
        return index(gzip_ok);
    }
    if path.starts_with("assets/") {
        // Hashed file names: cache for good, and a miss is a real 404.
        return file(path, IMMUTABLE, gzip_ok)
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    }
    if let Some(response) = file(path, "no-cache", gzip_ok) {
        return response;
    }
    // A path with an extension is a missing file, not an app route.
    if path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    index(gzip_ok)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use axum::body::to_bytes;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    const TEXT: &str = "function paperjam() { return 'hello hello hello hello'; }";

    fn gzip(text: &str) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    async fn body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn a_client_that_accepts_gzip_gets_the_stored_bytes() {
        let stored = gzip(TEXT);
        let response = deliver(
            Cow::Owned(stored.clone()),
            true,
            "text/javascript",
            "no-cache",
            true,
        );
        let headers = response.headers();
        assert_eq!(headers[CONTENT_ENCODING], "gzip");
        assert_eq!(headers[VARY], "Accept-Encoding");
        assert_eq!(headers[CONTENT_TYPE], "text/javascript");
        assert_eq!(headers[CONTENT_LENGTH], stored.len().to_string().as_str());
        assert_eq!(body(response).await, stored);
    }

    #[tokio::test]
    async fn a_client_without_accept_encoding_gets_the_inflated_text() {
        let response = deliver(
            Cow::Owned(gzip(TEXT)),
            true,
            "text/javascript",
            "no-cache",
            false,
        );
        let headers = response.headers();
        assert!(headers.get(CONTENT_ENCODING).is_none());
        assert_eq!(headers[VARY], "Accept-Encoding");
        assert_eq!(headers[CONTENT_LENGTH], TEXT.len().to_string().as_str());
        assert_eq!(body(response).await, TEXT.as_bytes());
    }

    #[tokio::test]
    async fn assets_that_are_not_text_are_sent_as_they_are_to_everyone() {
        for gzip_ok in [true, false] {
            let response = deliver(
                Cow::Borrowed(b"wOF2 font bytes"),
                false,
                "font/woff2",
                IMMUTABLE,
                gzip_ok,
            );
            assert!(response.headers().get(CONTENT_ENCODING).is_none());
            assert!(response.headers().get(VARY).is_none());
            assert_eq!(body(response).await, b"wOF2 font bytes");
        }
    }

    #[test]
    fn a_stored_asset_that_is_not_gzip_is_an_error_not_garbage() {
        let response = deliver(Cow::Borrowed(b"plain"), true, "text/css", "no-cache", false);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn the_embedded_table_is_sorted_and_found_by_path() {
        assert!(ASSETS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (name, bytes) in ASSETS {
            assert_eq!(asset(name), Some(*bytes));
        }
        assert_eq!(asset("no/such/file.js"), None);
    }

    #[test]
    fn media_types_follow_the_extension() {
        assert_eq!(
            mimetype("assets/index-x.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mimetype("fonts/a.woff2"), "font/woff2");
        assert_eq!(
            mimetype("manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(mimetype("blob"), "application/octet-stream");
    }

    #[test]
    fn accept_encoding_is_read_for_gzip() {
        let accepts = |v: &str| accepts_gzip(Some(&HeaderValue::from_str(v).unwrap()));
        assert!(accepts("gzip"));
        assert!(accepts("gzip, deflate, br"));
        assert!(accepts("br;q=1.0, gzip;q=0.8"));
        assert!(accepts("GZIP"));
        assert!(accepts("*"));
        assert!(!accepts("identity"));
        assert!(!accepts("br, deflate"));
        assert!(!accepts("gzip;q=0"));
        assert!(!accepts("gzip;q=0.0, br"));
        assert!(!accepts("gzipped"));
        assert!(!accepts_gzip(None));
    }

    #[tokio::test]
    async fn the_entry_page_answers_both_kinds_of_client() {
        // Whether or not the web build is embedded, both get the same page.
        let plain = serve(HeaderMap::new(), Uri::from_static("/")).await;
        let plain_headers = plain.headers().clone();
        assert!(plain_headers.get(CONTENT_ENCODING).is_none());
        let page = String::from_utf8(body(plain).await).unwrap();
        assert!(page.starts_with("<!doctype html>"), "{page:.80}");

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let packed = serve(headers, Uri::from_static("/")).await;
        let encoded = packed.headers().get(CONTENT_ENCODING).is_some();
        let bytes = body(packed).await;
        let text = if encoded {
            let mut text = String::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_string(&mut text)
                .unwrap();
            text
        } else {
            String::from_utf8(bytes).unwrap()
        };
        assert_eq!(text, page);
    }
}
