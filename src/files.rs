//! Content-addressed storage for original files, plus derived-file paths.
//!
//! Originals live at `originals/<aa>/<sha256>` and are never modified (spec
//! §5.1). Uploads are streamed to `tmp/` while hashing, then renamed into
//! place. Identical content is stored once.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::OwnedMutexGuard;

use crate::error::{Result, ServiceError};
use crate::hashing::to_hex;
use crate::ids::DocumentId;

const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlob {
    pub sha256: String,
    pub size: u64,
    /// Path of the original relative to `originals/`, e.g. `2c/2cf24d…`.
    pub storage_key: String,
}

/// A fully received, hashed upload that is not yet in `originals/`.
/// Dropping it without calling [`FileStore::commit`] removes the temp file.
pub struct StagedBlob {
    tmp_path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

impl StagedBlob {
    /// The first `n` bytes (fewer for a short file), for type sniffing.
    pub async fn head(&self, n: usize) -> io::Result<Vec<u8>> {
        let file = tokio::fs::File::open(&self.tmp_path).await?;
        let mut head = Vec::with_capacity(n);
        file.take(n as u64).read_to_end(&mut head).await?;
        Ok(head)
    }
}

impl Drop for StagedBlob {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp_path);
    }
}

#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

// Messages carry only the io error kind, never a path.
fn storage_error(e: io::Error) -> ServiceError {
    ServiceError::internal(
        "storage_error",
        format!("file store I/O failed: {}", e.kind()),
    )
}

/// Flushes a directory's entries to disk. A no-op where directories cannot be opened.
async fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        tokio::fs::File::open(dir).await?.sync_all().await
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// `<aa>/<64 lowercase hex>`: the only shape a storage key may have.
pub(crate) fn is_valid_key(key: &str) -> bool {
    let Some((shard, hash)) = key.split_once('/') else {
        return false;
    };
    let hex = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    shard.len() == 2 && hash.len() == 64 && hex(shard) && hex(hash) && hash.starts_with(shard)
}

