//! The authorization endpoint: a signed-in person sees who is asking and
//! decides. Nothing is granted without that click.

use axum::Form;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{Duration, Utc};
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::error::{ErrorKind, ServiceError};
use crate::hashing::sha256_hex;
use crate::http::auth::Principal;
use crate::http::{ApiError, HttpState};
use crate::oauth::{CODE_TTL, Client, find_client, no_store, purge_stale};
use crate::service::{Via, events, links};
use crate::{random, signing, time};

const CONSENT_PURPOSE: &str = "oauth-consent";
/// How long a rendered consent page stays valid.
const CONSENT_TTL: Duration = Duration::minutes(15);
const CODE_CHARS: usize = 48;
/// Sent with every page here: no scripts, inline style and this origin's fonts only, never framed.
/// No `form-action`: browsers apply it to the redirect that follows the form, which goes to the
/// client's own address.
const PAGE_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; font-src 'self'; img-src data:; \
     base-uri 'none'; frame-ancestors 'none'";

const CONSENT_TEMPLATE: &str = include_str!("consent.html");
const ERROR_TEMPLATE: &str = include_str!("error.html");
/// Shared by both pages; the fonts it names are put under `/fonts/` by the web build.
const PAGE_STYLE: &str = concat!(include_str!("../http/base.css"), include_str!("page.css"));

#[derive(Deserialize)]
pub struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
pub struct DecisionForm {
    decision: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    state: Option<String>,
    csrf: Option<String>,
}

// --- pages -------------------------------------------------------------------------

pub(crate) fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Fills `{{key}}` slots in one pass, so a value that itself contains
/// `{{...}}` is never expanded. Values must already be HTML.
pub(crate) fn render(template: &str, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 512);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        if let Some((_, value)) = values.iter().find(|(k, _)| *k == key) {
            out.push_str(value);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn page(status: StatusCode, body: String) -> Response {
    let mut response = (status, axum::response::Html(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_CSP),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    no_store(&mut response);
    response
}

/// A problem with the request itself (unknown client, wrong redirect URI):
/// shown to the person, never redirected to the address in question.
fn error_page(message: &str) -> Response {
    page(
        StatusCode::BAD_REQUEST,
        render(
            ERROR_TEMPLATE,
            &[
                ("style", PAGE_STYLE.to_string()),
                ("message", html_escape(message)),
            ],
        ),
    )
}

struct Strings {
    title: &'static str,
    /// Follows the application's name: "<name> wants to …".
    heading: &'static str,
    signed_in_as: &'static str,
    can_label: &'static str,
    perm_read: &'static str,
    perm_write: &'static str,
    perm_share: &'static str,
    perm_reprocess: &'static str,
    perm_never: &'static str,
    redirect_label: &'static str,
    /// Follows nothing: "This application registered itself on <time>. …".
    registered_note: &'static str,
    revoke_note: &'static str,
    approve: &'static str,
    deny: &'static str,
}

const EN: Strings = Strings {
    title: "Connect an application",
    heading: "wants to connect to your archive",
    signed_in_as: "Signed in as",
    can_label: "Acting as you, it will be able to",
    perm_read: "Read and search every document",
    perm_write: "Add documents and change their fields",
    perm_share: "Create public share links to documents",
    perm_reprocess: "Start reprocessing, which may send a document's text to the configured LLM unless it is local-only",
    perm_never: "Delete anything for good — that stays with you",
    redirect_label: "Then you return to",
    registered_note: "This application registered itself on {time}. Nobody has vouched for its name or its address: allow it only if you just started connecting it.",
    revoke_note: "Access lasts until you disconnect it in Settings → Connected apps.",
    approve: "Allow",
    deny: "Deny",
};

const RU: Strings = Strings {
    title: "Подключение приложения",
    heading: "просит доступ к вашему архиву",
    signed_in_as: "Вы вошли как",
    can_label: "От вашего имени оно сможет",
    perm_read: "Читать и искать любые документы",
    perm_write: "Добавлять документы и менять их поля",
    perm_share: "Создавать публичные ссылки на документы",
    perm_reprocess: "Запускать переобработку: текст документа может уйти в настроенную LLM, если он не помечен «только локально»",
    perm_never: "Удалять что-либо навсегда — это остаётся за вами",
    redirect_label: "Потом вы вернётесь на",
    registered_note: "Приложение зарегистрировалось само: {time}. Его имя и адрес никто не подтверждал: разрешайте, только если вы сами сейчас его подключаете.",
    revoke_note: "Доступ действует, пока вы не отключите его в Настройки → Подключённые приложения.",
    approve: "Разрешить",
    deny: "Отклонить",
};

/// The application's monogram on the consent page: its first letter or digit, if it has one.
fn monogram(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "•".to_string(), |c| c.to_uppercase().collect())
}

/// Where the client will be sent back to, in words a person can check.
fn redirect_label(redirect_uri: &str) -> String {
    match Url::parse(redirect_uri) {
        Ok(url) => match url.host_str() {
            Some(host) => match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            },
            None => format!("{}://", url.scheme()),
        },
        Err(_) => redirect_uri.to_string(),
    }
}

