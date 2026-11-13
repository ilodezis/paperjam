//! Who is calling: session cookie or bearer token, plus the login endpoints.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use webauthn_rs::prelude::PublicKeyCredential;

use crate::error::{ErrorKind, ServiceError};
use crate::http::{ApiError, ApiJson, ApiResult, HttpState};
use crate::model::user::{SessionToken, User};
use crate::service::{Actor, Ctx, Via, passkeys, tokens, users};

/// The session cookie over HTTPS. The `__Host-` prefix makes the browser
/// refuse it unless it is `Secure`, host-only and for `/`, so a neighbouring
/// subdomain can neither read nor plant it.
pub const SESSION_COOKIE: &str = "__Host-pj_session";
/// The same cookie on a plain-HTTP local setup, where `__Host-` cannot work.
pub const INSECURE_SESSION_COOKIE: &str = "pj_session";

/// The session cookie name for this server.
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure {
        SESSION_COOKIE
    } else {
        INSECURE_SESSION_COOKIE
    }
}

/// Who is calling and through which door.
#[derive(Debug, Clone)]
pub struct Principal {
    pub actor: Actor,
    pub user: User,
}

impl Principal {
    pub fn require_admin(&self) -> Result<(), ServiceError> {
        if self.user.is_admin {
            Ok(())
        } else {
            Err(ServiceError::forbidden(
                "admin_required",
                "this action is for administrators",
            ))
        }
    }

    /// Purging is a web-UI action: agents and tokens never get it.
    pub fn require_session(&self) -> Result<(), ServiceError> {
        if self.actor.via == Via::Web {
            Ok(())
        } else {
            Err(ServiceError::forbidden(
                "purge_requires_session",
                "this action needs a signed-in web session",
            ))
        }
    }
}

impl FromRequestParts<HttpState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &HttpState,
    ) -> Result<Self, Self::Rejection> {
        let app = &*state.app;
        if let Some(header) = parts.headers.get(AUTHORIZATION) {
            let raw = header
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(|| {
                    ServiceError::unauthorized("invalid_token", "malformed Authorization header")
                })?;
            let actor = tokens::resolve_token(app, raw.trim()).await?;
            let user = users::get_user(&app.ctx(actor), actor.require_user()?).await?;
            if user.disabled_at.is_some() {
                return Err(
                    ServiceError::unauthorized("invalid_token", "the account is disabled").into(),
                );
            }
            return Ok(Principal { actor, user });
        }
        let jar = CookieJar::from_headers(&parts.headers);
        let cookie = jar
            .get(session_cookie_name(state.secure_cookies))
            .ok_or_else(|| {
                ServiceError::unauthorized("authentication_required", "sign in first")
            })?;
        let user = users::resolve_session(app, &SessionToken(cookie.value().to_string())).await?;
        Ok(Principal {
            actor: Actor::user(user.id, Via::Web),
            user,
        })
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

fn user_json(user: &User) -> Value {
    json!({
        "id": user.id,
        "username": user.username,
        "display_name": user.display_name,
        "locale": user.locale,
        "person_id": user.person_id,
        "is_admin": user.is_admin,
    })
}

fn session_cookie(value: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(session_cookie_name(secure), value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_path("/");
    cookie.set_max_age(::time::Duration::days(users::SESSION_LIFETIME_DAYS));
    cookie
}

/// Sign in with a username and password; sets the `pj_session` cookie.
#[utoipa::path(
    post,
    path = "/auth/login",
    tag = "auth",
    summary = "Sign in with a username and password; sets the pj_session cookie.",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Signed in successfully", body = serde_json::Value),
        (status = 401, description = "Invalid credentials", body = crate::http::ErrorResponse),
        (status = 413, description = "Request body too large", body = crate::http::ErrorResponse),
        (status = 422, description = "Malformed request body, or a username over 64 characters or a password over 1024 bytes", body = crate::http::ErrorResponse),
        (status = 429, description = "Too many failed attempts", body = crate::http::ErrorResponse),
    )
)]
pub async fn login(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(body): ApiJson<LoginBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    // Before the limiter: an oversized name must never reach the limiter.
    users::check_login_lengths(&body.username, &body.password)?;
    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_password(ip, &body.username)?;
    let ctx = state.app.ctx(Actor {
        user_id: None,
        via: Via::Web,
    });
    let user = match users::verify_password(&ctx, &body.username, &body.password).await {
        Ok(user) => user,
        Err(e) => {
            if e.code == "invalid_credentials" {
                state.limiter.password_failed(ip, &body.username);
            }
            return Err(e.into());
        }
    };
    state.limiter.password_succeeded(ip, &body.username);
    signed_in(&state, &ctx, jar, &user).await
}

