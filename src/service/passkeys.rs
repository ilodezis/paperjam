//! Passkeys (WebAuthn): signing in to the web UI with Face ID, Touch ID, a
//! phone or a security key instead of typing the password.
//!
//! A passkey is a sign-in of its own, not a second step: the authenticator
//! proves possession of the key and checks the owner's face, fingerprint or
//! PIN before it signs (user verification is required on every ceremony). The
//! password stays as the way back in. A passkey only ever ends in the web
//! session the password login creates: API tokens and OAuth clients can neither
//! manage passkeys nor sign in with them.
//!
//! The relying party is `PAPERJAM_PUBLIC_URL`: its host is the RP ID and its
//! origin the one accepted origin. Without it, or when it names an IP address
//! (browsers refuse those as RP IDs) or plain http other than `localhost`,
//! passkeys are unavailable and the endpoints say so.
//!
//! Ceremony state (the challenge and what it was issued for) stays in memory
//! for [`CEREMONY_TTL`] under a random id the browser sends back, and is
//! removed the moment an answer arrives, whatever the outcome: a replayed or
//! late answer finds nothing. A restart forgets pending ceremonies, which
//! costs at most one retry.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, Webauthn, WebauthnBuilder,
};

use crate::config::Config;
use crate::error::{Result, ServiceError};
use crate::ids::{PasskeyId, UserId};
use crate::model::user::{PasskeyView, User};
use crate::service::{Ctx, Via, events, users};
use crate::{random, time};

const RP_NAME: &str = "paperjam";
/// How long a challenge can be answered; also the timeout the browser gets.
pub const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
/// Pending ceremonies kept at once. Login options are anonymous, so the store
/// is bounded; past the limit new ceremonies are refused until old ones expire.
const MAX_PENDING: usize = 1024;
/// Extra room in the store for passkey registrations.
const REGISTER_ROOM: usize = 64;
const MAX_LABEL_CHARS: usize = 64;

enum Login {
    /// No username given: the browser offers whichever passkey it holds.
    Discoverable(DiscoverableAuthentication),
    /// Username first: only that account's passkeys are allowed.
    Named {
        user_id: UserId,
        state: PasskeyAuthentication,
    },
}

enum Ceremony {
    Register {
        user_id: UserId,
        state: PasskeyRegistration,
    },
    Login(Login),
}

struct Pending {
    ceremony: Ceremony,
    started: Instant,
}

struct RelyingParty {
    id: String,
    webauthn: Webauthn,
}

/// The relying party (when configured) and the pending ceremonies.
pub struct Passkeys {
    rp: Option<RelyingParty>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// What `navigator.credentials.create()` / `.get()` needs, and the id to send
/// back with the answer.
#[derive(Debug, Serialize)]
pub struct Challenge {
    pub ceremony: String,
    /// The `publicKey` options as WebAuthn JSON (bytes are base64url).
    pub options: Value,
}

/// The passkeys of the signed-in user, plus what the browser needs to tell
/// its keychain which of them still work.
#[derive(Debug, Serialize)]
pub struct PasskeyList {
    pub available: bool,
    pub rp_id: Option<String>,
    /// The WebAuthn user handle of this account, base64url.
    pub user_handle: String,
    pub passkeys: Vec<PasskeyView>,
}

fn relying_party(url: &Url) -> std::result::Result<RelyingParty, &'static str> {
    let host = url
        .domain()
        .ok_or("PAPERJAM_PUBLIC_URL must name a host, not an IP address")?;
    let local = host == "localhost" || host.ends_with(".localhost");
    if url.scheme() != "https" && !local {
        return Err("PAPERJAM_PUBLIC_URL must use https (plain http works only on localhost)");
    }
    let origin = Url::parse(&url.origin().ascii_serialization())
        .map_err(|_| "PAPERJAM_PUBLIC_URL has no origin")?;
    let webauthn = WebauthnBuilder::new(host, &origin)
        .and_then(|b| b.rp_name(RP_NAME).timeout(CEREMONY_TTL).build())
        .map_err(|_| "PAPERJAM_PUBLIC_URL is not a valid WebAuthn relying party")?;
    Ok(RelyingParty {
        id: host.to_string(),
        webauthn,
    })
}

