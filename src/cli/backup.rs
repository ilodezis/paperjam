use std::path::PathBuf;

use crate::config::Config;
use crate::service::backup;

pub async fn backup(dir: PathBuf) -> anyhow::Result<()> {
    let app = super::open_app().await?;
    let manifest = backup::backup(&app, &dir).await?;
    println!(
        "backed up {} documents ({} originals) to {}",
        manifest.documents,
        manifest.files.len(),
        dir.display()
    );
    Ok(())
}

/// Works on the data directory without opening it as a database.
pub fn restore(dir: PathBuf, force: bool) -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let manifest = backup::restore(&dir, &config.data_dir, force)?;
    println!(
        "restored {} documents ({} originals) into {}",
        manifest.documents,
        manifest.files.len(),
        config.data_dir.display()
    );
    println!(
        "the search index is rebuilt on the next start; run `paperjam reprocess --all` to \
         regenerate page images"
    );
    Ok(())
}
