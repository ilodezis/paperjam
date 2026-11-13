//! The persistent job queue: a `jobs` table with claim/complete/fail
//! semantics, exponential backoff and recovery of jobs stuck in `running`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use tokio::sync::Notify;

use crate::error::{Result, ServiceError};
use crate::ids::{DocumentId, JobId};
use crate::time;

/// Attempts (claims) before a failing job is parked as `dead`.
pub const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF_SECS: i64 = 10;
const MAX_BACKOFF_SECS: i64 = 3600;
const MAX_ERROR_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Process,
    Reindex,
    /// Match the people against the documents already in the archive.
    Relink,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Process => "process",
            JobKind::Reindex => "reindex",
            JobKind::Relink => "relink",
        }
    }

    fn from_db(s: &str) -> Option<Self> {
        match s {
            "process" => Some(JobKind::Process),
            "reindex" => Some(JobKind::Reindex),
            "relink" => Some(JobKind::Relink),
            _ => None,
        }
    }
}

/// What a `Process` job should do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOptions {
    /// Run the local steps (extraction, OCR, MRZ, matching).
    #[serde(default = "default_true")]
    pub local: bool,
    /// Run the LLM steps (only if a key is configured and the document allows it).
    #[serde(default = "default_true")]
    pub llm: bool,
    #[serde(default)]
    pub model_override: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            local: true,
            llm: true,
            model_override: None,
        }
    }
}

/// Which workers may claim a job. Local processing (OCR is CPU heavy),
/// reindexing and relinking run in the `Ocr` pool; LLM-only processing, which waits on the
/// network, runs in the `Llm` pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPool {
    Ocr,
    Llm,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    pub document_id: Option<DocumentId>,
    pub payload: serde_json::Value,
    /// Number of times this job has been claimed, including this one.
    pub attempts: u32,
}