impl Passkeys {
    pub fn from_config(config: &Config) -> Self {
        let rp = config
            .public_url
            .as_ref()
            .and_then(|url| match relying_party(url) {
                Ok(rp) => Some(rp),
                Err(reason) => {
                    tracing::warn!(%url, reason, "passkeys are unavailable");
                    None
                }
            });
        Self {
            rp,
            pending: Mutex::default(),
        }
    }

    pub fn available(&self) -> bool {
        self.rp.is_some()
    }

    fn webauthn(&self) -> Result<&Webauthn> {
        self.rp.as_ref().map(|rp| &rp.webauthn).ok_or_else(|| {
            ServiceError::unavailable(
                "passkeys_unavailable",
                "passkeys need PAPERJAM_PUBLIC_URL set to the https address paperjam is opened at",
            )
        })
    }

    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, Pending>) -> T) -> T {
        let mut guard = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }

    fn start(&self, ceremony: Ceremony) -> Result<String> {
        let id = random::hex(32);
        // Anonymous sign-ins share MAX_PENDING; registering a passkey (always
        // by a signed-in user) has its own room, so a flood of sign-in
        // challenges cannot block it.
        let limit = match ceremony {
            Ceremony::Login(_) => MAX_PENDING,
            Ceremony::Register { .. } => MAX_PENDING + REGISTER_ROOM,
        };
        self.with(|map| {
            if map.len() >= limit {
                map.retain(|_, p| p.started.elapsed() < CEREMONY_TTL);
            }
            if map.len() >= limit {
                return Err(ServiceError::rate_limited(
                    "too_many_attempts",
                    "too many passkey sign-ins are pending, try again in a few minutes",
                ));
            }
            map.insert(
                id.clone(),
                Pending {
                    ceremony,
                    started: Instant::now(),
                },
            );
            Ok(())
        })?;
        Ok(id)
    }

    /// Removes the ceremony before anything is checked: each challenge is
    /// answered once, successfully or not.
    fn take(&self, id: &str) -> Result<Ceremony> {
        self.with(|map| map.remove(id))
            .filter(|p| p.started.elapsed() < CEREMONY_TTL)
            .map(|p| p.ceremony)
            .ok_or_else(expired)
    }
}

fn expired() -> ServiceError {
    ServiceError::invalid(
        "ceremony_expired",
        "the passkey request expired or was already used, try again",
    )
}

fn refused() -> ServiceError {
    ServiceError::unauthorized("invalid_passkey", "the passkey could not be verified")
}

fn webauthn_failed(e: impl std::fmt::Display) -> ServiceError {
    tracing::error!(error = %e, "passkey ceremony could not start");
    ServiceError::internal(
        "passkey_failed",
        "the passkey request could not be prepared",
    )
}

fn corrupt() -> ServiceError {
    ServiceError::internal("passkey_corrupt", "a stored passkey could not be read")
}

fn credential_text(id: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(id)
}

fn decode(json: &str) -> Result<Passkey> {
    serde_json::from_str(json).map_err(|_| corrupt())
}

fn to_value(value: &impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(webauthn_failed)
}

/// The signature counter inside a serialized passkey (0 for synced passkeys,
/// which do not count).
/// Whether a signature counter `sent` may follow the `stored` one. A
/// counting authenticator must count up: a counter at or below the last one
/// seen means a cloned key, and so does a key that counted before and now
/// sends 0 (WebAuthn §6.1.1). A synced passkey that never counts (0 stored,
/// 0 sent) is fine.
fn counter_moves_on(stored: i64, sent: i64) -> bool {
    sent > stored || (sent == 0 && stored == 0)
}

fn counter_of(json: &Value) -> i64 {
    json["cred"]["counter"].as_i64().unwrap_or(0)
}

/// Passkeys are managed from the web UI only, never with a token.
fn require_session(ctx: &Ctx<'_>) -> Result<UserId> {
    let user_id = ctx.actor.require_user()?;
    if ctx.actor.via == Via::Web {
        Ok(user_id)
    } else {
        Err(ServiceError::forbidden(
            "session_required",
            "passkeys are managed from a signed-in web session",
        ))
    }
}

fn validate_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(ServiceError::invalid(
            "invalid_passkey_label",
            format!("passkey names are 1-{MAX_LABEL_CHARS} characters"),
        ));
    }
    Ok(label.to_string())
}