impl FileStore {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        let root = data_dir.to_path_buf();
        for sub in ["originals", "derived", "tmp"] {
            std::fs::create_dir_all(root.join(sub))?;
        }
        Ok(Self { root })
    }

    fn originals(&self) -> PathBuf {
        self.root.join("originals")
    }

    fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Streams `stream` to a temp file while hashing it. Fails with
    /// `file_too_large` as soon as more than `max_bytes` have been read.
    pub async fn stage_stream(
        &self,
        mut stream: impl AsyncRead + Unpin,
        max_bytes: u64,
    ) -> Result<StagedBlob> {
        let tmp_path = self.tmp().join(uuid::Uuid::now_v7().to_string());
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(storage_error)?;
        // From here on the guard owns cleanup, including on early return.
        let mut staged = StagedBlob {
            tmp_path,
            sha256: String::new(),
            size: 0,
        };
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let n = stream.read(&mut buf).await.map_err(|e| {
                ServiceError::invalid(
                    "upload_interrupted",
                    format!("upload stream failed: {}", e.kind()),
                )
            })?;
            if n == 0 {
                break;
            }
            staged.size += n as u64;
            if staged.size > max_bytes {
                return Err(ServiceError::invalid(
                    "file_too_large",
                    format!("file exceeds the {max_bytes} byte limit"),
                ));
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).await.map_err(storage_error)?;
        }
        file.flush().await.map_err(storage_error)?;
        file.sync_all().await.map_err(storage_error)?;
        staged.sha256 = to_hex(&hasher.finalize());
        Ok(staged)
    }

    /// Moves a staged blob into `originals/`. Callers hold the [`HashLocks`]
    /// lock for `staged.sha256` so a concurrent purge cannot delete the blob
    /// between this call and the database insert that references it.
    pub async fn commit(&self, staged: StagedBlob) -> Result<StoredBlob> {
        let key = format!("{}/{}", &staged.sha256[..2], staged.sha256);
        let target = self.originals().join(&key);
        // If the same content is already stored, the temp file is simply
        // removed when `staged` drops.
        if !tokio::fs::try_exists(&target)
            .await
            .map_err(storage_error)?
        {
            let shard = target.parent().expect("key has a shard dir");
            tokio::fs::create_dir_all(shard)
                .await
                .map_err(storage_error)?;
            tokio::fs::rename(&staged.tmp_path, &target)
                .await
                .map_err(storage_error)?;
            // The blob itself was fsynced while staging; the directory entry
            // (and a newly created shard directory) must be too, or a crash
            // can lose a file whose source the caller is about to delete.
            sync_dir(shard).await.map_err(storage_error)?;
            sync_dir(&self.originals()).await.map_err(storage_error)?;
        }
        Ok(StoredBlob {
            sha256: staged.sha256.clone(),
            size: staged.size,
            storage_key: key,
        })
    }

    /// Stage and commit in one step (no hash lock; for tests and tools).
    pub async fn put_stream(
        &self,
        stream: impl AsyncRead + Unpin,
        max_bytes: u64,
    ) -> Result<StoredBlob> {
        let staged = self.stage_stream(stream, max_bytes).await?;
        self.commit(staged).await
    }

    pub fn open_original(&self, key: &str) -> io::Result<std::fs::File> {
        if !is_valid_key(key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid storage key",
            ));
        }
        std::fs::File::open(self.originals().join(key))
    }

    pub fn derived_path(&self, doc: DocumentId, name: &str) -> PathBuf {
        // Only the final component is honoured, so a name can never climb out
        // of the document's directory.
        let file_name = Path::new(name)
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_else(|| "_".into());
        self.root
            .join("derived")
            .join(doc.to_string())
            .join(file_name)
    }

    pub async fn delete_original(&self, key: &str) {
        if is_valid_key(key) {
            let _ = tokio::fs::remove_file(self.originals().join(key)).await;
        }
    }

    /// Removes everything derived from one document (renders, thumbnails).
    /// Whether the document has page images: its first page has a thumbnail.
    /// A plain-text file and a document that never got past recognition have
    /// none, and their image routes answer 404.
    pub async fn has_page_images(&self, doc: DocumentId) -> bool {
        tokio::fs::try_exists(self.derived_path(doc, "thumbnail.webp"))
            .await
            .unwrap_or(false)
    }

    pub async fn delete_derived(&self, doc: DocumentId) {
        let _ = tokio::fs::remove_dir_all(self.root.join("derived").join(doc.to_string())).await;
    }

    /// Deletes blobs whose key is not in `known`, and stale temp files.
    /// Returns the number of removed originals.
    pub async fn sweep_orphans(&self, known: &HashSet<String>) -> usize {
        let mut removed = 0;
        if let Ok(mut shards) = tokio::fs::read_dir(self.originals()).await {
            while let Ok(Some(shard)) = shards.next_entry().await {
                let Ok(shard_name) = shard.file_name().into_string() else {
                    continue;
                };
                let Ok(mut blobs) = tokio::fs::read_dir(shard.path()).await else {
                    continue;
                };
                while let Ok(Some(blob)) = blobs.next_entry().await {
                    let Ok(name) = blob.file_name().into_string() else {
                        continue;
                    };
                    let key = format!("{shard_name}/{name}");
                    if !known.contains(&key) && tokio::fs::remove_file(blob.path()).await.is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        if let Ok(mut tmp) = tokio::fs::read_dir(self.tmp()).await {
            while let Ok(Some(entry)) = tmp.next_entry().await {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
        removed
    }
}

/// Per-hash async locks serializing uploads and purges of the same bytes.
#[derive(Default)]
pub struct HashLocks {
    inner: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl HashLocks {
    pub async fn lock(&self, sha256: &str) -> OwnedMutexGuard<()> {
        let mutex = {
            let mut map = self.inner.lock().expect("hash lock map poisoned");
            map.retain(|_, w| w.strong_count() > 0);
            match map.get(sha256).and_then(Weak::upgrade) {
                Some(m) => m,
                None => {
                    let m = Arc::new(tokio::sync::Mutex::new(()));
                    map.insert(sha256.to_string(), Arc::downgrade(&m));
                    m
                }
            }
        };
        mutex.lock_owned().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn store() -> (tempfile::TempDir, FileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn files_in(dir: &Path) -> Vec<PathBuf> {
        walk(dir)
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn put_stream_hashes_and_stores() {
        let (dir, store) = store();
        let blob = store.put_stream(&b"hello"[..], 1024).await.unwrap();
        assert_eq!(blob.sha256, HELLO_SHA);
        assert_eq!(blob.size, 5);
        assert_eq!(blob.storage_key, format!("2c/{HELLO_SHA}"));
        let on_disk = std::fs::read(dir.path().join("originals").join(&blob.storage_key)).unwrap();
        assert_eq!(on_disk, b"hello");
        let mut f = store.open_original(&blob.storage_key).unwrap();
        let mut buf = Vec::new();
        io::Read::read_to_end(&mut f, &mut buf).unwrap();
        assert_eq!(buf, b"hello");
    }

    #[tokio::test]
    async fn put_stream_rejects_oversize() {
        let (dir, store) = store();
        let err = store.put_stream(&b"hello"[..], 4).await.unwrap_err();
        assert_eq!(err.kind, crate::error::ErrorKind::Invalid);
        assert_eq!(err.code, "file_too_large");
        assert!(files_in(&dir.path().join("tmp")).is_empty());
        assert!(files_in(&dir.path().join("originals")).is_empty());
    }

    #[tokio::test]
    async fn exactly_max_bytes_is_accepted() {
        let (_dir, store) = store();
        assert!(store.put_stream(&b"hello"[..], 5).await.is_ok());
    }

    #[tokio::test]
    async fn identical_content_is_stored_once() {
        let (dir, store) = store();
        let a = store.put_stream(&b"same bytes"[..], 1024).await.unwrap();
        let b = store.put_stream(&b"same bytes"[..], 1024).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(files_in(&dir.path().join("originals")).len(), 1);
        assert!(files_in(&dir.path().join("tmp")).is_empty());
    }

    #[tokio::test]
    async fn dropped_staged_blob_leaves_no_temp_file() {
        let (dir, store) = store();
        let staged = store.stage_stream(&b"abandoned"[..], 1024).await.unwrap();
        assert_eq!(files_in(&dir.path().join("tmp")).len(), 1);
        drop(staged);
        assert!(files_in(&dir.path().join("tmp")).is_empty());
    }

    #[tokio::test]
    async fn sweep_removes_unknown_blobs() {
        let (_dir, store) = store();
        let keep = store.put_stream(&b"keep me"[..], 1024).await.unwrap();
        let drop_ = store.put_stream(&b"orphan"[..], 1024).await.unwrap();
        let known: HashSet<String> = [keep.storage_key.clone()].into_iter().collect();
        assert_eq!(store.sweep_orphans(&known).await, 1);
        assert!(store.open_original(&keep.storage_key).is_ok());
        assert!(store.open_original(&drop_.storage_key).is_err());
    }

    #[tokio::test]
    async fn delete_original_removes_the_blob() {
        let (_dir, store) = store();
        let blob = store.put_stream(&b"bye"[..], 1024).await.unwrap();
        store.delete_original(&blob.storage_key).await;
        assert!(store.open_original(&blob.storage_key).is_err());
        // Deleting again is a no-op.
        store.delete_original(&blob.storage_key).await;
    }

    #[tokio::test]
    async fn keys_cannot_escape_the_store() {
        let (_dir, store) = store();
        for bad in [
            "../paperjam.db",
            "../../etc/passwd",
            "/etc/passwd",
            "ab/../../x",
            "zz/short",
        ] {
            assert!(
                store.open_original(bad).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn derived_path_is_per_document_and_flat() {
        let (dir, store) = store();
        let doc = DocumentId::new();
        let p = store.derived_path(doc, "thumb.webp");
        assert_eq!(
            p,
            dir.path()
                .join("derived")
                .join(doc.to_string())
                .join("thumb.webp")
        );
        // A name with separators cannot climb out of the document directory.
        let p = store.derived_path(doc, "../../evil");
        assert_eq!(
            p.parent().unwrap(),
            dir.path().join("derived").join(doc.to_string())
        );
    }

    #[tokio::test]
    async fn hash_locks_serialize_the_same_hash() {
        let locks = Arc::new(HashLocks::default());
        let first = locks.lock("abc").await;
        // A different hash is independent.
        drop(locks.lock("other").await);

        let l2 = Arc::clone(&locks);
        let waiter = tokio::spawn(async move {
            let _g = l2.lock("abc").await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "second lock must wait for the first");
        drop(first);
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter should proceed once released")
            .unwrap();
    }
}
