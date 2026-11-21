//! The processing pipeline: one `Process` job takes a freshly uploaded (or
//! reprocessed) document from `queued` to `ready` or `failed`.
//!
//! Order: embedded text per page → render/decode, orient, deskew and OCR the
//! pages that need it → thumbnail and page renders → MRZ → local matching →
//! LLM steps → checks → search index → `ready`. The local results of a run are
//! written in one transaction, so a crash never leaves half a document; the
//! LLM steps (spec §7.4) add theirs step by step and never fail the document.
//!
//! A queued job that asks for both parts is split by the handler: the OCR pool
//! does the local part and leaves an LLM-only job for the LLM pool, so waiting
//! on the network never holds up recognition.

pub mod checks;
pub mod local;
pub mod people;
pub mod rerender;
pub(crate) mod store;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::error::{Result, ServiceError};
use crate::ids::DocumentId;
use crate::jobs::queue::{JobKind, ProcessOptions};
use crate::jobs::worker::Handler;
use crate::llm;
use crate::search::sync;
use crate::service::App;

type TrackerEntry = (u64, watch::Receiver<bool>);

/// Coordinates concurrent background OCR completion with LLM steps for the
/// same document.
#[derive(Default)]
pub struct OcrTrackers {
    next_gen: AtomicU64,
    inner: Mutex<HashMap<DocumentId, TrackerEntry>>,
}

impl OcrTrackers {
    /// Registers an active background OCR run for `doc` and returns an RAII
    /// guard that notifies waiters and cleans up on drop.
    pub fn start(self: &Arc<Self>, doc: DocumentId) -> OcrGuard {
        let generation = self.next_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = watch::channel(false);
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(doc, (generation, rx));
        OcrGuard {
            trackers: Arc::clone(self),
            doc,
            generation,
            tx: Some(tx),
        }
    }

    /// Waits for any active background OCR run for `doc` to finish; returns at
    /// once when none is registered.
    pub async fn wait(&self, doc: DocumentId) {
        let mut rx = {
            let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some((_, rx)) = map.get(&doc) else {
                return;
            };
            rx.clone()
        };
        if *rx.borrow() {
            return;
        }
        let _ = rx.wait_for(|done| *done).await;
    }
}

pub struct OcrGuard {
    trackers: Arc<OcrTrackers>,
    doc: DocumentId,
    generation: u64,
    tx: Option<watch::Sender<bool>>,
}

impl Drop for OcrGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(true);
        }
        let mut map = self
            .trackers
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if map
            .get(&self.doc)
            .is_some_and(|(gen_id, _)| *gen_id == self.generation)
        {
            map.remove(&self.doc);
        }
    }
}

/// The handler registered for `JobKind::Process` jobs.
pub fn handler() -> Handler {
    Arc::new(|app, job| {
        Box::pin(async move {
            let Some(id) = job.document_id else {
                return Err(ServiceError::internal(
                    "job_without_document",
                    "a process job has no document",
                ));
            };
            let opts = job.process_options();
            if opts.local && opts.llm && llm::steps::applicable(&app, id).await? {
                return run_split_job(app, id, opts).await;
            }
            process_document(&app, id, opts).await
        })
    })
}

async fn run_split_job(app: Arc<App>, id: DocumentId, opts: ProcessOptions) -> Result<()> {
    store::set_processing(&app, id).await?;
    let prep = async {
        let info = store::load_info(&app, id).await?;
        let prepared = local::prepare(&app, &info.source()).await?;
        store::commit_prepared(
            &app,
            id,
            &info,
            &prepared.output,
            !prepared.pending.is_none(),
        )
        .await?;
        Ok::<_, ServiceError>((info, prepared.pending))
    }
    .await;
    let (info, pending) = match prep {
        Ok(pair) => pair,
        Err(error) => {
            record_failure(&app, id, &error).await;
            return Err(error);
        }
    };
    if pending.is_none() {
        if let Err(error) = enqueue_llm_follow_up(&app, id, &opts).await {
            record_failure(&app, id, &error).await;
            return Err(error);
        }
        return Ok(());
    }
    // Keep the tracker active before waking the LLM pool so any waiter
    // observes the in-flight OCR pass immediately.
    let _guard = app.ocr_trackers.start(id);
    if let Err(error) = enqueue_llm_follow_up(&app, id, &opts).await {
        record_failure(&app, id, &error).await;
        return Err(error);
    }
    if let Err(error) = finish_and_backfill(&app, id, &info, pending).await {
        tracing::warn!(document = %id, code = error.code, "background ocr backfill failed");
    }
    Ok(())
}

