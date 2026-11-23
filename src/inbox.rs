//! Watched inbox folder: polls a directory and ingests what appears in it
//! through the same service path as a web upload.
//!
//! Polling, because inotify does not fire on network mounts or for many sync
//! clients. A file is taken only when its size and modification time are the
//! same on two consecutive ticks and the modification time is at least
//! [`default_min_age`] in the past (a stalled writer can sit idle for a whole
//! interval). Once the original is stored and durable the source file is
//! deleted; the archive's copy is the original.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorKind, ServiceError};
use crate::ids::UserId;
use crate::service::settings::EmailUserTarget;
use crate::service::upload::{UploadInput, upload};
use crate::service::{Actor, App, Via, users};

const FAILED_DIR: &str = ".failed";
const MAX_DEPTH: usize = 16;
const TEMP_SUFFIXES: [&str; 2] = [".part", ".tmp"];
const MIN_AGE_FLOOR_SECS: u64 = 10;
const MAX_FAILED_FILES: usize = 100;
/// Upload errors that are about the file's content, not about reading it.
const REJECTION_CODES: [&str; 3] = ["unsupported_type", "empty_file", "file_too_large"];

/// One rejected file present in `<inbox>/.failed`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FailedInboxFile {
    pub name: String,
    pub reason: Option<String>,
    pub size: u64,
    pub failed_at: Option<String>,
}

/// Lists rejected files in `<dir>/.failed`, newest first, capped at 100 entries.
pub fn list_failed_files(dir: &Path) -> Vec<FailedInboxFile> {
    let failed_dir = dir.join(FAILED_DIR);
    let Ok(entries) = std::fs::read_dir(&failed_dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".reason.txt") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let size = meta.len();
        let failed_at = meta
            .modified()
            .ok()
            .map(|m| crate::time::ts(chrono::DateTime::<chrono::Utc>::from(m)));
        let reason_path = failed_dir.join(format!("{name}.reason.txt"));
        let reason = std::fs::read_to_string(&reason_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        files.push(FailedInboxFile {
            name,
            reason,
            size,
            failed_at,
        });
    }
    files.sort_by(|a, b| {
        b.failed_at
            .cmp(&a.failed_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    files.truncate(MAX_FAILED_FILES);
    files
}

/// How old a file's modification time must be before it is taken.
pub fn default_min_age(interval_secs: u64) -> Duration {
    Duration::from_secs(interval_secs.saturating_mul(2).max(MIN_AGE_FLOOR_SECS))
}

/// What one tick did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    /// Stored as new documents (or restored from the trash) and removed.
    pub ingested: usize,
    /// Already in the archive; removed.
    pub duplicates: usize,
    /// Moved to `.failed/`.
    pub rejected: usize,
    /// Seen for the first time, changed since the last tick, or too recent.
    pub waiting: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Signature {
    size: u64,
    modified: Option<SystemTime>,
}

impl Signature {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

struct Seen {
    signature: Signature,
    /// Failed in a way retrying cannot fix; left alone until the file changes.
    parked: bool,
    /// A transient read failure; not tried again before this instant.
    retry_at: Option<Instant>,
}

impl Seen {
    fn new(signature: Signature) -> Self {
        Self {
            signature,
            parked: false,
            retry_at: None,
        }
    }
}

pub struct Inbox {
    app: Arc<App>,
    dir: PathBuf,
    seen: HashMap<PathBuf, Seen>,
    last_user_problem: Option<String>,
    min_age_override: Option<Duration>,
    #[cfg(test)]
    open_fault: Option<std::io::ErrorKind>,
}

fn is_ignored_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    let lower = name.to_lowercase();
    name.starts_with('.')
        || name.starts_with('~')
        || TEMP_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// Regular files below `dir`, hidden entries and temp names skipped. Blocking:
/// call it from `spawn_blocking`.
fn list_files(dir: &Path) -> std::io::Result<Vec<(PathBuf, Signature)>> {
    let mut found = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(e) if depth == 0 => return Err(e),
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if is_ignored_name(&entry.file_name()) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((entry.path(), depth + 1));
                }
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                found.push((entry.path(), Signature::of(&meta)));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

async fn signature_of(path: &Path) -> Option<Signature> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some(Signature::of(&meta))
}

async fn unused_name(dir: &Path, name: &str) -> String {
    let taken = |n: String| async move {
        let reason = dir.join(format!("{n}.reason.txt"));
        tokio::fs::try_exists(dir.join(&n)).await.unwrap_or(true)
            || tokio::fs::try_exists(reason).await.unwrap_or(true)
    };
    if !taken(name.to_string()).await {
        return name.to_string();
    }
    let mut n = 1u64;
    loop {
        let candidate = format!("{name}.{n}");
        if !taken(candidate.clone()).await {
            return candidate;
        }
        n += 1;
    }
}

/// Remembers whether the wrapped reader ever returned an I/O error, so a read
/// failure on the source is told apart from a verdict about its content.
struct ReadTracker<R> {
    inner: R,
    failed: Arc<AtomicBool>,
}

impl<R: AsyncRead + Unpin> AsyncRead for ReadTracker<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Err(_))) {
            self.failed.store(true, Ordering::Relaxed);
        }
        polled
    }
}

