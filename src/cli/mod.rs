//! Command line interface: thin wrappers over the service layer.

mod backup;
mod export;
mod import;
mod maintenance;
pub mod serve;
mod token;
mod user;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::service::App;

#[derive(Parser)]
#[command(
    name = "paperjam",
    version,
    about = "Self-hosted document archive with a first-class MCP server"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the HTTP server (REST, MCP, web UI) and background workers
    Serve,
    /// Manage users
    User {
        #[command(subcommand)]
        cmd: user::UserCommand,
    },
    /// Manage API tokens
    Token {
        #[command(subcommand)]
        cmd: token::TokenCommand,
    },
    /// Write a consistent snapshot of the data directory into an empty directory
    Backup {
        /// Directory to write the snapshot to (created if missing)
        dir: std::path::PathBuf,
    },
    /// Restore a snapshot into the (empty) data directory
    Restore {
        /// Snapshot directory made by `backup`
        dir: std::path::PathBuf,
        /// Replace data that is already there
        #[arg(long)]
        force: bool,
    },
    /// Export every document with its metadata as a zip archive
    Export {
        /// The zip file to write
        file: std::path::PathBuf,
    },
    /// Import documents from another system
    Import {
        #[command(subcommand)]
        cmd: import::ImportCommand,
    },
    /// Rebuild the search index from the database
    Reindex,
    /// Queue documents for reprocessing (a running server picks them up)
    Reprocess {
        /// Every document
        #[arg(long)]
        all: bool,
        /// Also run the LLM steps (costs money; local steps only by default)
        #[arg(long)]
        llm: bool,
        /// Document ids
        ids: Vec<crate::ids::DocumentId>,
    },
    /// Rebuild cached thumbnails and page renders in the current, much
    /// smaller format, and show scans from their clean copy (safe to run
    /// while the server is up; run again any time)
    Rerender {
        /// Document ids (every document when omitted)
        ids: Vec<crate::ids::DocumentId>,
    },
    /// Database migrations
    Migrate {
        #[command(subcommand)]
        cmd: maintenance::MigrateCommand,
    },
    /// Proxy MCP between stdio and a running server
    #[command(name = "mcp-stdio")]
    McpStdio {
        /// Base URL of the server, e.g. https://paperjam.example.org
        #[arg(long)]
        url: String,
        /// API token (`pj_...`); read from PAPERJAM_TOKEN when omitted, which
        /// keeps it out of the process list
        #[arg(long)]
        token: Option<String>,
    },
    /// Parse one untrusted file for the server (started by it, not by hand:
    /// reads a request on stdin, answers on stdout)
    #[command(name = "extract-worker", hide = true)]
    ExtractWorker,
    /// Check that a running server is healthy (exit code 0 or 1)
    Healthcheck {
        /// Health endpoint to query (default: the local server on PAPERJAM_BIND)
        #[arg(long)]
        url: Option<String>,
    },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.cmd {
        Command::ExtractWorker => {
            anyhow::bail!("extract-worker is started by the server, not by hand")
        }
        Command::User { cmd } => user::run(cmd).await,
        Command::Token { cmd } => token::run(cmd).await,
        Command::Serve => serve::run().await,
        Command::Backup { dir } => backup::backup(dir).await,
        Command::Restore { dir, force } => backup::restore(dir, force),
        Command::Export { file } => export::run(file).await,
        Command::Import { cmd } => import::run(cmd).await,
        Command::Reindex => maintenance::reindex().await,
        Command::Reprocess { all, llm, ids } => maintenance::reprocess(all, llm, ids).await,
        Command::Rerender { ids } => maintenance::rerender(ids).await,
        Command::Migrate { cmd } => maintenance::migrate(cmd).await,
        Command::McpStdio { url, token } => crate::mcp::stdio_proxy::run(url, token).await,
        Command::Healthcheck { url } => serve::healthcheck(url).await,
    }
}

/// Opens the data directory configured in the environment.
async fn open_app() -> anyhow::Result<App> {
    let config = Config::from_env()?;
    App::open(config).await
}
