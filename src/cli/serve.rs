//! `paperjam serve`: open everything, start the workers, serve HTTP.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::http::{self, HttpState};
use crate::jobs::queue::{JobKind, WorkerPool};
use crate::jobs::worker::{Handler, Worker};
use crate::pipeline;
use crate::search::sync;
use crate::service::{Actor, App, setup, taxonomy, users};

/// Routes a claimed job to the code that runs it.
pub fn job_handler() -> Handler {
    let process = pipeline::handler();
    Arc::new(move |app, job| match job.kind {
        JobKind::Process => process(app, job),
        JobKind::Relink => Box::pin(async move {
            pipeline::people::relink_all(&app).await?;
            Ok(())
        }),
        JobKind::Reindex => Box::pin(async move {
            match job.document_id {
                Some(id) => sync::refresh(&app, id).await,
                None => {
                    sync::rebuild(&app).await?;
                }
            }
            Ok(())
        }),
    })
}

/// Startup chores that must finish before the first request or job.
pub async fn prepare(app: &App) -> anyhow::Result<()> {
    taxonomy::seed_builtin_types(&app.ctx(Actor::system())).await?;
    let (renamed, merged) = taxonomy::tidy_correspondents(&app.ctx(Actor::system())).await?;
    if renamed > 0 || merged > 0 {
        tracing::info!(renamed, merged, "tidied stored correspondents");
    }
    // Nothing is running yet, so every `running` job is a leftover.
    let reclaimed = app.jobs.reclaim_stale(Duration::ZERO).await?;
    if reclaimed > 0 {
        tracing::info!(reclaimed, "returned interrupted jobs to the queue");
    }
    let known: HashSet<String> = sqlx::query_scalar("SELECT storage_key FROM files")
        .fetch_all(&app.db.read)
        .await?
        .into_iter()
        .collect();
    let swept = app.files.sweep_orphans(&known).await;
    if swept > 0 {
        tracing::info!(swept, "removed orphaned files");
    }
    let expired = users::purge_expired_sessions(app).await?;
    if expired > 0 {
        tracing::info!(expired, "removed expired web sessions");
    }
    sync::ensure_current(app).await?;
    setup::bootstrap(app).await?;
    Ok(())
}

/// Runs the server on `listener` until `shutdown` completes, then stops the
/// workers and flushes the index.
pub async fn serve(
    app: Arc<App>,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    prepare(&app).await?;
    setup::announce(&app, listener.local_addr()?).await?;

    let cancel = CancellationToken::new();
    // Stopped after the inbox task, so a file ingested mid-shutdown still
    // reaches the committer's final commit.
    let index_cancel = CancellationToken::new();
    let committer = app.index.spawn_committer(index_cancel.clone());
    let handler = job_handler();
    let ocr = Worker::spawn(
        Arc::clone(&app),
        WorkerPool::Ocr,
        app.config.ocr_workers,
        cancel.clone(),
        Arc::clone(&handler),
    );
    let llm = Worker::spawn(
        Arc::clone(&app),
        WorkerPool::Llm,
        app.config.llm_workers,
        cancel.clone(),
        handler,
    );
    let inbox = app
        .config
        .inbox_dir
        .is_some()
        .then(|| crate::inbox::spawn(Arc::clone(&app), cancel.clone()));
    app.jobs.wake();

    let router = http::router(HttpState::new(Arc::clone(&app)));
    let served = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await;

    cancel.cancel();
    let _ = tokio::join!(ocr, llm, async {
        if let Some(inbox) = inbox {
            let _ = inbox.await;
        }
    });
    index_cancel.cancel();
    let _ = committer.await;
    served?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

pub async fn run() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let bind = config.bind;
    let app = Arc::new(App::open(config).await?);
    let listener = TcpListener::bind(bind).await?;
    println!("listening on http://{}", listener.local_addr()?);
    serve(app, listener, shutdown_signal()).await
}

/// `paperjam healthcheck`: exit status 0 when `/healthz` answers 200.
pub async fn healthcheck(url: Option<String>) -> anyhow::Result<()> {
    let url = match url {
        Some(url) => url,
        None => {
            let bind = std::env::var("PAPERJAM_BIND").unwrap_or_else(|_| "127.0.0.1:8140".into());
            let port = bind.rsplit(':').next().unwrap_or("8140");
            format!("http://127.0.0.1:{port}/healthz")
        }
    };
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("the server did not answer: {e}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "health check returned {}",
            response.status()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_signal_future_is_valid() {
        tokio::select! {
            _ = shutdown_signal() => panic!("unexpected signal in test"),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}