/// The "registered itself" mark for a client nobody configured, or nothing
/// for the operator's own. Returns HTML.
fn registered_note(client: &Client, strings: &Strings) -> String {
    if !client.self_registered {
        return String::new();
    }
    let when = chrono::DateTime::parse_from_rfc3339(&client.created_at).map_or_else(
        |_| client.created_at.clone(),
        |t| {
            t.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M UTC")
                .to_string()
        },
    );
    format!(
        "<p class=\"registered\">{}</p>",
        html_escape(&strings.registered_note.replace("{time}", &when))
    )
}

// --- request checks ------------------------------------------------------------------

/// The client and redirect URI, checked before anything else: until both are
/// known to belong together no redirect to the given address is safe.
/// The error is the finished response, which handlers return as it is.
#[allow(clippy::result_large_err)]
async fn load_client(
    state: &HttpState,
    client_id: Option<&str>,
    redirect_uri: Option<&str>,
) -> Result<(Client, String), Response> {
    let (Some(client_id), Some(redirect_uri)) = (client_id, redirect_uri) else {
        return Err(error_page("client_id and redirect_uri are required."));
    };
    let client = match find_client(&state.app, client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return Err(error_page("This application is not registered.")),
        Err(e) => return Err(ApiError(e).into_response()),
    };
    if !client.allows_redirect(redirect_uri) {
        return Err(error_page(
            "The redirect URI does not match the one this application registered.",
        ));
    }
    Ok((client, redirect_uri.to_string()))
}

/// A SHA-256 challenge in base64url without padding is exactly 43 characters.
fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Sends the browser back to the client with the given query parameters.
fn redirect_to_client(
    state: &HttpState,
    headers: &HeaderMap,
    redirect_uri: &str,
    params: &[(&str, &str)],
    client_state: Option<&str>,
) -> Response {
    let Ok(mut url) = Url::parse(redirect_uri) else {
        return error_page("The redirect URI is not valid.");
    };
    {
        let mut query = url.query_pairs_mut();
        for (k, v) in params {
            query.append_pair(k, v);
        }
        if let Some(client_state) = client_state {
            query.append_pair("state", client_state);
        }
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
        query.append_pair("iss", &links::base_url(&state.app, host));
    }
    let mut response = Redirect::to(url.as_str()).into_response();
    no_store(&mut response);
    response
}

fn redirect_error(
    state: &HttpState,
    headers: &HeaderMap,
    redirect_uri: &str,
    error: &str,
    description: &str,
    client_state: Option<&str>,
) -> Response {
    redirect_to_client(
        state,
        headers,
        redirect_uri,
        &[("error", error), ("error_description", description)],
        client_state,
    )
}

/// A fault in the authorization request itself. A client somebody has
/// approved gets the protocol's error redirect. One that registered itself and
/// was never approved gets a page here instead: anyone can register an address,
/// and this endpoint must not bounce a browser to it before a person has seen it.
fn request_error(
    state: &HttpState,
    headers: &HeaderMap,
    client: &Client,
    redirect_uri: &str,
    error: &str,
    description: &str,
    client_state: Option<&str>,
) -> Response {
    if client.approved {
        redirect_error(
            state,
            headers,
            redirect_uri,
            error,
            description,
            client_state,
        )
    } else {
        error_page(&format!(
            "The application sent a request that cannot be served ({description})."
        ))
    }
}

