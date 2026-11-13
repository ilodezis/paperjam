use std::path::PathBuf;

use crate::service::export;

pub async fn run(file: PathBuf) -> anyhow::Result<()> {
    let app = super::open_app().await?;
    let summary = export::export(&app, &file).await?;
    println!(
        "exported {} documents to {}",
        summary.documents,
        file.display()
    );
    Ok(())
}