async fn finish_and_backfill(
    app: &App,
    id: DocumentId,
    info: &store::DocInfo,
    pending: local::PendingOcr,
) -> Result<()> {
    let ocr_out = local::finish_ocr(app, pending).await?;
    store::commit_ocr_backfill(app, id, info, ocr_out).await
}

async fn record_failure(app: &App, id: DocumentId, error: &ServiceError) {
    tracing::warn!(document = %id, code = error.code, "processing failed");
    if let Err(e) = store::mark_failed(app, id, error).await {
        tracing::error!(document = %id, code = e.code, "could not record the failure");
    }
    sync::refresh(app, id).await;
}

/// Leaves an LLM-only job for the LLM pool. A job queued meanwhile for the
/// same document is newer than this one and is left alone.
async fn enqueue_llm_follow_up(app: &App, id: DocumentId, opts: &ProcessOptions) -> Result<()> {
    let mut tx = app.db.write.begin().await?;
    let queued: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM jobs WHERE document_id = ? AND kind = ? AND state = 'queued'",
    )
    .bind(id)
    .bind(JobKind::Process.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if queued.is_none() {
        let follow_up = ProcessOptions {
            local: false,
            llm: true,
            model_override: opts.model_override.clone(),
        };
        app.jobs
            .enqueue(&mut tx, JobKind::Process, id, follow_up)
            .await?;
    }
    tx.commit().await?;
    app.jobs.wake();
    Ok(())
}

/// Runs the pipeline for one document. On error the document is marked
/// `failed` (reason = the error code, flag `processing_failed`) and the error
/// is returned so the queue can decide about a retry.
pub async fn process_document(app: &App, id: DocumentId, opts: ProcessOptions) -> Result<()> {
    store::set_processing(app, id).await?;
    let result = run(app, id, &opts).await;
    if let Err(error) = &result {
        record_failure(app, id, error).await;
    }
    result
}

async fn run(app: &App, id: DocumentId, opts: &ProcessOptions) -> Result<()> {
    let info = store::load_info(app, id).await?;
    let llm = opts.llm && llm::steps::applicable(app, id).await?;
    if opts.local && llm {
        let prepared = local::prepare(app, &info.source()).await?;
        store::commit_prepared(
            app,
            id,
            &info,
            &prepared.output,
            !prepared.pending.is_none(),
        )
        .await?;
        if !prepared.pending.is_none() {
            let guard = app.ocr_trackers.start(id);
            let backfill = async {
                let _guard = guard;
                finish_and_backfill(app, id, &info, prepared.pending).await
            };
            let (backfill_res, llm_res) = tokio::join!(backfill, llm::steps::run(app, id, opts));
            if let Err(e) = backfill_res {
                tracing::warn!(document = %id, code = e.code, "background ocr backfill failed");
            }
            llm_res?;
        } else {
            llm::steps::run(app, id, opts).await?;
        }
        store::commit(app, id, &info, None, opts, true, false).await?;
        sync::refresh(app, id).await;
        return Ok(());
    }
    let output = if opts.local {
        Some(local::extract(app, &info.source()).await?)
    } else {
        None
    };
    store::commit(app, id, &info, output, opts, !llm, false).await?;
    if llm {
        llm::steps::run(app, id, opts).await?;
        store::commit(app, id, &info, None, opts, true, false).await?;
    }
    sync::refresh(app, id).await;
    Ok(())
}