/// A signed-in web session, or the finished response that sends the person
/// to sign in first (or refuses them).
#[allow(clippy::result_large_err)]
fn session_or_login(
    principal: Result<Principal, ApiError>,
    uri: &Uri,
) -> Result<Principal, Response> {
    match principal {
        Ok(p) if p.actor.via == Via::Web => Ok(p),
        Ok(_) => Err(ApiError(ServiceError::forbidden(
            "session_required",
            "authorizing an application needs a signed-in web session",
        ))
        .into_response()),
        Err(ApiError(e)) if e.kind == ErrorKind::Unauthorized => {
            let back = uri
                .path_and_query()
                .map_or_else(|| uri.path().to_string(), |pq| pq.as_str().to_string());
            let next: String = url::form_urlencoded::byte_serialize(back.as_bytes()).collect();
            Err(Redirect::to(&format!("/login?next={next}")).into_response())
        }
        Err(e) => Err(e.into_response()),
    }
}

// --- consent token -------------------------------------------------------------------

fn consent_signature(state: &HttpState, user_id: &str, fields: [&str; 4], expires: i64) -> String {
    let data = json!([user_id, fields, expires]).to_string();
    signing::sign(
        state.app.config.secret_key.expose_secret().as_bytes(),
        CONSENT_PURPOSE,
        &data,
    )
}

/// `<expiry>.<signature>`: proves the form was rendered here, for this user
/// and exactly these parameters.
fn consent_token(state: &HttpState, user_id: &str, fields: [&str; 4]) -> String {
    let expires = (Utc::now() + CONSENT_TTL).timestamp();
    format!(
        "{expires}.{}",
        consent_signature(state, user_id, fields, expires)
    )
}

fn consent_token_valid(state: &HttpState, user_id: &str, fields: [&str; 4], token: &str) -> bool {
    let Some((expires, signature)) = token.split_once('.') else {
        return false;
    };
    let Ok(expires) = expires.parse::<i64>() else {
        return false;
    };
    expires >= Utc::now().timestamp()
        && signing::verify(
            state.app.config.secret_key.expose_secret().as_bytes(),
            CONSENT_PURPOSE,
            &json!([user_id, fields, expires]).to_string(),
            signature,
        )
}

// --- GET: ask --------------------------------------------------------------------------

/// `GET /oauth/authorize`
pub async fn show(
    State(state): State<HttpState>,
    principal: Result<Principal, ApiError>,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    let (client, redirect_uri) = match load_client(
        &state,
        params.client_id.as_deref(),
        params.redirect_uri.as_deref(),
    )
    .await
    {
        Ok(found) => found,
        Err(response) => return response,
    };
    let client_state = params.state.as_deref();
    if params.response_type.as_deref() != Some("code") {
        return request_error(
            &state,
            &headers,
            &client,
            &redirect_uri,
            "unsupported_response_type",
            "response_type must be code",
            client_state,
        );
    }
    let challenge = params.code_challenge.as_deref().unwrap_or("");
    if params.code_challenge_method.as_deref() != Some("S256") || !valid_challenge(challenge) {
        return request_error(
            &state,
            &headers,
            &client,
            &redirect_uri,
            "invalid_request",
            "PKCE with code_challenge_method=S256 is required",
            client_state,
        );
    }
    let principal = match session_or_login(principal, &uri) {
        Ok(p) => p,
        Err(response) => return response,
    };
    let user = &principal.user;

    let user_id = user.id.to_string();
    let fields = [
        client.id.as_str(),
        redirect_uri.as_str(),
        challenge,
        client_state.unwrap_or(""),
    ];
    let csrf = consent_token(&state, &user_id, fields);
    let mut hidden = String::new();
    for (name, value) in [
        ("client_id", fields[0]),
        ("redirect_uri", fields[1]),
        ("code_challenge", fields[2]),
        ("state", fields[3]),
        ("csrf", csrf.as_str()),
    ] {
        hidden.push_str(&format!(
            "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
            html_escape(value)
        ));
    }
    let strings = if user.locale == "ru" { &RU } else { &EN };
    let body = render(
        CONSENT_TEMPLATE,
        &[
            ("lang", html_escape(&user.locale)),
            ("style", PAGE_STYLE.to_string()),
            ("title", html_escape(strings.title)),
            ("initial", html_escape(&monogram(&client.name))),
            ("client", html_escape(&client.name)),
            ("heading", html_escape(strings.heading)),
            ("signed_in_as", html_escape(strings.signed_in_as)),
            ("display_name", html_escape(&user.display_name)),
            ("username", html_escape(&user.username)),
            ("can_label", html_escape(strings.can_label)),
            ("perm_read", html_escape(strings.perm_read)),
            ("perm_write", html_escape(strings.perm_write)),
            ("perm_share", html_escape(strings.perm_share)),
            ("perm_reprocess", html_escape(strings.perm_reprocess)),
            ("perm_never", html_escape(strings.perm_never)),
            ("redirect_label", html_escape(strings.redirect_label)),
            ("redirect_host", html_escape(&redirect_label(&redirect_uri))),
            ("redirect_uri", html_escape(&redirect_uri)),
            ("registered_note", registered_note(&client, strings)),
            ("revoke_note", html_escape(strings.revoke_note)),
            ("approve", html_escape(strings.approve)),
            ("deny", html_escape(strings.deny)),
            ("fields", hidden),
        ],
    );
    page(StatusCode::OK, body)
}

