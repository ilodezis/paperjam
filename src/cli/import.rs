use std::path::PathBuf;

use clap::Subcommand;

use crate::service::paperless;

#[derive(Subcommand)]
pub enum ImportCommand {
    /// Import a Paperless-ngx export (the directory `document_exporter` wrote)
    Paperless {
        /// The export directory (unzip a `--zip` export first)
        dir: PathBuf,
        /// User the documents are attributed to (default: the only administrator)
        #[arg(long)]
        user: Option<String>,
        /// Parse and report what would be imported without writing anything
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn run(cmd: ImportCommand) -> anyhow::Result<()> {
    let ImportCommand::Paperless { dir, user, dry_run } = cmd;
    let app = super::open_app().await?;
    let user = paperless::resolve_user(&app, user.as_deref()).await?;
    let r = paperless::import(&app, &dir, user, dry_run).await?;
    let (imported, duplicates, created) = if r.dry_run {
        ("would import", "already in the archive", "to create")
    } else {
        ("imported", "duplicates skipped", "created")
    };
    println!("{imported}: {}", r.imported);
    println!("{duplicates}: {}", r.duplicates);
    if r.restored > 0 {
        println!("restored from the trash: {}", r.restored);
    }
    if r.trashed_in_paperless > 0 {
        println!("skipped (deleted in Paperless): {}", r.trashed_in_paperless);
    }
    println!("tags {created}: {}", r.tags_created);
    if r.tags_skipped > 0 {
        println!("tags skipped (name refused): {}", r.tags_skipped);
    }
    println!("correspondents {created}: {}", r.correspondents_created);
    if r.notes_dropped > 0 {
        println!("notes not imported: {}", r.notes_dropped);
    }
    println!("failed: {}", r.failed.len());
    if !r.failed.is_empty() {
        println!("(documents marked \"stored\" are in the archive without that metadata;");
        println!("a re-run skips them as duplicates, so fix them by hand)");
    }
    for (pk, reason) in &r.failed {
        println!("  document {pk}: {reason}");
    }
    if !r.failed.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