impl Job {
    pub fn process_options(&self) -> ProcessOptions {
        serde_json::from_value(self.payload.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QueueCounts {
    pub queued: u64,
    pub running: u64,
    pub dead: u64,
}

#[derive(Clone)]
pub struct JobQueue {
    pool: SqlitePool,
    wake: Arc<Notify>,
}

impl JobQueue {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Wakes idle workers. Call after the transaction that enqueued commits.
    pub fn wake(&self) {
        self.wake.notify_waiters();
    }

    pub(crate) fn waiter(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Adds a job inside the caller's transaction. A job of the same kind
    /// already queued for the same document (or for none) is reused (its payload is
    /// replaced), so requesting work twice is idempotent.
    pub async fn enqueue(
        &self,
        tx: &mut SqliteConnection,
        kind: JobKind,
        document: impl Into<Option<DocumentId>>,
        payload: impl Serialize,
    ) -> Result<JobId> {
        let payload = serde_json::to_value(payload).map_err(|_| {
            ServiceError::internal("job_payload_invalid", "job payload is not JSON")
        })?;
        let document = document.into();
        let now = time::now_ts();

        let existing: Option<(JobId,)> = sqlx::query_as(
            "SELECT id FROM jobs WHERE document_id IS ? AND kind = ? AND state = 'queued'",
        )
        .bind(document)
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id,)) = existing {
            // A fresh request should not wait out an old retry delay.
            sqlx::query("UPDATE jobs SET payload = ?, run_after = ?, updated_at = ? WHERE id = ?")
                .bind(payload.to_string())
                .bind(&now)
                .bind(&now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            return Ok(id);
        }

        let id = JobId::new();
        sqlx::query(
            "INSERT INTO jobs (id, kind, document_id, payload, state, attempts, run_after, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'queued', 0, ?, ?, ?)",
        )
        .bind(id)
        .bind(kind.as_str())
        .bind(document)
        .bind(payload.to_string())
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        Ok(id)
    }

    pub async fn claim(&self, kinds: &[JobKind]) -> Result<Option<Job>> {
        self.claim_at(kinds, None, Utc::now()).await
    }

    pub async fn claim_for_pool(&self, pool: WorkerPool) -> Result<Option<Job>> {
        let (kinds, local) = pool_filter(pool);
        self.claim_at(kinds, Some(local), Utc::now()).await
    }

    /// When the earliest queued job of `pool` becomes due: in the past or
    /// now for a job that is claimable already, later for one waiting out a
    /// retry delay, `None` when the pool has nothing queued.
    pub async fn next_due_for_pool(&self, pool: WorkerPool) -> Result<Option<DateTime<Utc>>> {
        let (kinds, local) = pool_filter(pool);
        let kind_names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let due: Option<String> = sqlx::query_scalar(
            "SELECT MIN(run_after) FROM jobs \
             WHERE state = 'queued' \
               AND kind IN (SELECT value FROM json_each(?1)) \
               AND (kind <> 'process' OR COALESCE(json_extract(payload, '$.local'), 1) = ?2)",
        )
        .bind(serde_json::json!(kind_names).to_string())
        .bind(local)
        .fetch_one(&self.pool)
        .await?;
        Ok(due.and_then(|s| DateTime::parse_from_rfc3339(&s).ok().map(|d| d.to_utc())))
    }

    /// `local`: `Some(true)` accepts only jobs that run local steps (and
    /// non-`Process` jobs), `Some(false)` only LLM-only `Process` jobs.
    pub(crate) async fn claim_at(
        &self,
        kinds: &[JobKind],
        local: Option<bool>,
        now: DateTime<Utc>,
    ) -> Result<Option<Job>> {
        let kind_names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let now = time::ts(now);
        // One statement picks and flips the job, so two workers can never
        // claim the same one (writes go through a single connection anyway).
        let row: Option<(JobId, String, Option<DocumentId>, String, i64)> = sqlx::query_as(
            "UPDATE jobs SET state = 'running', attempts = attempts + 1, updated_at = ?1 \
             WHERE id = ( \
                 SELECT id FROM jobs \
                 WHERE state = 'queued' AND run_after <= ?1 \
                   AND kind IN (SELECT value FROM json_each(?2)) \
                   AND (?3 IS NULL OR kind <> 'process' \
                        OR COALESCE(json_extract(payload, '$.local'), 1) = ?3) \
                 ORDER BY run_after, id LIMIT 1) \
             RETURNING id, kind, document_id, payload, attempts",
        )
        .bind(now)
        .bind(serde_json::json!(kind_names).to_string())
        .bind(local)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|(id, kind, document_id, payload, attempts)| {
            Some(Job {
                id,
                kind: JobKind::from_db(&kind)?,
                document_id,
                payload: serde_json::from_str(&payload).unwrap_or_default(),
                attempts: attempts as u32,
            })
        }))
    }

    pub async fn complete(&self, job: &Job) -> Result<()> {
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a failure. With `retry` the job goes back to `queued` after
    /// `min(2^attempts * 10 s, 1 h)`; after [`MAX_ATTEMPTS`] claims, or without
    /// `retry`, it is parked as `dead`.
    pub async fn fail(&self, job: &Job, error: &ServiceError, retry: bool) -> Result<()> {
        self.fail_at(job, error, retry, Utc::now()).await
    }

    pub(crate) async fn fail_at(
        &self,
        job: &Job,
        error: &ServiceError,
        retry: bool,
        now: DateTime<Utc>,
    ) -> Result<()> {
        // Codes and messages only; never document text.
        let message: String = format!("{}: {}", error.code, error.message)
            .chars()
            .take(MAX_ERROR_CHARS)
            .collect();
        if !retry || job.attempts >= MAX_ATTEMPTS {
            sqlx::query(
                "UPDATE jobs SET state = 'dead', last_error = ?, updated_at = ? WHERE id = ?",
            )
            .bind(message)
            .bind(time::ts(now))
            .bind(job.id)
            .execute(&self.pool)
            .await?;
        } else {
            let run_after = now + chrono::Duration::seconds(backoff_secs(job.attempts));
            sqlx::query(
                "UPDATE jobs SET state = 'queued', last_error = ?, run_after = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(message)
            .bind(time::ts(run_after))
            .bind(time::ts(now))
            .bind(job.id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Returns jobs stuck in `running` for longer than `older_than` to the
    /// queue. Run once at startup, before workers start.
    pub async fn reclaim_stale(&self, older_than: Duration) -> Result<u64> {
        self.reclaim_stale_at(older_than, Utc::now()).await
    }

    pub(crate) async fn reclaim_stale_at(
        &self,
        older_than: Duration,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let cutoff = now - chrono::Duration::from_std(older_than).unwrap_or(chrono::Duration::MAX);
        let reclaimed = sqlx::query(
            "UPDATE jobs SET state = 'queued', updated_at = ? WHERE state = 'running' AND updated_at < ?",
        )
        .bind(time::ts(now))
        .bind(time::ts(cutoff))
        .execute(&self.pool)
        .await?;
        Ok(reclaimed.rows_affected())
    }

    pub async fn counts(&self) -> Result<QueueCounts> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT state, COUNT(*) FROM jobs GROUP BY state")
                .fetch_all(&self.pool)
                .await?;
        let mut counts = QueueCounts::default();
        for (state, n) in rows {
            match state.as_str() {
                "queued" => counts.queued = n as u64,
                "running" => counts.running = n as u64,
                "dead" => counts.dead = n as u64,
                _ => {}
            }
        }
        Ok(counts)
    }
}

/// Kinds a pool claims, and whether its `Process` jobs run local steps
/// (see [`JobQueue::claim_at`]).
fn pool_filter(pool: WorkerPool) -> (&'static [JobKind], bool) {
    match pool {
        WorkerPool::Ocr => (&[JobKind::Process, JobKind::Reindex, JobKind::Relink], true),
        WorkerPool::Llm => (&[JobKind::Process], false),
    }
}

/// Delay before retrying after the `attempts`-th failed attempt:
/// `min(2^attempts * 10 s, 1 h)`.
fn backoff_secs(attempts: u32) -> i64 {
    BASE_BACKOFF_SECS
        .saturating_mul(1i64 << attempts.min(20))
        .min(MAX_BACKOFF_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::service::App;
    use crate::service::testing::{insert_document, test_app};
    use chrono::Duration as ChronoDuration;

    async fn enqueue(
        app: &App,
        kind: JobKind,
        doc: Option<DocumentId>,
        payload: impl Serialize,
    ) -> JobId {
        let mut tx = app.db.write.begin().await.unwrap();
        let id = app.jobs.enqueue(&mut tx, kind, doc, payload).await.unwrap();
        tx.commit().await.unwrap();
        id
    }

    async fn state_of(app: &App, id: JobId) -> (String, i64, String, Option<String>) {
        sqlx::query_as("SELECT state, attempts, run_after, last_error FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap()
    }

    fn err() -> ServiceError {
        ServiceError::internal("boom", "it broke")
    }

    #[tokio::test]
    async fn process_options_default_to_everything_on() {
        let o = ProcessOptions::default();
        assert!(o.local && o.llm && o.model_override.is_none());
        let from_empty: ProcessOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty, o);
        let llm_only: ProcessOptions = serde_json::from_str(r#"{"local": false}"#).unwrap();
        assert!(!llm_only.local && llm_only.llm);
    }

    #[tokio::test]
    async fn enqueue_then_claim_returns_the_job() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;

        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        assert_eq!(job.id, id);
        assert_eq!(job.kind, JobKind::Process);
        assert_eq!(job.document_id, Some(doc));
        assert_eq!(job.attempts, 1);
        assert_eq!(job.process_options(), ProcessOptions::default());
        assert_eq!(state_of(&app, id).await.0, "running");
        // Nothing left to claim.
        assert!(app.jobs.claim(&[JobKind::Process]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn claim_respects_the_kind_filter() {
        let (_dir, app) = test_app().await;
        enqueue(&app, JobKind::Reindex, None, serde_json::json!({})).await;
        assert!(app.jobs.claim(&[JobKind::Process]).await.unwrap().is_none());
        assert!(app.jobs.claim(&[JobKind::Reindex]).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn claim_is_exclusive() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;

        let (a, b) = tokio::join!(
            app.jobs.claim(&[JobKind::Process]),
            app.jobs.claim(&[JobKind::Process])
        );
        let winners = [a.unwrap(), b.unwrap()].into_iter().flatten().count();
        assert_eq!(winners, 1, "exactly one concurrent claim wins");
    }

    #[tokio::test]
    async fn enqueue_reuses_a_queued_job_for_the_same_document() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let first = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let second = enqueue(
            &app,
            JobKind::Process,
            Some(doc),
            ProcessOptions {
                local: false,
                llm: true,
                model_override: Some("m".into()),
            },
        )
        .await;
        assert_eq!(first, second);
        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        assert!(!job.process_options().local, "the latest payload wins");
        // Once running, a new request queues a fresh job.
        let third = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        assert_ne!(third, first);
    }

    #[tokio::test]
    async fn pools_split_local_and_llm_only_jobs() {
        let (_dir, app) = test_app().await;
        let a = insert_document(&app).await;
        let b = insert_document(&app).await;
        enqueue(&app, JobKind::Process, Some(a), ProcessOptions::default()).await;
        enqueue(
            &app,
            JobKind::Process,
            Some(b),
            ProcessOptions {
                local: false,
                ..ProcessOptions::default()
            },
        )
        .await;

        let ocr = app
            .jobs
            .claim_for_pool(WorkerPool::Ocr)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ocr.document_id, Some(a));
        assert!(
            app.jobs
                .claim_for_pool(WorkerPool::Ocr)
                .await
                .unwrap()
                .is_none()
        );
        let llm = app
            .jobs
            .claim_for_pool(WorkerPool::Llm)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(llm.document_id, Some(b));
    }

    #[tokio::test]
    async fn complete_removes_the_job() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        app.jobs.complete(&job).await.unwrap();
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_all(&app.db.read)
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn next_due_tells_when_the_pool_has_something_to_claim() {
        let (_dir, app) = test_app().await;
        assert_eq!(
            app.jobs.next_due_for_pool(WorkerPool::Ocr).await.unwrap(),
            None
        );

        let doc = insert_document(&app).await;
        let before = Utc::now();
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let due = app
            .jobs
            .next_due_for_pool(WorkerPool::Ocr)
            .await
            .unwrap()
            .unwrap();
        assert!(due >= before - ChronoDuration::seconds(1) && due <= Utc::now());
        // Local jobs are not the LLM pool's business.
        assert_eq!(
            app.jobs.next_due_for_pool(WorkerPool::Llm).await.unwrap(),
            None
        );

        // After a failure the job is due when its backoff ends, not before.
        let now = Utc::now();
        let job = app
            .jobs
            .claim_at(&[JobKind::Process], None, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.id, id);
        assert_eq!(
            app.jobs.next_due_for_pool(WorkerPool::Ocr).await.unwrap(),
            None,
            "a running job is not due"
        );
        app.jobs.fail_at(&job, &err(), true, now).await.unwrap();
        let due = app
            .jobs
            .next_due_for_pool(WorkerPool::Ocr)
            .await
            .unwrap()
            .unwrap();
        // Stored with millisecond precision.
        let expected = now + ChronoDuration::seconds(20);
        assert!((due - expected).abs() < ChronoDuration::milliseconds(2));
    }

    #[tokio::test]
    async fn next_due_picks_the_earliest_of_the_pools_jobs() {
        let (_dir, app) = test_app().await;
        let a = insert_document(&app).await;
        let b = insert_document(&app).await;
        let llm_only = ProcessOptions {
            local: false,
            ..ProcessOptions::default()
        };
        enqueue(&app, JobKind::Process, Some(a), ProcessOptions::default()).await;
        enqueue(&app, JobKind::Process, Some(b), llm_only).await;
        let later = Utc::now() + ChronoDuration::minutes(5);
        sqlx::query("UPDATE jobs SET run_after = ? WHERE document_id = ?")
            .bind(crate::time::ts(later))
            .bind(b)
            .execute(&app.db.write)
            .await
            .unwrap();

        let llm = app
            .jobs
            .next_due_for_pool(WorkerPool::Llm)
            .await
            .unwrap()
            .unwrap();
        assert!((llm - later).abs() < ChronoDuration::milliseconds(2));
        let ocr = app
            .jobs
            .next_due_for_pool(WorkerPool::Ocr)
            .await
            .unwrap()
            .unwrap();
        assert!(ocr < later - ChronoDuration::minutes(4));
    }

    #[tokio::test]
    async fn failed_job_backs_off() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let now = Utc::now();
        let job = app
            .jobs
            .claim_at(&[JobKind::Process], None, now)
            .await
            .unwrap()
            .unwrap();

        app.jobs.fail_at(&job, &err(), true, now).await.unwrap();

        let (state, attempts, run_after, last_error) = state_of(&app, id).await;
        assert_eq!((state.as_str(), attempts), ("queued", 1));
        assert_eq!(last_error.as_deref(), Some("boom: it broke"));
        // 2^1 * 10 s = 20 s.
        assert_eq!(
            run_after,
            crate::time::ts(now + ChronoDuration::seconds(20))
        );
        // Not claimable before then, claimable after, and attempts keep counting.
        let early = now + ChronoDuration::seconds(19);
        assert!(
            app.jobs
                .claim_at(&[JobKind::Process], None, early)
                .await
                .unwrap()
                .is_none()
        );
        let later = now + ChronoDuration::seconds(21);
        let again = app
            .jobs
            .claim_at(&[JobKind::Process], None, later)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.attempts, 2);
    }

    #[test]
    fn backoff_doubles_and_is_capped_at_one_hour() {
        let secs: Vec<i64> = [1, 2, 3, 4, 8, 9].map(backoff_secs).to_vec();
        assert_eq!(secs, [20, 40, 80, 160, 2560, 3600]);
        assert_eq!(backoff_secs(63), 3600);
        assert_eq!(backoff_secs(u32::MAX), 3600);
    }

    #[tokio::test]
    async fn job_dead_after_five_attempts() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let mut now = Utc::now();
        for attempt in 1..=MAX_ATTEMPTS {
            now += ChronoDuration::hours(2);
            let job = app
                .jobs
                .claim_at(&[JobKind::Process], None, now)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(job.attempts, attempt);
            app.jobs.fail_at(&job, &err(), true, now).await.unwrap();
        }
        let (state, attempts, _, last_error) = state_of(&app, id).await;
        assert_eq!((state.as_str(), attempts), ("dead", MAX_ATTEMPTS as i64));
        assert!(last_error.is_some());
        now += ChronoDuration::days(365);
        assert!(
            app.jobs
                .claim_at(&[JobKind::Process], None, now)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(app.jobs.counts().await.unwrap().dead, 1);
    }

    #[tokio::test]
    async fn non_retryable_failure_goes_straight_to_dead() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        let e = ServiceError::new(ErrorKind::Invalid, "bad_input", "nope");
        app.jobs.fail(&job, &e, false).await.unwrap();
        assert_eq!(state_of(&app, id).await.0, "dead");
    }

    #[tokio::test]
    async fn stale_running_job_is_reclaimed() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;
        let id = enqueue(&app, JobKind::Process, Some(doc), ProcessOptions::default()).await;
        let t0 = Utc::now();
        app.jobs
            .claim_at(&[JobKind::Process], None, t0)
            .await
            .unwrap()
            .unwrap();
        // The process "crashed": the job is still `running`.
        sqlx::query("UPDATE jobs SET updated_at = ? WHERE id = ?")
            .bind(crate::time::ts(t0))
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();

        // Not stale yet at +9 minutes.
        let n = app
            .jobs
            .reclaim_stale_at(Duration::from_secs(600), t0 + ChronoDuration::minutes(9))
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(state_of(&app, id).await.0, "running");

        let n = app
            .jobs
            .reclaim_stale_at(Duration::from_secs(600), t0 + ChronoDuration::minutes(11))
            .await
            .unwrap();
        assert_eq!(n, 1);
        let reclaimed = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        assert_eq!(reclaimed.id, id);
        assert_eq!(reclaimed.attempts, 2, "a crash still counts as an attempt");
    }

    #[tokio::test]
    async fn counts_report_queue_state() {
        let (_dir, app) = test_app().await;
        let a = insert_document(&app).await;
        let b = insert_document(&app).await;
        enqueue(&app, JobKind::Process, Some(a), ProcessOptions::default()).await;
        enqueue(&app, JobKind::Process, Some(b), ProcessOptions::default()).await;
        app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        let c = app.jobs.counts().await.unwrap();
        assert_eq!((c.queued, c.running, c.dead), (1, 1, 0));
    }
}