#[derive(Debug)]
enum Verdict {
    Stored,
    Duplicate,
    /// The file's content is unacceptable; it is moved to `.failed/`.
    Rejected(ServiceError),
    /// The source could not be read (I/O error); tried again later.
    Retry,
    /// Not the file's fault (database or disk trouble); retried only if it changes.
    Failed(ServiceError),
}

/// A source whose copy is stored and waits for the durability barrier.
struct Stored {
    path: PathBuf,
    signature: Signature,
}

impl Inbox {
    pub fn new(app: Arc<App>, dir: PathBuf) -> Self {
        Self {
            app,
            dir,
            seen: HashMap::new(),
            last_user_problem: None,
            min_age_override: None,
            #[cfg(test)]
            open_fault: None,
        }
    }

    /// Overrides how old a file must be before it is taken (tests).
    #[must_use]
    pub fn with_min_age(mut self, min_age: Duration) -> Self {
        self.min_age_override = Some(min_age);
        self
    }

    fn effective_min_age(&self, interval_secs: u64) -> Duration {
        self.min_age_override
            .unwrap_or_else(|| default_min_age(interval_secs))
    }

    #[cfg(test)]
    async fn resolve_user(&mut self) -> Option<UserId> {
        let target = self.app.runtime_settings().inbox.user;
        self.resolve_user_target(target.as_ref()).await
    }

    /// The configured account, or the only active one. Logs a problem once.
    async fn resolve_user_target(&mut self, target: Option<&EmailUserTarget>) -> Option<UserId> {
        let ctx = self.app.ctx(Actor::system());
        let found = match target {
            Some(EmailUserTarget::UserId(id)) => users::get_user(&ctx, *id)
                .await
                .map_err(|e| format!("inbox user {id}: {e}"))
                .and_then(|u| match u.disabled_at {
                    None => Ok(u.id),
                    Some(_) => Err("inbox user: the account is disabled".to_string()),
                }),
            Some(EmailUserTarget::Username(name)) => users::find_user_by_username(&ctx, name)
                .await
                .map_err(|e| format!("PAPERJAM_INBOX_USER: {e}"))
                .and_then(|u| match u.disabled_at {
                    None => Ok(u.id),
                    Some(_) => Err("PAPERJAM_INBOX_USER: the account is disabled".to_string()),
                }),
            None => users::list_users(&ctx)
                .await
                .map_err(|e| e.to_string())
                .and_then(|all| {
                    let mut active = all.into_iter().filter(|u| u.disabled_at.is_none());
                    match (active.next(), active.next()) {
                        (Some(only), None) => Ok(only.id),
                        (None, _) => {
                            Err("there is no active user to attribute documents to".into())
                        }
                        _ => Err("more than one user exists; set PAPERJAM_INBOX_USER".into()),
                    }
                }),
        };
        match found {
            Ok(id) => {
                self.last_user_problem = None;
                Some(id)
            }
            Err(problem) => {
                if self.last_user_problem.as_deref() != Some(&problem) {
                    tracing::error!("inbox folder is idle: {problem}");
                    self.last_user_problem = Some(problem);
                }
                None
            }
        }
    }

