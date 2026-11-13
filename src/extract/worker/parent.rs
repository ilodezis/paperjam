//! The server side: runs `paperjam extract-worker` and reads what it says.
//!
//! A worker that hangs is killed once it has been silent for the time limit;
//! one that dies, runs out of memory or answers in any way the protocol does
//! not allow counts as a file that cannot be read, and fails the document
//! with the same code a corrupt file always got. Either way the server is
//! unharmed.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ::image::RgbImage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::wire::{self, Header, PageText, Request, WireError};
use crate::config::Config;
use crate::error::{Result, ServiceError};

/// How long a worker that has said everything gets to exit.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How much of a worker's complaints is kept for the log.
const STDERR_KEPT: usize = 4096;

/// Variables a worker is started with. Nothing else is passed on: a parser
/// has no use for the server's secret key or API keys, and a hostile file
/// that takes over the parser would read them from its environment.
#[cfg(windows)]
const INHERITED_ENV: &[&str] = &["SystemRoot", "windir", "PATH"];
#[cfg(not(windows))]
const INHERITED_ENV: &[&str] = &[];

/// Where and how to run workers.
#[derive(Debug, Clone)]
pub struct Isolation {
    program: PathBuf,
    args: Vec<OsString>,
    timeout: Duration,
    /// Address-space cap for the worker; only Linux can enforce one.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    memory_bytes: Option<u64>,
}

/// A rendered page: its number, the render and how long the worker took.
pub type Rendered = (u32, Result<RgbImage>, Duration);

/// A PDF being read by a worker.
pub struct PdfRun {
    pub texts: Vec<PageText>,
    /// Rendered pages in order. It closes after the last page, or earlier
    /// when the worker ended: the pages that never came did not render.
    pub pages: mpsc::Receiver<Rendered>,
}

impl Isolation {
    pub fn new(config: &Config) -> Self {
        let (program, args) = match &config.extract_worker_program {
            Some(program) => (
                program.clone(),
                config
                    .extract_worker_args
                    .iter()
                    .map(OsString::from)
                    .collect(),
            ),
            None => (
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("paperjam")),
                vec![OsString::from("extract-worker")],
            ),
        };
        Isolation {
            program,
            args,
            timeout: config.extract_timeout,
            memory_bytes: (config.extract_memory_mb > 0)
                .then(|| config.extract_memory_mb.saturating_mul(1024 * 1024)),
        }
    }

    /// Workers run by an explicit command (benchmarks).
    pub fn command(
        program: PathBuf,
        args: Vec<OsString>,
        timeout: Duration,
        memory_bytes: Option<u64>,
    ) -> Self {
        Isolation {
            program,
            args,
            timeout,
            memory_bytes,
        }
    }

    fn spawn(&self, request: Request, bytes: Arc<Vec<u8>>) -> Result<Worker> {
        let header = serde_json::to_vec(&Header { request }).map_err(|_| unavailable())?;
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in INHERITED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(bytes) = self.memory_bytes {
            cap_address_space(&mut command, bytes);
        }
        let mut child = command.spawn().map_err(|e| {
            tracing::error!(error = %e, program = ?self.program, "the extract worker could not be started");
            unavailable()
        })?;
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(unavailable());
        };
        // Feeding the file in the background: a worker that dies without
        // reading it breaks the pipe, which the reading side reports.
        tokio::spawn(async move {
            let sent = async {
                stdin
                    .write_all(&(header.len() as u32).to_le_bytes())
                    .await?;
                stdin.write_all(&header).await?;
                stdin.write_all(&bytes).await?;
                stdin.shutdown().await
            }
            .await;
            if let Err(e) = sent {
                tracing::debug!(error = %e, "the extract worker stopped reading its input");
            }
        });
        Ok(Worker {
            child,
            stdout,
            stderr: drain(stderr),
        })
    }

    /// Decodes an image in a worker: upright, RGB, scaled down to `max_edge`.
    pub async fn decode_image(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        max_edge: u32,
    ) -> Result<RgbImage> {
        let request = Request::Image {
            mime: mime.to_string(),
            max_edge,
        };
        let mut worker = self.spawn(request, Arc::new(bytes))?;
        let image = match timeout(self.timeout, wire::read_frame(&mut worker.stdout)).await {
            Ok(Ok(Some((wire::IMAGE, payload)))) => wire::take_u32s::<2>(&payload)
                .and_then(|([width, height], rgb)| wire::rgb_image(width, height, rgb)),
            Ok(Ok(Some((wire::FAILED, payload)))) => {
                worker.reap().await;
                return Err(reported(&payload, "image_corrupt"));
            }
            Err(_) => {
                worker.kill().await;
                tracing::warn!("an image took too long to decode; its worker was killed");
                return Err(ServiceError::invalid(
                    "image_corrupt",
                    "image decoding timed out",
                ));
            }
            _ => None,
        };
        match image {
            Some(image) => {
                worker.reap().await;
                Ok(image)
            }
            None => Err(worker.died(image_corrupt()).await),
        }
    }

    /// Starts reading a PDF in a worker: returns once the text of every page
    /// is in, while the pages keep rendering behind the returned channel.
    pub async fn open_pdf(
        &self,
        bytes: Arc<Vec<u8>>,
        ocr_dpi: u32,
        preview_dpi: u32,
    ) -> Result<PdfRun> {
        let request = Request::Pdf {
            ocr_dpi,
            preview_dpi,
        };
        let mut worker = self.spawn(request, bytes)?;
        let texts = match timeout(self.timeout, wire::read_frame(&mut worker.stdout)).await {
            Ok(Ok(Some((wire::TEXT, payload)))) => {
                serde_json::from_slice::<Vec<PageText>>(&payload).ok()
            }
            Ok(Ok(Some((wire::FAILED, payload)))) => {
                worker.reap().await;
                return Err(reported(&payload, "pdf_corrupt"));
            }
            Err(_) => {
                worker.kill().await;
                tracing::warn!("a PDF took too long to read; its worker was killed");
                return Err(ServiceError::invalid(
                    "pdf_corrupt",
                    "PDF text extraction timed out",
                ));
            }
            _ => None,
        };
        let Some(texts) = texts else {
            return Err(worker.died(pdf_corrupt()).await);
        };
        // One page waits in the channel and one more in the reader's hand:
        // the worker stays a page or two ahead of the reading, no further.
        let (sender, pages) = mpsc::channel(1);
        tokio::spawn(forward(
            worker,
            sender,
            u32::try_from(texts.len()).unwrap_or(u32::MAX),
            self.timeout,
        ));
        Ok(PdfRun { texts, pages })
    }
}

