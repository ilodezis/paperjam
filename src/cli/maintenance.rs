//! `reindex`, `reprocess`, `rerender` and `migrate status`.

use std::collections::BTreeMap;

use clap::Subcommand;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

use crate::config::Config;
use crate::db::MIGRATOR;
use crate::ids::DocumentId;
use crate::jobs::queue::ProcessOptions;
use crate::pipeline::rerender::{self, Report};
use crate::search::sync;
use crate::service::{Actor, documents};

#[derive(Subcommand)]
pub enum MigrateCommand {
    /// List the migrations and whether the database has applied them
    Status,
}

pub async fn reindex() -> anyhow::Result<()> {
    let app = super::open_app().await?;
    let count = sync::rebuild(&app).await?;
    println!("indexed {count} documents");
    Ok(())
}

pub async fn reprocess(all: bool, llm: bool, ids: Vec<DocumentId>) -> anyhow::Result<()> {
    if all == !ids.is_empty() {
        anyhow::bail!("give either --all or one or more document ids");
    }
    let app = super::open_app().await?;
    let ids = if all {
        sqlx::query_scalar("SELECT id FROM documents WHERE trashed_at IS NULL ORDER BY id")
            .fetch_all(&app.db.read)
            .await?
    } else {
        ids
    };
    let ctx = app.ctx(Actor::cli());
    for id in &ids {
        let opts = ProcessOptions {
            local: true,
            llm,
            model_override: None,
        };
        documents::reprocess_document(&ctx, *id, opts).await?;
    }
    println!(
        "queued {} documents (a running server processes them)",
        ids.len()
    );
    Ok(())
}

pub async fn rerender(ids: Vec<DocumentId>) -> anyhow::Result<()> {
    let app = super::open_app().await?;
    let ids: Vec<DocumentId> = if ids.is_empty() {
        sqlx::query_scalar("SELECT id FROM documents ORDER BY id")
            .fetch_all(&app.db.read)
            .await?
    } else {
        ids
    };
    let mut total = Report::default();
    for id in &ids {
        let dir = app.files.derived_path(*id, "_");
        let dir = dir.parent().unwrap_or(std::path::Path::new(".")).to_owned();
        let done = tokio::task::spawn_blocking(move || rerender::rerender_dir(&dir)).await?;
        match done {
            Ok(report) => {
                // The pages table keeps the size of what is shown.
                for (n, width, height) in &report.cleaned {
                    sqlx::query(
                        "UPDATE pages SET width = ?, height = ? WHERE document_id = ? AND n = ?",
                    )
                    .bind(i64::from(*width))
                    .bind(i64::from(*height))
                    .bind(id)
                    .bind(i64::from(*n))
                    .execute(&app.db.write)
                    .await?;
                }
                total.add(report);
            }
            // One unreadable image must not stop the rest.
            Err(e) => eprintln!("document {id}: {} ({})", e.message, e.code),
        }
    }
    println!(
        "checked {} documents: rebuilt {} page renders and {} thumbnails ({} KB -> {} KB)",
        ids.len(),
        total.pages,
        total.thumbnails,
        total.bytes_before / 1024,
        total.bytes_after / 1024,
    );
    Ok(())
}

pub async fn migrate(cmd: MigrateCommand) -> anyhow::Result<()> {
    match cmd {
        MigrateCommand::Status => status().await,
    }
}

/// Reads the migration table without applying anything.
async fn status() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let path = config.data_dir.join("paperjam.db");
    let mut applied: BTreeMap<i64, bool> = BTreeMap::new();
    if path.exists() {
        let mut conn = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(&path).read_only(true),
        )
        .await?;
        let rows: Result<Vec<(i64, bool)>, _> =
            sqlx::query_as("SELECT version, success FROM _sqlx_migrations")
                .fetch_all(&mut conn)
                .await;
        // No table yet: nothing applied.
        applied.extend(rows.unwrap_or_default());
    }
    for migration in MIGRATOR.iter() {
        let state = match applied.get(&migration.version) {
            Some(true) => "applied",
            Some(false) => "failed",
            None => "pending",
        };
        println!(
            "{:04}\t{}\t{state}",
            migration.version, migration.description
        );
    }
    Ok(())
}