    /// One scan. Public so tests can drive ticks without waiting.
    pub async fn tick(&mut self, cancel: &CancellationToken) -> TickReport {
        let mut report = TickReport::default();
        let cfg = self.app.runtime_settings().inbox;
        if cfg.paused {
            return report;
        }
        let min_age = self.effective_min_age(cfg.interval_secs);
        let Some(user) = self.resolve_user_target(cfg.user.as_ref()).await else {
            return report;
        };
        let dir = self.dir.clone();
        let listed = match tokio::task::spawn_blocking(move || list_files(&dir)).await {
            Ok(Ok(files)) => files,
            Ok(Err(e)) => {
                tracing::warn!(error = %e.kind(), "cannot read the inbox folder");
                return report;
            }
            Err(_) => return report,
        };
        let present: HashSet<&PathBuf> = listed.iter().map(|(p, _)| p).collect();
        self.seen.retain(|p, _| present.contains(p));

        let now = SystemTime::now();
        let mut stored = Vec::new();
        for (path, signature) in listed {
            if cancel.is_cancelled() {
                break;
            }
            match self.seen.get(&path) {
                Some(seen) if seen.signature == signature => {
                    if seen.parked || seen.retry_at.is_some_and(|at| at > Instant::now()) {
                        continue;
                    }
                }
                _ => {
                    self.seen.insert(path, Seen::new(signature));
                    report.waiting += 1;
                    continue;
                }
            }
            // An empty file is a writer that has not started yet. An mtime in
            // the future (clock skew) counts as too recent.
            let old_enough = signature
                .modified
                .is_none_or(|m| now.duration_since(m).is_ok_and(|age| age >= min_age));
            if signature.size == 0 || !old_enough {
                report.waiting += 1;
                continue;
            }
            self.handle(&path, signature, user, &mut report, &mut stored)
                .await;
        }
        self.remove_stored(stored).await;
        if report.ingested + report.duplicates + report.rejected > 0 {
            tracing::info!(
                ingested = report.ingested,
                duplicates = report.duplicates,
                rejected = report.rejected,
                "inbox folder scanned"
            );
        }
        report
    }

    async fn open(&self, path: &Path) -> std::io::Result<tokio::fs::File> {
        #[cfg(test)]
        if let Some(kind) = self.open_fault {
            return Err(kind.into());
        }
        tokio::fs::File::open(path).await
    }

    async fn handle(
        &mut self,
        path: &Path,
        signature: Signature,
        user: UserId,
        report: &mut TickReport,
        stored: &mut Vec<Stored>,
    ) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        tracing::debug!(file = %name, "ingesting from the inbox folder");