// --- POST: decide ----------------------------------------------------------------------

/// `POST /oauth/authorize`
pub async fn decide(
    State(state): State<HttpState>,
    principal: Result<Principal, ApiError>,
    uri: Uri,
    headers: HeaderMap,
    Form(form): Form<DecisionForm>,
) -> Response {
    let (client, redirect_uri) = match load_client(
        &state,
        form.client_id.as_deref(),
        form.redirect_uri.as_deref(),
    )
    .await
    {
        Ok(found) => found,
        Err(response) => return response,
    };
    let principal = match session_or_login(principal, &uri) {
        Ok(p) => p,
        Err(response) => return response,
    };
    let challenge = form.code_challenge.as_deref().unwrap_or("");
    let client_state = form.state.as_deref().filter(|s| !s.is_empty());
    let fields = [
        client.id.as_str(),
        redirect_uri.as_str(),
        challenge,
        client_state.unwrap_or(""),
    ];
    let user_id = principal.user.id.to_string();
    if !consent_token_valid(&state, &user_id, fields, form.csrf.as_deref().unwrap_or("")) {
        return error_page(
            "This page has expired or was not issued to you. Start again from the application.",
        );
    }
    if !valid_challenge(challenge) {
        return error_page("The request has no valid PKCE challenge.");
    }

    if form.decision.as_deref() != Some("approve") {
        return redirect_error(
            &state,
            &headers,
            &redirect_uri,
            "access_denied",
            "the user denied the request",
            client_state,
        );
    }
    let code = match issue_code(&state, &client, &principal, &redirect_uri, challenge).await {
        Ok(code) => code,
        Err(e) => return ApiError(e).into_response(),
    };
    redirect_to_client(
        &state,
        &headers,
        &redirect_uri,
        &[("code", &code)],
        client_state,
    )
}