/// Whether the instance still has to be set up (no user exists yet).
#[utoipa::path(
    get,
    path = "/setup",
    tag = "auth",
    summary = "Whether the first administrator still has to be created.",
    responses(
        (status = 200, description = "`{\"needed\": true}` until the first user exists", body = serde_json::Value),
    )
)]
pub async fn setup_status(State(state): State<HttpState>) -> ApiResult<Json<Value>> {
    let needed = crate::service::setup::is_needed(&state.app).await?;
    Ok(Json(json!({"needed": needed})))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SetupBody {
    /// The setup code the server prints to its log at start-up.
    pub code: String,
    pub username: String,
    pub password: String,
    /// Shown in the interface; defaults to the username.
    pub display_name: Option<String>,
    /// Interface language, `en` or `ru`; defaults to `en`.
    pub locale: Option<String>,
}

/// Creates the first administrator and signs them in. Works once: after the
/// first user exists it answers 409.
#[utoipa::path(
    post,
    path = "/setup",
    tag = "auth",
    summary = "Create the first administrator with the setup code from the server log; sets the pj_session cookie.",
    request_body = SetupBody,
    responses(
        (status = 200, description = "The administrator was created and signed in", body = serde_json::Value),
        (status = 409, description = "The instance already has a user", body = crate::http::ErrorResponse),
        (status = 413, description = "Request body too large", body = crate::http::ErrorResponse),
        (status = 422, description = "Wrong setup code, or an invalid username or password", body = crate::http::ErrorResponse),
        (status = 429, description = "Too many wrong codes", body = crate::http::ErrorResponse),
    )
)]
pub async fn setup(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(body): ApiJson<SetupBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    users::check_login_lengths(&body.username, &body.password)?;
    if !crate::service::setup::is_needed(&state.app).await? {
        return Err(
            ServiceError::conflict("already_set_up", "this instance already has a user").into(),
        );
    }
    // Wrong codes count against the address, like wrong passwords do.
    const LIMITER_NAME: &str = "setup code";
    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_password(ip, LIMITER_NAME)?;
    let created = crate::service::setup::create_first_admin(
        &state.app,
        crate::service::setup::FirstAdmin {
            code: body.code,
            display_name: body.display_name.unwrap_or_default(),
            username: body.username,
            password: SecretString::from(body.password),
            locale: body.locale.unwrap_or_else(|| "en".to_string()),
        },
    )
    .await;
    let user = match created {
        Ok(user) => user,
        Err(e) => {
            if e.code == "invalid_setup_code" {
                state.limiter.password_failed(ip, LIMITER_NAME);
            }
            return Err(e.into());
        }
    };
    state.limiter.password_succeeded(ip, LIMITER_NAME);
    let ctx = state.app.ctx(Actor {
        user_id: None,
        via: Via::Web,
    });
    signed_in(&state, &ctx, jar, &user).await
}

/// Opens the web session of a verified user: the one way every sign-in
/// (password or passkey) ends.
pub(crate) async fn signed_in(
    state: &HttpState,
    ctx: &Ctx<'_>,
    jar: CookieJar,
    user: &User,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let token = users::create_session(ctx, user.id).await?;
    let jar = jar.add(session_cookie(token.0, state.secure_cookies));
    Ok((jar, Json(json!({"user": user_json(user)}))))
}

/// Ends the current session. Always succeeds.
#[utoipa::path(
    post,
    path = "/auth/logout",
    tag = "auth",
    summary = "Ends the current session. Always succeeds.",
    security(
        ("session_cookie" = [])
    ),
    responses(
        (status = 204, description = "Session ended"),
    )
)]
pub async fn logout(
    State(state): State<HttpState>,
    jar: CookieJar,
) -> ApiResult<(CookieJar, StatusCode)> {
    let name = session_cookie_name(state.secure_cookies);
    if let Some(cookie) = jar.get(name) {
        let ctx = state.app.ctx(Actor::system());
        // An invalid or expired cookie is simply cleared.
        let _ = users::delete_session(&ctx, &SessionToken(cookie.value().to_string())).await;
    }
    let jar = jar.remove(Cookie::build(name).path("/"));
    Ok((jar, StatusCode::NO_CONTENT))
}

