//! First-run setup: how the first administrator comes to exist.
//!
//! An instance with no users is claimed in one of three ways, and never
//! without proof that the person has control of the installation:
//!
//! - the web page asks for the *setup code* that the server prints to its log
//!   at start-up (whoever can read the log owns the machine);
//! - `PAPERJAM_ADMIN_USER` and `PAPERJAM_ADMIN_PASSWORD` create the account at
//!   start-up, for scripts and app stores that fill in a form;
//! - `paperjam user add --admin <name>` on the command line.
//!
//! The setup code is derived from the secret key, so it is the same after a
//! restart and nothing has to be stored.

use std::net::SocketAddr;

use secrecy::ExposeSecret;

use crate::error::{Result, ServiceError};
use crate::model::user::{NewUser, User};
use crate::service::{Actor, App, Ctx, users};
use crate::signing;

const PURPOSE: &str = "setup-code";
const COMPARE_PURPOSE: &str = "setup-code-compare";
/// Hex digits in the code (48 bits), shown in groups of four.
const CODE_DIGITS: usize = 12;

/// Whether no user exists yet, so that the instance can still be claimed.
pub async fn is_needed(app: &App) -> Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&app.db.read)
        .await?;
    Ok(count == 0)
}

fn key(app: &App) -> &[u8] {
    app.config.secret_key.expose_secret().as_bytes()
}

fn raw_code(key: &[u8]) -> String {
    signing::sign(key, PURPOSE, "v1")[..CODE_DIGITS].to_string()
}

/// The code to type on the setup page, as `abcd-ef01-2345`.
pub fn setup_code(app: &App) -> String {
    code_for_key(key(app))
}

/// [`setup_code`] for a given secret key.
pub fn code_for_key(key: &[u8]) -> String {
    raw_code(key)
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// Constant-time comparison; spaces, dashes and case do not matter.
fn code_matches(app: &App, provided: &str) -> bool {
    let provided: String = provided
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let expected = raw_code(key(app));
    // Compare keyed digests of both, so the comparison does not leak how many
    // leading characters were right.
    let proof = signing::sign(key(app), COMPARE_PURPOSE, &provided);
    signing::verify(key(app), COMPARE_PURPOSE, &expected, &proof)
}

/// What the setup page submits.
pub struct FirstAdmin {
    pub code: String,
    pub username: String,
    pub display_name: String,
    pub password: secrecy::SecretString,
    pub locale: String,
}

/// Creates the first administrator after checking the setup code.
pub async fn create_first_admin(app: &App, input: FirstAdmin) -> Result<User> {
    if !code_matches(app, &input.code) {
        return Err(ServiceError::invalid(
            "invalid_setup_code",
            "that is not the setup code from the server log",
        ));
    }
    let ctx: Ctx<'_> = app.ctx(Actor {
        user_id: None,
        via: crate::service::Via::Web,
    });
    users::create_first_user(
        &ctx,
        NewUser {
            username: input.username,
            display_name: input.display_name,
            password: input.password,
            locale: input.locale,
            is_admin: true,
        },
    )
    .await
}

/// Creates the administrator named by `PAPERJAM_ADMIN_USER` and
/// `PAPERJAM_ADMIN_PASSWORD` when there is no user yet. Later starts leave
/// the variables alone, so they can be removed once the account exists.
pub async fn bootstrap(app: &App) -> Result<()> {
    let Some(admin) = &app.config.bootstrap_admin else {
        return Ok(());
    };
    if !is_needed(app).await? {
        return Ok(());
    }
    let user = users::create_first_user(
        &app.ctx(Actor::cli()),
        NewUser {
            display_name: admin.username.clone(),
            username: admin.username.clone(),
            password: admin.password.clone(),
            locale: "en".to_string(),
            is_admin: true,
        },
    )
    .await?;
    tracing::info!(username = %user.username, "created the first administrator from the environment");
    Ok(())
}

/// Tells whoever reads the log how to claim an instance without users.
pub async fn announce(app: &App, listening: SocketAddr) -> Result<()> {
    if !is_needed(app).await? {
        return Ok(());
    }
    let url = app.config.public_url.as_ref().map_or_else(
        || format!("http://localhost:{}", listening.port()),
        |u| u.as_str().trim_end_matches('/').to_string(),
    );
    println!("paperjam has no users yet.");
    println!("  Open {url} and enter the setup code: {}", setup_code(app));
    println!("  Or create the administrator here: paperjam user add --admin <name>");
    Ok(())
}