/// Records the approval and the grant, and mints the one-time code for it.
async fn issue_code(
    state: &HttpState,
    client: &Client,
    principal: &Principal,
    redirect_uri: &str,
    challenge: &str,
) -> crate::error::Result<String> {
    let app = &state.app;
    purge_stale(app).await?;
    let code = random::base62(CODE_CHARS);
    let grant_id = uuid::Uuid::now_v7().to_string();
    let now = time::now_ts();
    let mut tx = app.db.write.begin().await?;
    sqlx::query("UPDATE oauth_clients SET approved_by = ? WHERE id = ? AND approved_by IS NULL")
        .bind(principal.user.id)
        .bind(&client.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO oauth_grants (id, client_id, user_id, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&grant_id)
    .bind(&client.id)
    .bind(principal.user.id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO oauth_codes (code_hash, grant_id, redirect_uri, code_challenge, expires_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(sha256_hex(code.as_bytes()))
    .bind(&grant_id)
    .bind(redirect_uri)
    .bind(challenge)
    .bind(time::ts(Utc::now() + CODE_TTL))
    .execute(&mut *tx)
    .await?;
    events::record(
        &mut tx,
        &principal.actor,
        "oauth.grant.create",
        None,
        json!({"client": client.name}),
    )
    .await?;
    tx.commit().await?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_lets_the_page_draw_its_own_style() {
        // The paper grain is an inline SVG image; without `data:` images the
        // policy blocks it.
        assert!(PAGE_STYLE.contains("url(\"data:image/svg+xml"));
        assert!(PAGE_CSP.contains("img-src data:"));
    }

    #[test]
    fn render_fills_slots_once() {
        let out = render(
            "<p>{{a}} {{b}} {{missing}}</p>",
            &[("a", "{{b}}".to_string()), ("b", "x".to_string())],
        );
        assert_eq!(out, "<p>{{b}} x </p>");
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            html_escape("<b onload=\"x\">&'"),
            "&lt;b onload=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn monogram_is_the_first_letter_or_digit() {
        assert_eq!(monogram("claude"), "C");
        assert_eq!(monogram("  <ёлка>"), "Ё");
        assert_eq!(monogram("9to5"), "9");
        assert_eq!(monogram("<>!"), "•");
        assert_eq!(monogram(""), "•");
    }

    fn slots(template: &str) -> std::collections::BTreeSet<&str> {
        template
            .split("{{")
            .skip(1)
            .filter_map(|rest| rest.split_once("}}").map(|(key, _)| key))
            .collect()
    }

    /// The templates ask for exactly the slots the handlers fill: none renders empty, none is
    /// filled for nothing.
    #[test]
    fn templates_use_the_filled_slots() {
        let consent = [
            "lang",
            "style",
            "title",
            "initial",
            "client",
            "heading",
            "signed_in_as",
            "display_name",
            "username",
            "can_label",
            "perm_read",
            "perm_write",
            "perm_share",
            "perm_reprocess",
            "perm_never",
            "redirect_label",
            "redirect_host",
            "redirect_uri",
            "registered_note",
            "revoke_note",
            "approve",
            "deny",
            "fields",
        ];
        assert_eq!(slots(CONSENT_TEMPLATE), consent.into_iter().collect());
        assert_eq!(
            slots(ERROR_TEMPLATE),
            ["style", "message"].into_iter().collect()
        );
        assert!(slots(PAGE_STYLE).is_empty());
    }

    /// The consent page lists what a connected application can do beyond reading and editing
    /// fields, in both languages: public share links, and reprocessing that can reach the LLM.
    #[test]
    fn the_consent_text_names_sharing_and_reprocessing() {
        for (strings, share, reprocess, local) in [
            (&EN, "public share links", "reprocessing", "local-only"),
            (&RU, "публичные ссылки", "переобработку", "только локально"),
        ] {
            assert!(strings.perm_share.contains(share), "{}", strings.perm_share);
            assert!(
                strings.perm_reprocess.contains(reprocess),
                "{}",
                strings.perm_reprocess
            );
            assert!(
                strings.perm_reprocess.contains(local),
                "{}",
                strings.perm_reprocess
            );
            assert!(strings.perm_reprocess.contains("LLM"));
        }
    }

    fn client(self_registered: bool) -> Client {
        Client {
            id: "pjc_x".into(),
            name: "<b>Claude</b>".into(),
            redirect_uris: vec![],
            secret_hash: None,
            approved: false,
            self_registered,
            created_at: "2026-11-11T14:02:33.123Z".into(),
        }
    }

    #[test]
    fn a_self_registered_client_is_marked_with_when_it_appeared() {
        for strings in [&EN, &RU] {
            let note = registered_note(&client(true), strings);
            assert!(note.starts_with("<p class=\"registered\">"), "{note}");
            assert!(note.contains("2026-11-11 14:02 UTC"), "{note}");
            assert!(!note.contains("{time}"), "{note}");
        }
        // The operator's own client carries no such mark.
        assert_eq!(registered_note(&client(false), &EN), "");
    }

    #[test]
    fn challenge_shape() {
        assert!(valid_challenge(&"a".repeat(43)));
        assert!(!valid_challenge(&"a".repeat(42)));
        assert!(!valid_challenge(&format!("{}=", "a".repeat(42))));
    }

    #[test]
    fn redirect_label_names_the_host_or_scheme() {
        assert_eq!(
            redirect_label("https://claude.ai/api/mcp/auth_callback"),
            "claude.ai"
        );
        assert_eq!(redirect_label("http://localhost:3118/cb"), "localhost:3118");
        assert_eq!(redirect_label("cursor:///oauth/cb"), "cursor://");
    }
}
