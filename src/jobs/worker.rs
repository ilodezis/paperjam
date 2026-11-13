//! Worker pools: claim jobs, run a handler, record the outcome.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorKind, Result, ServiceError};
use crate::jobs::queue::{Job, WorkerPool};
use crate::service::App;

/// The longest an idle worker sleeps. Enqueueing wakes it and a retry delay
/// is slept out to the second, so this only bounds the damage of a missed
/// wake-up or a jump of the system clock.
const FALLBACK_WAKE: Duration = Duration::from_secs(60);
/// The shortest nap: a job that came due while the worker was looking is
/// claimed on the next round, and a queue that cannot be claimed from never
/// turns the worker into a busy loop.
const MIN_NAP: Duration = Duration::from_millis(50);
/// Pause after a database error while claiming, so a broken database does
/// not turn the worker into a busy loop.
const ERROR_PAUSE: Duration = Duration::from_secs(5);

/// Runs one job. Registered by the pipeline (`JobKind::Process`) and the
/// index maintenance code (`JobKind::Reindex`).
pub type Handler =
    Arc<dyn Fn(Arc<App>, Job) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send + Sync>;

pub struct Worker;

impl Worker {
    /// Starts `n` workers for `pool`. The returned handle completes after
    /// `cancel` fires and every worker has finished the job it was running.
    pub fn spawn(
        app: Arc<App>,
        pool: WorkerPool,
        n: usize,
        cancel: CancellationToken,
        handler: Handler,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            for _ in 0..n.max(1) {
                workers.spawn(worker_loop(
                    Arc::clone(&app),
                    pool,
                    cancel.clone(),
                    Arc::clone(&handler),
                ));
            }
            while workers.join_next().await.is_some() {}
        })
    }
}

async fn worker_loop(app: Arc<App>, pool: WorkerPool, cancel: CancellationToken, handler: Handler) {
    let wake = app.jobs.waiter();
    while !cancel.is_cancelled() {
        // Subscribe before looking at the queue: a job enqueued between the
        // claim and the sleep below must wake this worker, not wait for the
        // fallback.
        let woken = wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        match app.jobs.claim_for_pool(pool).await {
            Ok(Some(job)) => run_job(&app, &handler, job).await,
            Ok(None) => {
                let nap = nap_until_due(&app, pool).await;
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = &mut woken => {}
                    _ = tokio::time::sleep(nap) => {}
                }
            }
            Err(e) => {
                tracing::error!(code = e.code, "claiming a job failed");
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = tokio::time::sleep(ERROR_PAUSE) => {}
                }
            }
        }
    }
}

/// How long an idle worker may sleep: until the earliest queued job (a retry
/// waiting out its backoff) is due, at most [`FALLBACK_WAKE`].
async fn nap_until_due(app: &App, pool: WorkerPool) -> Duration {
    match app.jobs.next_due_for_pool(pool).await {
        Ok(Some(due)) => (due - Utc::now())
            .to_std()
            .unwrap_or_default()
            .clamp(MIN_NAP, FALLBACK_WAKE),
        Ok(None) => FALLBACK_WAKE,
        Err(e) => {
            tracing::error!(code = e.code, "reading the next due job failed");
            ERROR_PAUSE
        }
    }
}

async fn run_job(app: &Arc<App>, handler: &Handler, job: Job) {
    let job_id = job.id;
    // A separate task, so a panic in the handler fails the job instead of
    // taking the worker down.
    let outcome = tokio::spawn(handler(Arc::clone(app), job.clone())).await;
    let recorded = match outcome {
        Ok(Ok(())) => app.jobs.complete(&job).await,
        Ok(Err(e)) => {
            tracing::warn!(%job_id, code = e.code, attempts = job.attempts, "job failed");
            app.jobs.fail(&job, &e, is_retryable(e.kind)).await
        }
        Err(_) => {
            tracing::error!(%job_id, "job handler panicked");
            let e = ServiceError::internal("worker_panic", "the job handler panicked");
            app.jobs.fail(&job, &e, true).await
        }
    };
    if let Err(e) = recorded {
        tracing::error!(%job_id, code = e.code, "recording a job outcome failed");
    }
}

