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

use std::sync::Arc;

use crate::error::{Result, ServiceError};
use crate::ids::DocumentId;
use crate::jobs::queue::{JobKind, ProcessOptions};
use crate::jobs::worker::Handler;
use crate::llm;
use crate::search::sync;
use crate::service::App;

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
                let local_only = ProcessOptions {
                    llm: false,
                    ..opts.clone()
                };
                run_document(&app, id, local_only, true).await?;
                return enqueue_llm_follow_up(&app, id, &opts).await;
            }
            process_document(&app, id, opts).await
        })
    })
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
    run_document(app, id, opts, false).await
}

/// [`process_document`]; with `llm_follows` an LLM-only job comes next for
/// the document, so this run is not its end.
async fn run_document(
    app: &App,
    id: DocumentId,
    opts: ProcessOptions,
    llm_follows: bool,
) -> Result<()> {
    store::set_processing(app, id).await?;
    let result = run(app, id, &opts, llm_follows).await;
    if let Err(error) = &result {
        tracing::warn!(document = %id, code = error.code, "processing failed");
        if let Err(e) = store::mark_failed(app, id, error).await {
            tracing::error!(document = %id, code = e.code, "could not record the failure");
        }
        sync::refresh(app, id).await;
    }
    result
}

async fn run(app: &App, id: DocumentId, opts: &ProcessOptions, llm_follows: bool) -> Result<()> {
    let info = store::load_info(app, id).await?;
    let output = if opts.local {
        Some(local::extract(app, &info.source()).await?)
    } else {
        None
    };
    let llm = opts.llm && llm::steps::applicable(app, id).await?;
    // With LLM steps ahead in this run, the local results are stored now and
    // the document becomes `ready` only after them. With the follow-up job
    // ahead it is `ready` now, but its holder becomes a person only once the
    // model has checked the names the zone brought.
    store::commit(app, id, &info, output, opts, !llm, llm_follows).await?;
    if llm {
        llm::steps::run(app, id, opts).await?;
        store::commit(app, id, &info, None, opts, true, false).await?;
    }
    sync::refresh(app, id).await;
    Ok(())
}