        let file = match self.open(path).await {
            Ok(file) => file,
            // Removed by its owner since the listing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.seen.remove(path);
                return;
            }
            Err(e) => {
                self.retry_later(path, signature, e.kind());
                return;
            }
        };
        // The open handle is what upload reads, so check that it is still the
        // file that was listed: a replacement or a late write means waiting.
        match file.metadata().await {
            Ok(meta) if Signature::of(&meta) == signature => {}
            Ok(_) => {
                tracing::debug!(file = %name, "inbox file changed before ingest, waiting");
                self.seen.remove(path);
                report.waiting += 1;
                return;
            }
            Err(e) => {
                self.retry_later(path, signature, e.kind());
                return;
            }
        }

        match self.ingest(&name, user, file).await {
            Verdict::Stored => {
                report.ingested += 1;
                stored.push(Stored {
                    path: path.to_path_buf(),
                    signature,
                });
            }
            Verdict::Duplicate => {
                report.duplicates += 1;
                stored.push(Stored {
                    path: path.to_path_buf(),
                    signature,
                });
            }
            Verdict::Rejected(error) => {
                report.rejected += 1;
                self.move_to_failed(path, signature, &name, &error).await;
            }
            Verdict::Retry => self.retry_later(path, signature, std::io::ErrorKind::Other),
            Verdict::Failed(error) => {
                tracing::error!(
                    code = error.code,
                    "inbox file could not be ingested; it stays in place until it changes"
                );
                self.park(path, signature);
            }
        }
    }

    /// Hands one reader to the upload path and classifies the outcome.
    async fn ingest(&self, name: &str, user: UserId, reader: impl AsyncRead + Unpin) -> Verdict {
        let failed = Arc::new(AtomicBool::new(false));
        let reader = ReadTracker {
            inner: reader,
            failed: Arc::clone(&failed),
        };
        let input = UploadInput {
            original_name: name.to_string(),
            ..UploadInput::default()
        };
        let ctx = self.app.ctx(Actor::user(user, Via::Inbox));
        match upload(&ctx, input, reader).await {
            Ok(outcome) => {
                tracing::info!(document = %outcome.document_id(), "document added from the inbox folder");
                Verdict::Stored
            }
            // A failed read is the source's I/O, never a verdict on its content.
            Err(_) if failed.load(Ordering::Relaxed) => Verdict::Retry,
            Err(e) if (e.kind, e.code) == (ErrorKind::Conflict, "duplicate") => Verdict::Duplicate,
            Err(e) if e.kind == ErrorKind::Invalid && REJECTION_CODES.contains(&e.code) => {
                Verdict::Rejected(e)
            }
            Err(e) => Verdict::Failed(e),
        }
    }

    fn park(&mut self, path: &Path, signature: Signature) {
        self.seen.insert(
            path.to_path_buf(),
            Seen {
                parked: true,
                ..Seen::new(signature)
            },
        );
    }

    /// The source could not be read right now; it stays in place and is tried
    /// again after the minimum file age.
    fn retry_later(&mut self, path: &Path, signature: Signature, kind: std::io::ErrorKind) {
        tracing::warn!(error = %kind, "could not read an inbox file; will retry");
        let min_age = self.effective_min_age(self.app.runtime_settings().inbox.interval_secs);
        self.seen.insert(
            path.to_path_buf(),
            Seen {
                retry_at: Some(Instant::now() + min_age),
                ..Seen::new(signature)
            },
        );
    }

    /// Deletes the sources of everything stored this tick.
    ///
    /// Ordering invariant: a source is deleted only after its archive copy is
    /// durable, because the source is then the last good copy until the power
    /// survives. `upload` has already fsynced the blob and its shard directory
    /// (`FileStore::commit`); the single `flush_durable` call below forces the
    /// database commits of the whole batch to disk before any file is removed.
    /// If it fails nothing is deleted; the files are seen as duplicates on the
    /// next tick and go through the barrier again.
    async fn remove_stored(&mut self, stored: Vec<Stored>) {
        if stored.is_empty() {
            return;
        }
        if let Err(e) = self.app.db.flush_durable().await {
            tracing::error!(error = %e, "could not make stored inbox files durable; keeping the sources");
            return;
        }
        for Stored { path, signature } in stored {
            self.remove_source(&path, signature).await;
        }
    }

    /// A file that changed while it was being read was not stored whole, so
    /// it is kept and looked at again as new content.
    async fn remove_source(&mut self, path: &Path, signature: Signature) {
        if signature_of(path).await != Some(signature) {
            tracing::warn!("inbox file changed after it was stored; keeping it for the next scan");
            self.seen.remove(path);
            return;
        }
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                self.seen.remove(path);
            }
            Err(e) => {
                tracing::warn!(error = %e.kind(), "stored an inbox file but could not delete it; leaving it in place");
                self.park(path, signature);
            }
        }
    }

    async fn move_to_failed(
        &mut self,
        path: &Path,
        signature: Signature,
        name: &str,
        error: &ServiceError,
    ) {
        tracing::warn!(code = error.code, "inbox file rejected");
        let failed = self.dir.join(FAILED_DIR);
        let moved = async {
            tokio::fs::create_dir_all(&failed).await?;
            let target = unused_name(&failed, name).await;
            // The reason goes first: a crash between the two steps leaves an
            // orphan note, never a rejected file without its explanation.
            let reason_path = failed.join(format!("{target}.reason.txt"));
            let reason = format!("{}: {}\n", error.code, error.message);
            tokio::fs::write(&reason_path, reason).await?;
            if let Err(e) = tokio::fs::rename(path, failed.join(&target)).await {
                let _ = tokio::fs::remove_file(&reason_path).await;
                return Err(e);
            }
            Ok::<(), std::io::Error>(())
        }
        .await;
        match moved {
            Ok(()) => {
                self.seen.remove(path);
            }
            Err(e) => {
                tracing::warn!(error = %e.kind(), "could not move a rejected inbox file; leaving it in place");
                self.park(path, signature);
            }
        }
    }
}