/// Hands the worker's pages to `sender` as they come.
async fn forward(mut worker: Worker, sender: mpsc::Sender<Rendered>, total: u32, limit: Duration) {
    let mut next = 1u32;
    loop {
        let frame = tokio::select! {
            // Nobody reads any more: dropping the worker kills it.
            () = sender.closed() => return,
            frame = timeout(limit, wire::read_frame(&mut worker.stdout)) => frame,
        };
        let failure = match frame {
            Ok(Ok(Some((wire::PAGE, payload)))) => match page(&payload) {
                Some(rendered) => {
                    next = rendered.0.saturating_add(1);
                    if sender.send(rendered).await.is_err() {
                        return;
                    }
                    continue;
                }
                None => worker.died(pdf_corrupt()).await,
            },
            Ok(Ok(Some((wire::PAGE_FAILED, payload)))) => match page_failure(&payload) {
                Some(rendered) => {
                    next = rendered.0.saturating_add(1);
                    if sender.send(rendered).await.is_err() {
                        return;
                    }
                    continue;
                }
                None => worker.died(pdf_corrupt()).await,
            },
            Ok(Ok(Some((wire::DONE, _)))) => {
                worker.reap().await;
                return;
            }
            Err(_) => {
                worker.kill().await;
                tracing::warn!(
                    page = next,
                    "a PDF page took too long to render; its worker was killed"
                );
                ServiceError::invalid("pdf_corrupt", "PDF page rendering timed out")
            }
            _ => worker.died(pdf_corrupt()).await,
        };
        if next <= total {
            let _ = sender.send((next, Err(failure), Duration::ZERO)).await;
        }
        return;
    }
}

fn page(payload: &[u8]) -> Option<Rendered> {
    let ([n, width, height], rest) = wire::take_u32s::<3>(payload)?;
    let (micros, rgb) = rest.split_at_checked(8)?;
    let micros = u64::from_le_bytes(micros.try_into().ok()?);
    let image = wire::rgb_image(width, height, rgb)?;
    Some((n, Ok(image), Duration::from_micros(micros)))
}

fn page_failure(payload: &[u8]) -> Option<Rendered> {
    let ([n], rest) = wire::take_u32s::<1>(payload)?;
    Some((n, Err(reported(rest, "pdf_corrupt")), Duration::ZERO))
}

/// The error a worker reported, or `fallback` when it is not readable.
fn reported(payload: &[u8], fallback: &'static str) -> ServiceError {
    match serde_json::from_slice::<WireError>(payload) {
        Ok(e) => e.into_service(fallback),
        Err(_) => ServiceError::invalid(fallback, "the file could not be read"),
    }
}

fn pdf_corrupt() -> ServiceError {
    ServiceError::invalid("pdf_corrupt", "the PDF could not be read")
}

fn image_corrupt() -> ServiceError {
    ServiceError::invalid("image_corrupt", "the image could not be read")
}

fn unavailable() -> ServiceError {
    ServiceError::internal(
        "worker_unavailable",
        "the document parser could not be started",
    )
}

/// Caps the worker's address space between fork and exec, so the limit holds
/// from its first instruction. Set later, from inside the worker, it would come
/// after mimalloc had reserved its arena, and allocations carved out of that
/// reservation never meet the limit.
#[cfg(target_os = "linux")]
fn cap_address_space(command: &mut Command, bytes: u64) {
    let cap = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    // SAFETY: the closure runs in the forked child before exec and only calls
    // setrlimit, which is async-signal-safe, on a struct it owns.
    unsafe {
        command.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_AS, &cap) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

struct Worker {
    child: Child,
    stdout: ChildStdout,
    stderr: JoinHandle<Vec<u8>>,
}

impl Worker {
    /// Waits for a worker that has finished; kills one that lingers.
    async fn reap(&mut self) {
        if timeout(EXIT_GRACE, self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// The worker ended without a proper answer (it crashed, ran out of
    /// memory or spoke out of turn). Logs what it said; returns `error`.
    async fn died(mut self, error: ServiceError) -> ServiceError {
        self.kill().await;
        let status = self.child.try_wait().ok().flatten();
        let said = timeout(Duration::from_secs(1), self.stderr)
            .await
            .ok()
            .and_then(|joined| joined.ok())
            .unwrap_or_default();
        tracing::warn!(
            status = ?status,
            stderr = %String::from_utf8_lossy(&said).trim(),
            "the extract worker ended without an answer"
        );
        error
    }
}

/// Reads a worker's stderr to the end, keeping the first [`STDERR_KEPT`]
/// bytes. It must be read: a worker that fills the pipe would stop.
fn drain(mut stderr: ChildStderr) -> JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut kept = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let room = STDERR_KEPT.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        kept
    })
}