/// Failures worth retrying; everything else is the caller's mistake and
/// would fail again.
pub(crate) fn is_retryable(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::Invalid
            | ErrorKind::NotFound
            | ErrorKind::Conflict
            | ErrorKind::Forbidden
            | ErrorKind::Unauthorized
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ServiceError;
    use crate::jobs::queue::{JobKind, ProcessOptions};
    use crate::service::testing::{insert_document, test_app};
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn enqueue_process(app: &App) -> crate::ids::JobId {
        let doc = insert_document(app).await;
        let mut tx = app.db.write.begin().await.unwrap();
        let id = app
            .jobs
            .enqueue(&mut tx, JobKind::Process, doc, ProcessOptions::default())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        app.jobs.wake();
        id
    }

    fn counting_handler(counter: Arc<AtomicUsize>) -> Handler {
        Arc::new(move |_app, _job| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        })
    }

    async fn wait_until(mut cond: impl AsyncFnMut() -> bool) {
        for _ in 0..60 {
            if cond().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("condition not reached within 3 s");
    }

    async fn job_count(app: &App) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
            .fetch_one(&app.db.read)
            .await
            .unwrap()
    }

    async fn stop(cancel: CancellationToken, handle: JoinHandle<()>) {
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("workers stop after cancel")
            .unwrap();
    }

    #[tokio::test]
    async fn worker_runs_jobs_to_completion_and_stops_on_cancel() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            enqueue_process(&app).await;
        }
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            2,
            cancel.clone(),
            counting_handler(Arc::clone(&done)),
        );

        wait_until(async || job_count(&app).await == 0).await;
        assert_eq!(done.load(Ordering::SeqCst), 3);
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn failing_handler_requeues_with_backoff() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let id = enqueue_process(&app).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let handler: Handler = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_app, _job| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(ServiceError::internal("flaky", "try later"))
                })
            })
        };
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            1,
            cancel.clone(),
            handler,
        );

        wait_until(async || calls.load(Ordering::SeqCst) >= 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        stop(cancel, handle).await;

        // Failed once, then backed off (20 s): not retried in a hot loop.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (state, attempts, last_error): (String, i64, Option<String>) =
            sqlx::query_as("SELECT state, attempts, last_error FROM jobs WHERE id = ?")
                .bind(id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!((state.as_str(), attempts), ("queued", 1));
        assert_eq!(last_error.as_deref(), Some("flaky: try later"));
    }

    #[tokio::test]
    async fn caller_errors_are_not_retried() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let id = enqueue_process(&app).await;
        let handler: Handler = Arc::new(|_app, _job| {
            Box::pin(async { Err(ServiceError::invalid("bad_input", "never works")) })
        });
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            1,
            cancel.clone(),
            handler,
        );

        wait_until(async || {
            let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = ?")
                .bind(id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
            state == "dead"
        })
        .await;
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn panicking_handler_does_not_kill_the_worker() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let first = enqueue_process(&app).await;
        let seen = Arc::new(AtomicUsize::new(0));
        let handler: Handler = {
            let seen = Arc::clone(&seen);
            Arc::new(move |_app, job| {
                let seen = Arc::clone(&seen);
                Box::pin(async move {
                    if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("handler bug on {}", job.id);
                    }
                    Ok(())
                })
            })
        };
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            1,
            cancel.clone(),
            handler,
        );
        wait_until(async || seen.load(Ordering::SeqCst) >= 1).await;

        // The worker is still alive and picks up the next job.
        let second = enqueue_process(&app).await;
        wait_until(async || seen.load(Ordering::SeqCst) >= 2).await;
        wait_until(async || {
            let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = ?")
                .bind(second)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
            gone == 0
        })
        .await;
        stop(cancel, handle).await;

        let (state, error): (String, Option<String>) =
            sqlx::query_as("SELECT state, last_error FROM jobs WHERE id = ?")
                .bind(first)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(state, "queued", "the panicked job is retried later");
        assert!(error.unwrap().starts_with("worker_panic"));
    }

    #[tokio::test]
    async fn shutdown_waits_for_the_running_job() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        enqueue_process(&app).await;
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let handler: Handler = {
            let (started, finished) = (Arc::clone(&started), Arc::clone(&finished));
            Arc::new(move |_app, _job| {
                let (started, finished) = (Arc::clone(&started), Arc::clone(&finished));
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    finished.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
        };
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            1,
            cancel.clone(),
            handler,
        );
        wait_until(async || started.load(Ordering::SeqCst) == 1).await;

        stop(cancel, handle).await;

        assert_eq!(
            finished.load(Ordering::SeqCst),
            1,
            "running job finished before exit"
        );
        assert_eq!(job_count(&app).await, 0, "and was completed");
    }

    /// Workers idle on a long sleep, so a job is picked up because it was
    /// announced, not because somebody looked at the queue by chance.
    #[tokio::test]
    async fn idle_workers_do_not_poll_but_wake_on_enqueue() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            3,
            cancel.clone(),
            counting_handler(Arc::clone(&done)),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A job that appears without a wake-up stays queued well past the old
        // one-second poll...
        let doc = insert_document(&app).await;
        let mut tx = app.db.write.begin().await.unwrap();
        app.jobs
            .enqueue(&mut tx, JobKind::Process, doc, ProcessOptions::default())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(done.load(Ordering::SeqCst), 0, "an idle worker polled");

        // ...and the wake-up runs it at once, and the next one too.
        let started = std::time::Instant::now();
        app.jobs.wake();
        wait_until(async || done.load(Ordering::SeqCst) == 1).await;
        assert!(started.elapsed() < Duration::from_millis(500));
        enqueue_process(&app).await;
        wait_until(async || done.load(Ordering::SeqCst) == 2).await;
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn a_job_waiting_out_a_backoff_runs_when_it_is_due() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let done = Arc::new(AtomicUsize::new(0));
        let id = enqueue_process(&app).await;
        // As after a failure: queued, due a little later.
        let due = Utc::now() + chrono::Duration::milliseconds(1200);
        sqlx::query("UPDATE jobs SET run_after = ?, attempts = 1 WHERE id = ?")
            .bind(crate::time::ts(due))
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            2,
            cancel.clone(),
            counting_handler(Arc::clone(&done)),
        );

        // Not before it is due, and without any further wake-up, well before
        // the fallback sleep would end.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(done.load(Ordering::SeqCst), 0, "ran before its due time");
        wait_until(async || done.load(Ordering::SeqCst) == 1).await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(1100), "{waited:?}");
        assert!(waited < Duration::from_millis(2500), "{waited:?}");
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn a_fresh_request_does_not_wait_out_an_old_retry_delay() {
        let (_dir, app) = test_app().await;
        let app = Arc::new(app);
        let done = Arc::new(AtomicUsize::new(0));
        let doc = insert_document(&app).await;
        let mut tx = app.db.write.begin().await.unwrap();
        app.jobs
            .enqueue(&mut tx, JobKind::Process, doc, ProcessOptions::default())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Parked for an hour, as after repeated failures.
        let far = Utc::now() + chrono::Duration::hours(1);
        sqlx::query("UPDATE jobs SET run_after = ?")
            .bind(crate::time::ts(far))
            .execute(&app.db.write)
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let handle = Worker::spawn(
            Arc::clone(&app),
            WorkerPool::Ocr,
            1,
            cancel.clone(),
            counting_handler(Arc::clone(&done)),
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(done.load(Ordering::SeqCst), 0);

        // Asking again pulls the job forward (enqueue resets its due time).
        let mut tx = app.db.write.begin().await.unwrap();
        app.jobs
            .enqueue(&mut tx, JobKind::Process, doc, ProcessOptions::default())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        app.jobs.wake();
        wait_until(async || done.load(Ordering::SeqCst) == 1).await;
        stop(cancel, handle).await;
    }

    #[test]
    fn only_non_caller_errors_are_retryable() {
        assert!(is_retryable(ErrorKind::Internal));
        assert!(is_retryable(ErrorKind::Unavailable));
        assert!(is_retryable(ErrorKind::Upstream));
        assert!(!is_retryable(ErrorKind::Invalid));
        assert!(!is_retryable(ErrorKind::NotFound));
    }
}