/// Starts the scanner: one task for the whole folder. It ends once `cancel`
/// fires and the tick in progress has finished its current file.
pub fn spawn(app: Arc<App>, cancel: CancellationToken) -> JoinHandle<()> {
    let dir = app
        .config
        .inbox_dir
        .clone()
        .expect("spawn is only called with PAPERJAM_INBOX_DIR set");
    let initial_interval = app.runtime_settings().inbox.interval_secs;
    tokio::spawn(async move {
        tracing::info!(
            interval_secs = initial_interval,
            "watching the inbox folder"
        );
        let mut inbox = Inbox::new(Arc::clone(&app), dir);
        loop {
            inbox.tick(&cancel).await;
            let interval = Duration::from_secs(app.runtime_settings().inbox.interval_secs);
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::user::NewUser;
    use crate::service::testing::test_app;

    fn pdf(tag: &str) -> Vec<u8> {
        format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
    }

    struct Fixture {
        _data: tempfile::TempDir,
        inbox_dir: tempfile::TempDir,
        app: Arc<App>,
        inbox: Inbox,
        cancel: CancellationToken,
    }

    async fn add_user(app: &App, name: &str) -> UserId {
        users::create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                username: name.into(),
                display_name: name.into(),
                password: secrecy::SecretString::from("correct horse battery".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn fixture_with(max_bytes: Option<u64>, user: Option<&str>, users: &[&str]) -> Fixture {
        let (data, app) = test_app().await;
        let mut config = (*app.config).clone();
        if let Some(max) = max_bytes {
            config.max_upload_bytes = max;
        }
        let inbox_dir = tempfile::tempdir().unwrap();
        config.inbox_dir = Some(inbox_dir.path().to_path_buf());
        config.inbox_interval_secs = 1;
        config.inbox_user = user.map(str::to_string);
        let app = Arc::new(App {
            config: Arc::new(config),
            ..app
        });
        app.reload_settings().await.unwrap();
        for name in users {
            add_user(&app, name).await;
        }
        let inbox = Inbox::new(Arc::clone(&app), inbox_dir.path().to_path_buf())
            .with_min_age(Duration::ZERO);
        Fixture {
            _data: data,
            inbox_dir,
            app,
            inbox,
            cancel: CancellationToken::new(),
        }
    }

    async fn fixture() -> Fixture {
        fixture_with(None, None, &["anna"]).await
    }

    impl Fixture {
        fn path(&self, rel: &str) -> PathBuf {
            self.inbox_dir.path().join(rel)
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.path(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        async fn tick(&mut self) -> TickReport {
            self.inbox.tick(&self.cancel).await
        }

        async fn documents(&self) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM documents")
                .fetch_one(&self.app.db.read)
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn a_stable_file_is_ingested_and_removed() {
        let mut f = fixture().await;
        f.write("scan.pdf", &pdf("stable"));

        let first = f.tick().await;
        assert_eq!((first.waiting, first.ingested), (1, 0));
        assert!(f.path("scan.pdf").exists());

        let second = f.tick().await;
        assert_eq!(second.ingested, 1);
        assert!(!f.path("scan.pdf").exists());
        assert_eq!(f.documents().await, 1);

        let (name, added_by, via): (String, Option<String>, String) = sqlx::query_as(
            "SELECT f.original_name, d.added_by, e.via FROM documents d \
             JOIN files f ON f.document_id = d.id \
             JOIN events e ON e.document_id = d.id AND e.action = 'document.upload'",
        )
        .fetch_one(&f.app.db.read)
        .await
        .unwrap();
        assert_eq!(name, "scan.pdf");
        assert_eq!(via, "inbox");
        let anna: String = sqlx::query_scalar("SELECT id FROM users WHERE username = 'anna'")
            .fetch_one(&f.app.db.read)
            .await
            .unwrap();
        assert_eq!(added_by, Some(anna));
    }

    #[test]
    fn the_default_minimum_age_is_twice_the_interval_with_a_floor() {
        assert_eq!(default_min_age(1), Duration::from_secs(10));
        assert_eq!(default_min_age(5), Duration::from_secs(10));
        assert_eq!(default_min_age(30), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn a_file_modified_too_recently_waits() {
        let mut f = fixture().await;
        f.inbox.min_age_override = Some(Duration::from_secs(3600));
        f.write("scan.pdf", &pdf("fresh"));
        for _ in 0..3 {
            let report = f.tick().await;
            assert_eq!((report.waiting, report.ingested), (1, 0));
        }
        assert!(f.path("scan.pdf").exists());
        assert_eq!(f.documents().await, 0);

        f.inbox.min_age_override = Some(Duration::ZERO);
        assert_eq!(f.tick().await.ingested, 1);
        assert!(!f.path("scan.pdf").exists());
    }

    #[tokio::test]
    async fn a_file_that_changed_since_the_listing_is_not_handed_to_upload() {
        let mut f = fixture().await;
        f.write("scan.pdf", &pdf("v1"));
        let listed = signature_of(&f.path("scan.pdf")).await.unwrap();
        let mut longer = pdf("v1");
        longer.extend_from_slice(b"% appended after the listing\n");
        f.write("scan.pdf", &longer);

        let user = f.inbox.resolve_user().await.unwrap();
        let (mut report, mut stored) = (TickReport::default(), Vec::new());
        let path = f.path("scan.pdf");
        f.inbox
            .handle(&path, listed, user, &mut report, &mut stored)
            .await;
        assert_eq!((report.waiting, report.ingested), (1, 0));
        assert!(stored.is_empty());
        assert!(path.exists());
        assert_eq!(f.documents().await, 0);
    }

    #[tokio::test]
    async fn a_file_that_changes_after_it_was_stored_is_kept() {
        let mut f = fixture().await;
        f.write("scan.pdf", &pdf("v1"));
        let path = f.path("scan.pdf");
        let stale = signature_of(&path).await.unwrap();
        f.write("scan.pdf", &pdf("v2 replaced"));
        f.inbox.remove_source(&path, stale).await;
        assert!(path.exists());
    }

    #[tokio::test]
    async fn an_open_error_retries_instead_of_quarantining() {
        let mut f = fixture().await;
        f.write("scan.pdf", &pdf("flaky"));
        f.inbox.open_fault = Some(std::io::ErrorKind::PermissionDenied);
        f.tick().await;
        for _ in 0..2 {
            let report = f.tick().await;
            assert_eq!((report.rejected, report.ingested), (0, 0));
        }
        assert!(f.path("scan.pdf").exists());
        assert!(!f.path(FAILED_DIR).exists());

        f.inbox.open_fault = None;
        assert_eq!(f.tick().await.ingested, 1);
        assert!(!f.path("scan.pdf").exists());
    }

    struct BrokenReader;

    impl AsyncRead for BrokenReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("connection reset")))
        }
    }

    #[tokio::test]
    async fn a_read_error_is_a_retry_not_a_rejection() {
        let mut f = fixture().await;
        let user = f.inbox.resolve_user().await.unwrap();
        let verdict = f.inbox.ingest("scan.pdf", user, BrokenReader).await;
        assert!(matches!(verdict, Verdict::Retry), "{verdict:?}");
        assert_eq!(f.documents().await, 0);
    }

    #[tokio::test]
    async fn only_content_problems_are_rejections() {
        let mut f = fixture().await;
        let user = f.inbox.resolve_user().await.unwrap();
        let exe = &b"MZ\x90\0\x03\0\0\0"[..];
        let verdict = f.inbox.ingest("a.exe", user, exe).await;
        assert!(matches!(verdict, Verdict::Rejected(_)), "{verdict:?}");
        let verdict = f.inbox.ingest("a.pdf", user, &b""[..]).await;
        assert!(matches!(verdict, Verdict::Rejected(_)), "{verdict:?}");
    }

    #[tokio::test]
    async fn a_reason_file_never_outlives_a_failed_move() {
        let mut f = fixture().await;
        let missing = f.path("gone.exe");
        let signature = Signature {
            size: 1,
            modified: None,
        };
        let error = ServiceError::invalid("unsupported_type", "nope");
        f.inbox
            .move_to_failed(&missing, signature, "gone.exe", &error)
            .await;
        assert!(!f.path(".failed/gone.exe.reason.txt").exists());
    }

    #[tokio::test]
    async fn a_growing_file_waits_for_a_quiet_tick() {
        let mut f = fixture().await;
        f.write("big.pdf", &pdf("grow"));
        f.tick().await;

        let mut longer = pdf("grow");
        longer.extend_from_slice(b"% more bytes arrived\n");
        f.write("big.pdf", &longer);
        let changed = f.tick().await;
        assert_eq!((changed.waiting, changed.ingested), (1, 0));
        assert!(f.path("big.pdf").exists());

        assert_eq!(f.tick().await.ingested, 1);
        assert!(!f.path("big.pdf").exists());
    }

    #[tokio::test]
    async fn hidden_temp_and_empty_files_are_left_alone() {
        let mut f = fixture().await;
        let ignored = [
            ".hidden.pdf",
            "upload.pdf.part",
            "upload.pdf.TMP",
            "~draft.pdf",
            ".syncthing.scan.pdf.tmp",
            ".~lock.scan.pdf#",
            ".hiddendir/scan.pdf",
        ];
        for name in ignored {
            f.write(name, &pdf(name));
        }
        f.write("empty.pdf", b"");
        f.tick().await;
        let second = f.tick().await;
        assert_eq!((second.ingested, second.rejected), (0, 0));
        assert_eq!(f.documents().await, 0);
        for name in ignored {
            assert!(f.path(name).exists(), "{name}");
        }
        assert!(f.path("empty.pdf").exists());
        assert!(!f.path(FAILED_DIR).exists());
    }

    #[tokio::test]
    async fn a_duplicate_is_removed_and_counted() {
        let mut f = fixture().await;
        f.write("a.pdf", &pdf("same"));
        f.tick().await;
        f.tick().await;
        assert_eq!(f.documents().await, 1);

        f.write("copy.pdf", &pdf("same"));
        f.tick().await;
        let report = f.tick().await;
        assert_eq!((report.duplicates, report.ingested), (1, 0));
        assert!(!f.path("copy.pdf").exists());
        assert_eq!(f.documents().await, 1);
    }

    #[tokio::test]
    async fn a_rejected_file_goes_to_failed_with_a_reason() {
        let mut f = fixture().await;
        f.write("setup.exe", b"MZ\x90\0\x03\0\0\0");
        f.tick().await;
        let report = f.tick().await;
        assert_eq!(report.rejected, 1);
        assert!(!f.path("setup.exe").exists());
        assert!(f.path(".failed/setup.exe").exists());
        let reason = std::fs::read_to_string(f.path(".failed/setup.exe.reason.txt")).unwrap();
        assert!(reason.starts_with("unsupported_type"), "{reason}");

        // Never retried: the failed folder is hidden from later scans.
        let again = f.tick().await;
        assert_eq!(again, TickReport::default());
        assert_eq!(f.documents().await, 0);

        // A second file with the same name does not overwrite the first.
        f.write("setup.exe", b"MZ\x90\0\x03\0\0\0");
        f.tick().await;
        f.tick().await;
        assert!(f.path(".failed/setup.exe.1").exists());
        assert!(f.path(".failed/setup.exe.1.reason.txt").exists());
    }

    #[tokio::test]
    async fn an_oversize_file_is_rejected() {
        let mut f = fixture_with(Some(64), None, &["anna"]).await;
        f.write("huge.pdf", &pdf(&"x".repeat(200)));
        f.tick().await;
        assert_eq!(f.tick().await.rejected, 1);
        let reason = std::fs::read_to_string(f.path(".failed/huge.pdf.reason.txt")).unwrap();
        assert!(reason.starts_with("file_too_large"), "{reason}");
    }

    #[tokio::test]
    async fn files_in_subdirectories_are_picked_up() {
        let mut f = fixture().await;
        f.write("2026/taxes/return.pdf", &pdf("nested"));
        f.tick().await;
        assert_eq!(f.tick().await.ingested, 1);
        assert!(!f.path("2026/taxes/return.pdf").exists());
        assert_eq!(f.documents().await, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_undeletable_source_is_not_ingested_again() {
        use std::os::unix::fs::PermissionsExt;
        let mut f = fixture().await;
        f.write("locked/scan.pdf", &pdf("locked"));
        let locked = f.path("locked");
        f.tick().await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::write(locked.join("probe"), b"x").is_ok();
        let report = f.tick().await;
        let _ = std::fs::remove_file(locked.join("probe"));
        if writable {
            // Running as a user that ignores permissions (root): nothing to prove.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        assert_eq!(report.ingested, 1);
        assert!(f.path("locked/scan.pdf").exists());
        for _ in 0..2 {
            assert_eq!(f.tick().await, TickReport::default());
        }
        assert_eq!(f.documents().await, 1);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn several_users_without_a_configured_one_keep_it_idle() {
        let mut f = fixture_with(None, None, &["anna", "ben"]).await;
        f.write("scan.pdf", &pdf("idle"));
        for _ in 0..3 {
            assert_eq!(f.tick().await, TickReport::default());
        }
        assert!(f.path("scan.pdf").exists());
        assert_eq!(f.documents().await, 0);
        assert!(f.inbox.last_user_problem.is_some());
    }

    #[tokio::test]
    async fn the_configured_user_wins_among_several() {
        let mut f = fixture_with(None, Some("ben"), &["anna", "ben"]).await;
        f.write("scan.pdf", &pdf("ben"));
        f.tick().await;
        assert_eq!(f.tick().await.ingested, 1);
        let owner: String = sqlx::query_scalar(
            "SELECT u.username FROM documents d JOIN users u ON u.id = d.added_by",
        )
        .fetch_one(&f.app.db.read)
        .await
        .unwrap();
        assert_eq!(owner, "ben");
    }

    #[tokio::test]
    async fn an_unknown_configured_user_keeps_it_idle() {
        let mut f = fixture_with(None, Some("nobody"), &["anna"]).await;
        f.write("scan.pdf", &pdf("idle"));
        assert_eq!(f.tick().await, TickReport::default());
        assert!(f.inbox.last_user_problem.is_some());
        assert!(f.path("scan.pdf").exists());
    }

    #[tokio::test]
    async fn the_scanner_task_stops_on_shutdown() {
        let f = fixture().await;
        let cancel = CancellationToken::new();
        let handle = spawn(Arc::clone(&f.app), cancel.clone());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the scanner stops promptly")
            .unwrap();
    }
}