async fn stored(ctx: &Ctx<'_>, user_id: UserId) -> Result<Vec<Passkey>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT passkey FROM passkeys WHERE user_id = ? ORDER BY id")
            .bind(user_id)
            .fetch_all(&ctx.app.db.read)
            .await?;
    rows.iter().map(|(json,)| decode(json)).collect()
}

pub async fn list(ctx: &Ctx<'_>) -> Result<PasskeyList> {
    let user_id = require_session(ctx)?;
    let passkeys = sqlx::query_as(
        "SELECT id, credential_id, label, created_at, last_used_at \
         FROM passkeys WHERE user_id = ? ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(&ctx.app.db.read)
    .await?;
    let passkeys_config = &ctx.app.passkeys;
    Ok(PasskeyList {
        available: passkeys_config.available(),
        rp_id: passkeys_config.rp.as_ref().map(|rp| rp.id.clone()),
        user_handle: credential_text(user_id.uuid().as_bytes()),
        passkeys,
    })
}

/// Starts adding a passkey to the signed-in account.
pub async fn begin_registration(ctx: &Ctx<'_>) -> Result<Challenge> {
    let user_id = require_session(ctx)?;
    let webauthn = ctx.app.passkeys.webauthn()?;
    let user = users::get_user(ctx, user_id).await?;
    // Keys already registered are excluded, so the browser says "already set
    // up" instead of minting a duplicate on the same device.
    let exclude = stored(ctx, user_id)
        .await?
        .iter()
        .map(|p| p.cred_id().clone())
        .collect();
    let (ccr, state) = webauthn
        .start_passkey_registration(
            user_id.uuid(),
            &user.username,
            &user.display_name,
            Some(exclude),
        )
        .map_err(webauthn_failed)?;
    let mut options = to_value(&ccr.public_key)?;
    // Discoverable where the authenticator can keep it (sign-in then needs no
    // username and shows up in autofill); a key without room for it still
    // works username-first.
    if let Some(selection) = options
        .get_mut("authenticatorSelection")
        .and_then(Value::as_object_mut)
    {
        selection.insert("residentKey".into(), json!("preferred"));
        selection.insert("requireResidentKey".into(), json!(false));
    }
    let ceremony = ctx
        .app
        .passkeys
        .start(Ceremony::Register { user_id, state })?;
    Ok(Challenge { ceremony, options })
}

/// Checks the browser's answer to [`begin_registration`] and stores the key.
pub async fn finish_registration(
    ctx: &Ctx<'_>,
    ceremony: &str,
    credential: &RegisterPublicKeyCredential,
    label: &str,
) -> Result<PasskeyView> {
    let user_id = require_session(ctx)?;
    let label = validate_label(label)?;
    let webauthn = ctx.app.passkeys.webauthn()?;
    let Ceremony::Register {
        user_id: owner,
        state,
    } = ctx.app.passkeys.take(ceremony)?
    else {
        return Err(expired());
    };
    if owner != user_id {
        return Err(expired());
    }
    let passkey = webauthn
        .finish_passkey_registration(credential, &state)
        .map_err(|e| {
            tracing::info!(error = %e, "passkey registration refused");
            ServiceError::invalid("invalid_passkey", "the passkey could not be verified")
        })?;

    let serialized = to_value(&passkey)?;
    let view = PasskeyView {
        id: PasskeyId::new(),
        credential_id: credential_text(passkey.cred_id()),
        label,
        created_at: time::now_ts(),
        last_used_at: None,
    };
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query(
        "INSERT INTO passkeys (id, user_id, credential_id, passkey, sign_count, label, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(view.id)
    .bind(user_id)
    .bind(&view.credential_id)
    .bind(serialized.to_string())
    .bind(counter_of(&serialized))
    .bind(&view.label)
    .bind(&view.created_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => ServiceError::conflict(
            "passkey_already_registered",
            "this passkey is already registered",
        ),
        _ => e.into(),
    })?;
    events::record(
        &mut tx,
        &ctx.actor,
        "passkey.register",
        None,
        json!({"passkey_id": view.id, "label": view.label}),
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

pub async fn revoke(ctx: &Ctx<'_>, id: PasskeyId) -> Result<()> {
    let user_id = require_session(ctx)?;
    let mut tx = ctx.app.db.write.begin().await?;
    // Another user's passkey is reported as missing rather than forbidden.
    let deleted = sqlx::query("DELETE FROM passkeys WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ServiceError::not_found(
            "passkey_not_found",
            "no such passkey",
        ));
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "passkey.revoke",
        None,
        json!({"passkey_id": id}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Starts a passkey sign-in. With a username that has passkeys, only those
/// are allowed (this tells the caller the account has passkeys, as any
/// username-first WebAuthn login does); otherwise the browser picks any
/// discoverable paperjam passkey it holds.
pub async fn begin_login(ctx: &Ctx<'_>, username: Option<&str>) -> Result<Challenge> {
    let webauthn = ctx.app.passkeys.webauthn()?;
    let mut named = None;
    if let Some(name) = username.map(str::trim).filter(|n| !n.is_empty()) {
        let rows: Vec<(UserId, String)> = sqlx::query_as(
            "SELECT p.user_id, p.passkey FROM passkeys p JOIN users u ON u.id = p.user_id \
             WHERE u.username = ? AND u.disabled_at IS NULL ORDER BY p.id",
        )
        .bind(name)
        .fetch_all(&ctx.app.db.read)
        .await?;
        if let Some((user_id, _)) = rows.first() {
            let keys = rows
                .iter()
                .map(|(_, json)| decode(json))
                .collect::<Result<Vec<_>>>()?;
            named = Some((*user_id, keys));
        }
    }
    let (options, login) = match named {
        Some((user_id, keys)) => {
            let (rcr, state) = webauthn
                .start_passkey_authentication(&keys)
                .map_err(webauthn_failed)?;
            (to_value(&rcr.public_key)?, Login::Named { user_id, state })
        }
        None => {
            let (rcr, state) = webauthn
                .start_discoverable_authentication()
                .map_err(webauthn_failed)?;
            (to_value(&rcr.public_key)?, Login::Discoverable(state))
        }
    };
    let ceremony = ctx.app.passkeys.start(Ceremony::Login(login))?;
    Ok(Challenge { ceremony, options })
}

#[derive(sqlx::FromRow)]
struct StoredPasskey {
    id: PasskeyId,
    user_id: UserId,
    passkey: String,
    sign_count: i64,
}

/// Checks a signed assertion and returns the account it signs in to.
pub async fn finish_login(
    ctx: &Ctx<'_>,
    ceremony: &str,
    credential: &PublicKeyCredential,
) -> Result<User> {
    let webauthn = ctx.app.passkeys.webauthn()?;
    let Ceremony::Login(login) = ctx.app.passkeys.take(ceremony)? else {
        return Err(expired());
    };
    let row: StoredPasskey = sqlx::query_as(
        "SELECT id, user_id, passkey, sign_count FROM passkeys WHERE credential_id = ?",
    )
    .bind(credential_text(credential.get_credential_id()))
    .fetch_optional(&ctx.app.db.read)
    .await?
    .ok_or_else(|| {
        ServiceError::unauthorized(
            "unknown_passkey",
            "this passkey is not registered here, sign in with the password",
        )
    })?;
    let mut passkey = decode(&row.passkey)?;

    let verified = match login {
        Login::Discoverable(state) => {
            // The user handle names the account the authenticator signs in
            // to; it has to be the account that registered the credential.
            let (handle, _) = webauthn
                .identify_discoverable_authentication(credential)
                .map_err(|_| refused())?;
            if handle != row.user_id.uuid() {
                tracing::info!(passkey_id = %row.id, "passkey user handle names another account");
                return Err(refused());
            }
            webauthn.finish_discoverable_authentication_v2(credential, state, &passkey)
        }
        Login::Named { user_id, state } => {
            if user_id != row.user_id {
                return Err(refused());
            }
            webauthn.finish_passkey_authentication(credential, &state)
        }
    }
    .map_err(|e| {
        tracing::info!(passkey_id = %row.id, error = %e, "passkey sign-in refused");
        refused()
    })?;

    let counter = i64::from(verified.counter());
    if !counter_moves_on(row.sign_count, counter) {
        tracing::warn!(passkey_id = %row.id, "passkey signature counter went backwards");
        return Err(refused());
    }

    let user = users::get_user(ctx, row.user_id).await?;
    if user.disabled_at.is_some() {
        return Err(ServiceError::unauthorized(
            "account_disabled",
            "this account is disabled",
        ));
    }

    passkey.update_credential(&verified);
    // Conditional on the counter read above: a concurrent sign-in with the
    // same counting key, or a revoke in between, loses.
    let updated = sqlx::query(
        "UPDATE passkeys SET passkey = ?, sign_count = ?, last_used_at = ? \
         WHERE id = ? AND sign_count = ?",
    )
    .bind(to_value(&passkey)?.to_string())
    .bind(counter.max(row.sign_count))
    .bind(time::now_ts())
    .bind(row.id)
    .bind(row.sign_count)
    .execute(&ctx.app.db.write)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(refused());
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::user::NewUser;
    use crate::service::testing::{TEST_SECRET, test_app};
    use crate::service::{Actor, App};
    use secrecy::SecretString;
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

    const PUBLIC_URL: &str = "https://paperjam.example.org";

    fn config(public_url: Option<&str>) -> Config {
        Config::from_lookup(|name| match name {
            "PAPERJAM_SECRET_KEY" => Some(TEST_SECRET.to_string()),
            "PAPERJAM_PUBLIC_URL" => public_url.map(str::to_string),
            _ => None,
        })
        .unwrap()
    }

    async fn app() -> (tempfile::TempDir, App) {
        let (dir, app) = test_app().await;
        let passkeys = Passkeys::from_config(&config(Some(PUBLIC_URL)));
        (dir, App { passkeys, ..app })
    }

    #[test]
    fn the_signature_counter_must_move_on() {
        // A synced passkey never counts.
        assert!(counter_moves_on(0, 0));
        // A counting key counts up.
        assert!(counter_moves_on(0, 1));
        assert!(counter_moves_on(7, 8));
        // Backwards, standing still, or back to 0 after counting: a clone.
        assert!(!counter_moves_on(7, 7));
        assert!(!counter_moves_on(7, 3));
        assert!(!counter_moves_on(7, 0));
    }

    async fn user(app: &App, name: &str) -> User {
        users::create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                username: name.into(),
                display_name: name.into(),
                password: SecretString::from("correct horse".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap()
    }

    fn web(user: &User) -> Actor {
        Actor::user(user.id, Via::Web)
    }

    fn origin() -> Url {
        Url::parse(PUBLIC_URL).unwrap()
    }

    async fn register(app: &App, user: &User, key: &mut SoftPasskey) -> PasskeyView {
        let ctx = app.ctx(web(user));
        let challenge = begin_registration(&ctx).await.unwrap();
        let options = CreationChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
        };
        let credential = key.do_registration(origin(), options).unwrap();
        finish_registration(&ctx, &challenge.ceremony, &credential, "Phone")
            .await
            .unwrap()
    }

    /// A username-first sign-in answered by `key`.
    async fn sign_in(app: &App, username: &str, key: &mut SoftPasskey) -> Result<User> {
        let ctx = app.ctx(Actor {
            user_id: None,
            via: Via::Web,
        });
        let challenge = begin_login(&ctx, Some(username)).await.unwrap();
        let options = RequestChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
            mediation: None,
        };
        let credential = key.do_authentication(origin(), options).unwrap();
        finish_login(&ctx, &challenge.ceremony, &credential).await
    }

    #[test]
    fn relying_party_comes_from_the_public_url() {
        let rp = relying_party(&Url::parse("https://paperjam.example.org/base/").unwrap()).unwrap();
        assert_eq!(rp.id, "paperjam.example.org");
        assert!(relying_party(&Url::parse("http://localhost:8140").unwrap()).is_ok());
        // Browsers refuse IP addresses as RP IDs, and passkeys need a secure context.
        assert!(relying_party(&Url::parse("https://192.0.2.10").unwrap()).is_err());
        assert!(relying_party(&Url::parse("http://paperjam.example.org").unwrap()).is_err());

        assert!(Passkeys::from_config(&config(Some(PUBLIC_URL))).available());
        assert!(!Passkeys::from_config(&config(None)).available());
        assert!(!Passkeys::from_config(&config(Some("http://192.0.2.10:8140"))).available());
    }

    #[tokio::test]
    async fn unavailable_without_a_relying_party() {
        let (_dir, app) = test_app().await;
        let alice = user(&app, "alice").await;
        let err = begin_registration(&app.ctx(web(&alice))).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Unavailable, "passkeys_unavailable")
        );
        // Listing still works and says why nothing can be added.
        assert!(!list(&app.ctx(web(&alice))).await.unwrap().available);
    }

    #[tokio::test]
    async fn ceremonies_are_single_use_and_expire() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let challenge = begin_registration(&ctx).await.unwrap();
        assert!(app.passkeys.take(&challenge.ceremony).is_ok());
        assert_eq!(
            app.passkeys.take(&challenge.ceremony).err().unwrap().code,
            "ceremony_expired"
        );

        let challenge = begin_registration(&ctx).await.unwrap();
        app.passkeys.with(|map| {
            map.get_mut(&challenge.ceremony).unwrap().started -= CEREMONY_TTL;
        });
        assert_eq!(
            app.passkeys.take(&challenge.ceremony).err().unwrap().code,
            "ceremony_expired"
        );
    }

    #[tokio::test]
    async fn the_pending_store_is_bounded() {
        let (_dir, app) = app().await;
        let anonymous = app.ctx(Actor {
            user_id: None,
            via: Via::Web,
        });
        for _ in 0..MAX_PENDING {
            begin_login(&anonymous, None).await.unwrap();
        }
        let err = begin_login(&anonymous, None).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::RateLimited);
        // A flood of sign-in challenges does not block adding a passkey.
        let alice = user(&app, "alice").await;
        assert!(begin_registration(&app.ctx(web(&alice))).await.is_ok());
        // Expired ceremonies make room again.
        app.passkeys.with(|map| {
            for pending in map.values_mut() {
                pending.started -= CEREMONY_TTL;
            }
        });
        assert!(begin_login(&anonymous, None).await.is_ok());
    }

    #[tokio::test]
    async fn registration_options_exclude_existing_keys_and_require_verification() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let view = register(&app, &alice, &mut SoftPasskey::new(true)).await;
        let options = begin_registration(&app.ctx(web(&alice)))
            .await
            .unwrap()
            .options;
        assert_eq!(options["rp"]["id"], "paperjam.example.org");
        assert_eq!(options["user"]["name"], "alice");
        assert_eq!(
            options["authenticatorSelection"]["userVerification"],
            "required"
        );
        assert_eq!(
            options["authenticatorSelection"]["residentKey"],
            "preferred"
        );
        assert_eq!(options["excludeCredentials"][0]["id"], view.credential_id);
    }

    #[tokio::test]
    async fn a_key_without_user_verification_is_refused() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let challenge = begin_registration(&ctx).await.unwrap();
        let options = CreationChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
        };
        // The soft key cannot verify the user and refuses to pretend.
        assert!(
            SoftPasskey::new(false)
                .do_registration(origin(), options)
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_credential_registered_elsewhere_conflicts() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let bob = user(&app, "bob").await;
        let ctx = app.ctx(web(&bob));
        let challenge = begin_registration(&ctx).await.unwrap();
        let options = CreationChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
        };
        let credential = SoftPasskey::new(true)
            .do_registration(origin(), options)
            .unwrap();
        // The same credential id already belongs to alice.
        sqlx::query(
            "INSERT INTO passkeys (id, user_id, credential_id, passkey, label, created_at) \
             VALUES (?, ?, ?, '{}', 'x', ?)",
        )
        .bind(PasskeyId::new())
        .bind(alice.id)
        .bind(credential_text(&credential.raw_id))
        .bind(time::now_ts())
        .execute(&app.db.write)
        .await
        .unwrap();
        let err = finish_registration(&ctx, &challenge.ceremony, &credential, "Phone")
            .await
            .unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Conflict, "passkey_already_registered")
        );
    }

    #[tokio::test]
    async fn a_ceremony_belongs_to_the_user_who_started_it() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let bob = user(&app, "bob").await;
        let challenge = begin_registration(&app.ctx(web(&alice))).await.unwrap();
        let options = CreationChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
        };
        let credential = SoftPasskey::new(true)
            .do_registration(origin(), options)
            .unwrap();
        let err = finish_registration(&app.ctx(web(&bob)), &challenge.ceremony, &credential, "x")
            .await
            .unwrap_err();
        assert_eq!(err.code, "ceremony_expired");
        assert!(list(&app.ctx(web(&bob))).await.unwrap().passkeys.is_empty());
    }

    #[tokio::test]
    async fn labels_are_checked_before_the_ceremony_is_spent() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let ctx = app.ctx(web(&alice));
        let challenge = begin_registration(&ctx).await.unwrap();
        let options = CreationChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
        };
        let credential = SoftPasskey::new(true)
            .do_registration(origin(), options)
            .unwrap();
        for label in ["", "   ", &"x".repeat(MAX_LABEL_CHARS + 1)] {
            let err = finish_registration(&ctx, &challenge.ceremony, &credential, label)
                .await
                .unwrap_err();
            assert_eq!(err.code, "invalid_passkey_label");
        }
        let view = finish_registration(&ctx, &challenge.ceremony, &credential, "  Phone ")
            .await
            .unwrap();
        assert_eq!(view.label, "Phone");
    }

    #[tokio::test]
    async fn tokens_cannot_manage_passkeys() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let view = register(&app, &alice, &mut SoftPasskey::new(true)).await;
        for via in [Via::Token, Via::OAuth] {
            let ctx = app.ctx(Actor::user(alice.id, via));
            assert_eq!(list(&ctx).await.unwrap_err().code, "session_required");
            assert_eq!(
                begin_registration(&ctx).await.unwrap_err().code,
                "session_required"
            );
            let err = revoke(&ctx, view.id).await.unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Forbidden, "session_required")
            );
        }
        assert_eq!(list(&app.ctx(web(&alice))).await.unwrap().passkeys.len(), 1);
    }

    #[tokio::test]
    async fn sign_in_counts_up_and_a_counter_going_back_is_refused() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let mut key = SoftPasskey::new(true);
        let view = register(&app, &alice, &mut key).await;

        // The soft key counts: each sign-in moves the stored counter forward.
        assert_eq!(sign_in(&app, "alice", &mut key).await.unwrap().id, alice.id);
        let first: (i64,) = sqlx::query_as("SELECT sign_count FROM passkeys")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        sign_in(&app, "ALICE", &mut key).await.unwrap();
        let second: (i64,) = sqlx::query_as("SELECT sign_count FROM passkeys")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert!(second.0 > first.0 && first.0 > 0);

        // A clone that already used higher counters: the next one looks old.
        sqlx::query("UPDATE passkeys SET sign_count = 1000 WHERE id = ?")
            .bind(view.id)
            .execute(&app.db.write)
            .await
            .unwrap();
        let err = sign_in(&app, "alice", &mut key).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Unauthorized, "invalid_passkey")
        );
    }

    #[tokio::test]
    async fn a_disabled_account_cannot_sign_in_with_a_passkey() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let mut key = SoftPasskey::new(true);
        register(&app, &alice, &mut key).await;
        users::disable_user(&app.ctx(Actor::cli()), alice.id)
            .await
            .unwrap();
        // A disabled account offers no passkeys by name...
        let anonymous = app.ctx(Actor {
            user_id: None,
            via: Via::Web,
        });
        let challenge = begin_login(&anonymous, Some("alice")).await.unwrap();
        assert_eq!(challenge.options["allowCredentials"], json!([]));
        // ...and a discoverable sign-in with its key is refused.
        let challenge = begin_login(&anonymous, None).await.unwrap();
        let mut options = RequestChallengeResponse {
            public_key: serde_json::from_value(challenge.options).unwrap(),
            mediation: None,
        };
        let stored: (String,) = sqlx::query_as("SELECT credential_id FROM passkeys")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        options.public_key.allow_credentials = vec![webauthn_rs_proto::AllowCredentials {
            type_: "public-key".into(),
            id: URL_SAFE_NO_PAD.decode(stored.0).unwrap(),
            transports: None,
        }];
        let mut credential = key.do_authentication(origin(), options).unwrap();
        credential.response.user_handle = Some(alice.id.uuid().as_bytes().to_vec());
        let err = finish_login(&anonymous, &challenge.ceremony, &credential)
            .await
            .unwrap_err();
        assert_eq!(err.code, "account_disabled");
    }

    #[tokio::test]
    async fn register_and_revoke_are_audited() {
        let (_dir, app) = app().await;
        let alice = user(&app, "alice").await;
        let view = register(&app, &alice, &mut SoftPasskey::new(true)).await;
        revoke(&app.ctx(web(&alice)), view.id).await.unwrap();
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT action, actor_user_id, diff FROM events \
             WHERE action LIKE 'passkey.%' ORDER BY id",
        )
        .fetch_all(&app.db.read)
        .await
        .unwrap();
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(actions, ["passkey.register", "passkey.revoke"]);
        assert!(rows.iter().all(|r| r.1 == Some(alice.id.to_string())));
        // No key material in the audit log.
        assert!(rows.iter().all(|r| !r.2.contains("cred")));
    }
}
