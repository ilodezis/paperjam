use std::io::IsTerminal;

use anyhow::{Context, bail};
use clap::Subcommand;
use secrecy::SecretString;

use crate::model::user::NewUser;
use crate::service::Actor;
use crate::service::users::{self, UserPatch};

#[derive(Subcommand)]
pub enum UserCommand {
    /// Create a user. The password comes from PAPERJAM_NEW_PASSWORD or a prompt.
    Add {
        username: String,
        /// Make the user an administrator
        #[arg(long)]
        admin: bool,
        /// Name shown in the UI (defaults to the username)
        #[arg(long)]
        display_name: Option<String>,
        /// Interface language, en or ru
        #[arg(long, default_value = "en")]
        locale: String,
    },
    /// List users
    List,
    /// Disable a user account and end its active sessions
    Disable { username: String },
    /// Re-enable a disabled user account
    Enable { username: String },
    /// Permanently delete a user account (documents remain in the archive)
    Delete { username: String },
    /// Set a new password for a user. The password comes from PAPERJAM_NEW_PASSWORD or a prompt.
    Passwd { username: String },
}

pub async fn run(cmd: UserCommand) -> anyhow::Result<()> {
    match cmd {
        UserCommand::Add {
            username,
            admin,
            display_name,
            locale,
        } => {
            let password = new_password()?;
            let app = super::open_app().await?;
            let user = users::create_user(
                &app.ctx(Actor::cli()),
                NewUser {
                    display_name: display_name.unwrap_or_else(|| username.clone()),
                    username,
                    password,
                    locale,
                    is_admin: admin,
                },
            )
            .await?;
            println!(
                "created user {}{}",
                user.username,
                if user.is_admin { " (admin)" } else { "" }
            );
            Ok(())
        }
        UserCommand::List => {
            let app = super::open_app().await?;
            for user in users::list_users(&app.ctx(Actor::cli())).await? {
                println!(
                    "{}\t{}\t{}",
                    user.username,
                    if user.is_admin { "admin" } else { "user" },
                    if user.disabled_at.is_some() {
                        "disabled"
                    } else {
                        "active"
                    },
                );
            }
            Ok(())
        }
        UserCommand::Disable { username } => {
            let app = super::open_app().await?;
            let ctx = app.ctx(Actor::cli());
            let user = users::find_user_by_username(&ctx, &username).await?;
            users::disable_user(&ctx, user.id).await?;
            println!("disabled user {}", user.username);
            Ok(())
        }
        UserCommand::Enable { username } => {
            let app = super::open_app().await?;
            let ctx = app.ctx(Actor::cli());
            let user = users::find_user_by_username(&ctx, &username).await?;
            users::enable_user(&ctx, user.id).await?;
            println!("enabled user {}", user.username);
            Ok(())
        }
        UserCommand::Delete { username } => {
            let app = super::open_app().await?;
            let ctx = app.ctx(Actor::cli());
            let user = users::find_user_by_username(&ctx, &username).await?;
            users::delete_user(&ctx, user.id).await?;
            println!("deleted user {}", user.username);
            Ok(())
        }
        UserCommand::Passwd { username } => {
            let password = new_password()?;
            let app = super::open_app().await?;
            let ctx = app.ctx(Actor::cli());
            let user = users::find_user_by_username(&ctx, &username).await?;
            users::update_user(
                &ctx,
                user.id,
                UserPatch {
                    password: Some(password),
                    ..Default::default()
                },
            )
            .await?;
            println!("updated password for {}", user.username);
            Ok(())
        }
    }
}

/// PAPERJAM_NEW_PASSWORD wins; otherwise prompt twice on a terminal.
fn new_password() -> anyhow::Result<SecretString> {
    if let Ok(password) = std::env::var("PAPERJAM_NEW_PASSWORD") {
        return Ok(SecretString::from(password));
    }
    if !std::io::stdin().is_terminal() {
        bail!("no password: set PAPERJAM_NEW_PASSWORD or run this command in a terminal");
    }
    let first = rpassword::prompt_password("Password: ").context("reading the password")?;
    let second = rpassword::prompt_password("Repeat password: ").context("reading the password")?;
    if first != second {
        bail!("the passwords do not match");
    }
    Ok(SecretString::from(first))
}
