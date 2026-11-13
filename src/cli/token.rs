use clap::Subcommand;

use crate::service::{Actor, Via, tokens, users};

#[derive(Subcommand)]
pub enum TokenCommand {
    /// Create an API token for a user. The value is printed once.
    Create {
        /// Username that owns the token
        #[arg(long)]
        user: String,
        /// A label for the token, e.g. the machine it lives on
        name: String,
    },
}

pub async fn run(cmd: TokenCommand) -> anyhow::Result<()> {
    match cmd {
        TokenCommand::Create { user, name } => {
            let app = super::open_app().await?;
            let owner = users::find_user_by_username(&app.ctx(Actor::cli()), &user).await?;
            let ctx = app.ctx(Actor::user(owner.id, Via::Cli));
            let (_view, raw) = tokens::create_token(&ctx, &name).await?;
            println!("{raw}");
            eprintln!("Store this token now; it is not shown again.");
            Ok(())
        }
    }
}
