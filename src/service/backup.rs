//! Backup and restore of the irreplaceable data: the database and the
//! originals. Renders and the search index are rebuildable and not included.
//!
//! A backup directory holds `paperjam.db` (a `VACUUM INTO` snapshot, so it is
//! consistent while the server runs), `originals/<aa>/<sha256>` (hard-linked
//! when the filesystem allows, copied otherwise) and `manifest.json`.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, ServiceError};
use crate::hashing::to_hex;
use crate::service::App;
use crate::time;

pub const MANIFEST_VERSION: u32 = 1;
const MANIFEST: &str = "manifest.json";
const DATABASE: &str = "paperjam.db";
const ORIGINALS: &str = "originals";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub created_at: String,
    pub documents: i64,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    pub storage_key: String,
    pub sha256: String,
    pub size: u64,
}

fn io_error(what: &'static str, e: &std::io::Error) -> ServiceError {
    tracing::error!(error = %e, what, "backup I/O failed");
    ServiceError::internal("backup_io_failed", format!("{what} failed"))
}

fn corrupt(why: impl Into<String>) -> ServiceError {
    ServiceError::invalid("backup_corrupt", why)
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Hard-links `from` to `to`, copying when the filesystem refuses.
fn link_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(from, to).is_err() {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

fn sha256_of(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((to_hex(&hasher.finalize()), size))
}

/// Writes a consistent snapshot into `target` (created; must be empty).
pub async fn backup(app: &App, target: &Path) -> Result<Manifest> {
    if target.exists() && !is_empty_dir(target) {
        return Err(ServiceError::invalid(
            "backup_target_not_empty",
            "the backup directory must be empty",
        ));
    }
    std::fs::create_dir_all(target).map_err(|e| io_error("creating the backup directory", &e))?;

    // Snapshot first, then list the files: the list can only be a superset.
    let database = target.join(DATABASE);
    sqlx::query("VACUUM INTO ?")
        .bind(database.to_string_lossy().into_owned())
        .execute(&app.db.write)
        .await?;
    let documents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM documents")
        .fetch_one(&app.db.read)
        .await?;
    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT storage_key, sha256, size FROM files ORDER BY storage_key")
            .fetch_all(&app.db.read)
            .await?;

    let originals = app.config.data_dir.join(ORIGINALS);
    let destination = target.join(ORIGINALS);
    let files = tokio::task::spawn_blocking(move || {
        let mut files = Vec::with_capacity(rows.len());
        for (storage_key, sha256, size) in rows {
            let from = originals.join(&storage_key);
            match link_or_copy(&from, &destination.join(&storage_key)) {
                Ok(()) => files.push(ManifestFile {
                    storage_key,
                    sha256,
                    size: size.max(0) as u64,
                }),
                // Purged between the snapshot and now: nothing to keep.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error("copying an original", &e)),
            }
        }
        Ok(files)
    })
    .await
    .map_err(|_| ServiceError::internal("backup_io_failed", "the backup task failed"))??;

    // A key kept in the data directory (not set by environment variable) goes
    // with the data: signed links and connected apps keep working after a
    // restore on another machine.
    let secret = app.config.data_dir.join(crate::config::SECRET_KEY_FILE);
    if secret.is_file() {
        std::fs::copy(&secret, target.join(crate::config::SECRET_KEY_FILE))
            .map_err(|e| io_error("copying the secret key", &e))?;
    }

    let manifest = Manifest {
        version: MANIFEST_VERSION,
        created_at: time::now_ts(),
        documents,
        files,
    };
    std::fs::write(
        target.join(MANIFEST),
        serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| io_error("writing the manifest", &e))?;
    Ok(manifest)
}

fn read_manifest(source: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(source.join(MANIFEST))
        .map_err(|_| corrupt("the directory has no manifest.json"))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| corrupt("manifest.json is not valid"))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(corrupt(format!(
            "backup format version {} is not supported",
            manifest.version
        )));
    }
    Ok(manifest)
}

/// Restores a snapshot into `data_dir`. Refuses a data directory that
/// already holds data unless `force`. Every original is verified against the
/// manifest before anything is written.
pub fn restore(source: &Path, data_dir: &Path, force: bool) -> Result<Manifest> {
    let manifest = read_manifest(source)?;
    if !source.join(DATABASE).is_file() {
        return Err(corrupt("the backup has no database"));
    }
    for file in &manifest.files {
        if !crate::files::is_valid_key(&file.storage_key) {
            return Err(corrupt(format!(
                "storage key {:?} in manifest is invalid",
                file.storage_key
            )));
        }
        let path = source.join(ORIGINALS).join(&file.storage_key);
        let (sha256, size) = sha256_of(&path)
            .map_err(|_| corrupt(format!("original {} is missing", file.storage_key)))?;
        if sha256 != file.sha256 || size != file.size {
            return Err(corrupt(format!(
                "original {} does not match the manifest",
                file.storage_key
            )));
        }
    }

    let has_data = data_dir.join(DATABASE).exists() || data_dir.join(ORIGINALS).exists();
    if has_data && !force {
        return Err(ServiceError::invalid(
            "data_dir_not_empty",
            "the data directory already holds data; use --force to replace it",
        ));
    }
    std::fs::create_dir_all(data_dir).map_err(|e| io_error("creating the data directory", &e))?;
    if has_data {
        for name in ["paperjam.db", "paperjam.db-wal", "paperjam.db-shm"] {
            let _ = std::fs::remove_file(data_dir.join(name));
        }
        for name in [ORIGINALS, "derived", "index"] {
            let _ = std::fs::remove_dir_all(data_dir.join(name));
        }
    }

    // The database is always copied: a live database must never share an
    // inode with the snapshot.
    std::fs::copy(source.join(DATABASE), data_dir.join(DATABASE))
        .map_err(|e| io_error("restoring the database", &e))?;
    let originals: PathBuf = data_dir.join(ORIGINALS);
    for file in &manifest.files {
        link_or_copy(
            &source.join(ORIGINALS).join(&file.storage_key),
            &originals.join(&file.storage_key),
        )
        .map_err(|e| io_error("restoring an original", &e))?;
    }
    // The key that went with this data, if the backup holds one.
    let key = source.join(crate::config::SECRET_KEY_FILE);
    if key.is_file() {
        let target = data_dir.join(crate::config::SECRET_KEY_FILE);
        let _ = std::fs::remove_file(&target);
        std::fs::copy(&key, &target).map_err(|e| io_error("restoring the secret key", &e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(manifest)
}