/// The signed-in user (cookie session or bearer token).
#[utoipa::path(
    get,
    path = "/auth/session",
    tag = "auth",
    summary = "The signed-in user (cookie session or bearer token).",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Current signed-in user", body = serde_json::Value),
        (status = 401, description = "Not authenticated", body = crate::http::ErrorResponse),
    )
)]
pub async fn session(principal: Principal) -> Json<Value> {
    Json(json!({"user": user_json(&principal.user), "via": principal.actor.via.as_str()}))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SessionPatch {
    /// `en` or `ru`.
    pub locale: String,
}

/// Change the signed-in user's own settings: the account language.
#[utoipa::path(
    patch,
    path = "/auth/session",
    tag = "auth",
    summary = "Change the signed-in user's own settings: the account language.",
    description = "The account language is the interface default and the language titles and summaries are written in for the documents this user adds.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = SessionPatch,
    responses(
        (status = 200, description = "The updated signed-in user", body = serde_json::Value),
        (status = 401, description = "Not authenticated", body = crate::http::ErrorResponse),
        (status = 422, description = "Unsupported locale", body = crate::http::ErrorResponse),
    )
)]
pub async fn update_session(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(body): ApiJson<SessionPatch>,
) -> ApiResult<Json<Value>> {
    let user = users::set_locale(
        &state.app.ctx(principal.actor),
        principal.user.id,
        &body.locale,
    )
    .await?;
    Ok(Json(
        json!({"user": user_json(&user), "via": principal.actor.via.as_str()}),
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ChangePasswordBody {
    pub current_password: String,
    pub new_password: String,
}

/// Change the signed-in user's own password. Needs a web session.
#[utoipa::path(
    post,
    path = "/auth/password",
    tag = "auth",
    summary = "Change the signed-in user's own password.",
    description = "Needs a signed-in web session: API tokens and OAuth clients are refused. Ends every other web session of this user while keeping the current session alive.",
    security(
        ("session_cookie" = [])
    ),
    request_body = ChangePasswordBody,
    responses(
        (status = 204, description = "Password updated"),
        (status = 401, description = "Not authenticated", body = crate::http::ErrorResponse),
        (status = 403, description = "Needs a web session, or current password is wrong", body = crate::http::ErrorResponse),
        (status = 422, description = "Weak or invalid new password", body = crate::http::ErrorResponse),
        (status = 429, description = "Too many failed attempts", body = crate::http::ErrorResponse),
    )
)]
pub async fn change_password(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    principal: Principal,
    ApiJson(body): ApiJson<ChangePasswordBody>,
) -> ApiResult<StatusCode> {
    principal.actor.require_session_or_cli()?;
    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_password(ip, &principal.user.username)?;
    let keep = jar
        .get(session_cookie_name(state.secure_cookies))
        .map(|cookie| SessionToken(cookie.value().to_string()));
    let ctx = state.app.ctx(principal.actor);
    match users::change_password(
        &ctx,
        principal.user.id,
        &body.current_password,
        SecretString::from(body.new_password),
        keep.as_ref(),
    )
    .await
    {
        Ok(()) => {
            state
                .limiter
                .password_succeeded(ip, &principal.user.username);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(err) => {
            if err.code == "wrong_password" {
                state.limiter.password_failed(ip, &principal.user.username);
            }
            Err(err.into())
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PasskeyChallengeBody {
    /// Optional: limits the sign-in to this account's passkeys. Without it the
    /// browser offers any paperjam passkey it holds.
    #[serde(default)]
    pub username: Option<String>,
}

/// Starts a passkey sign-in: WebAuthn request options and a ceremony id.
#[utoipa::path(
    post,
    path = "/auth/passkey/challenge",
    tag = "auth",
    summary = "Starts a passkey sign-in: WebAuthn request options and a ceremony id.",
    request_body = PasskeyChallengeBody,
    responses(
        (status = 200, description = "Ceremony id and `publicKey` options for navigator.credentials.get()", body = serde_json::Value),
        (status = 429, description = "Too many failed attempts", body = crate::http::ErrorResponse),
        (status = 503, description = "Passkeys are not configured (PAPERJAM_PUBLIC_URL)", body = crate::http::ErrorResponse),
    )
)]
pub async fn passkey_challenge(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PasskeyChallengeBody>,
) -> ApiResult<Json<passkeys::Challenge>> {
    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_passkey(ip)?;
    state.limiter.allow_challenge(ip)?;
    let ctx = state.app.ctx(Actor {
        user_id: None,
        via: Via::Web,
    });
    Ok(Json(
        passkeys::begin_login(&ctx, body.username.as_deref()).await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PasskeyLoginBody {
    /// The id from `/auth/passkey/challenge`.
    pub ceremony: String,
    /// The `PublicKeyCredential` from navigator.credentials.get(), as JSON.
    #[schema(value_type = Object)]
    pub credential: PublicKeyCredential,
}

/// Signs in with a passkey; sets the same pj_session cookie as the password login.
#[utoipa::path(
    post,
    path = "/auth/passkey",
    tag = "auth",
    summary = "Signs in with a passkey; sets the same pj_session cookie as the password login.",
    request_body = PasskeyLoginBody,
    responses(
        (status = 200, description = "Signed in successfully", body = serde_json::Value),
        (status = 401, description = "Unknown or unverifiable passkey", body = crate::http::ErrorResponse),
        (status = 422, description = "Expired or reused ceremony, or a malformed body", body = crate::http::ErrorResponse),
        (status = 429, description = "Too many failed attempts", body = crate::http::ErrorResponse),
        (status = 503, description = "Passkeys are not configured (PAPERJAM_PUBLIC_URL)", body = crate::http::ErrorResponse),
    )
)]
pub async fn passkey_login(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    ApiJson(body): ApiJson<PasskeyLoginBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let ip = state.client_addr.resolve(peer, &headers);
    state.limiter.check_passkey(ip)?;
    let ctx = state.app.ctx(Actor {
        user_id: None,
        via: Via::Web,
    });
    let user = match passkeys::finish_login(&ctx, &body.ceremony, &body.credential).await {
        Ok(user) => user,
        Err(e) => {
            if e.kind == ErrorKind::Unauthorized {
                state.limiter.passkey_failed(ip);
            }
            return Err(e.into());
        }
    };
    state.limiter.passkey_succeeded(ip);
    signed_in(&state, &ctx, jar, &user).await
}
