//! The worker side: `paperjam extract-worker`.
//!
//! Reads one request from stdin, parses the file it carries and answers on
//! stdout (see [`super::wire`]). It runs without the async runtime, the
//! database or logging: all it has is the file and the parsers, and whatever
//! goes wrong in them ends this process and nothing else.

use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;
use std::time::Instant;

use super::wire::{self, Header, PageText, Request, WireError};
use crate::error::ServiceError;
use crate::extract::pdf::{HayroRenderer, OxideExtractor};
use crate::extract::{PageRenderer, TextExtractor, image as images, quality};

pub fn run() -> ExitCode {
    match serve(
        io::stdin().lock(),
        BufWriter::with_capacity(1 << 20, io::stdout().lock()),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Nothing to tell the server with; it sees the stream end.
            eprintln!("extract-worker: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(mut input: impl Read, mut out: impl Write) -> io::Result<()> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > wire::MAX_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "header too large",
        ));
    }
    let mut header = vec![0u8; len];
    input.read_exact(&mut header)?;
    let header: Header = serde_json::from_slice(&header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    confine();
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    match header.request {
        Request::Pdf {
            ocr_dpi,
            preview_dpi,
        } => pdf(&bytes, ocr_dpi, preview_dpi, &mut out),
        Request::Image { mime, max_edge } => image(&bytes, &mime, max_edge, &mut out),
    }
}

/// Confines the worker before it touches the file: it dies with the server,
/// leaves no core dump of someone's document and cannot gain privileges. Its
/// memory cap is set by the server before exec (see `parent`).
#[cfg(target_os = "linux")]
fn confine() {
    // SAFETY: each call takes plain integers or a pointer to a struct that
    // lives for the call; failures are ignored, the worker is only safer for
    // the ones that work.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
    }
}

/// Other systems have no address-space limit worth the name; the server's
/// timeout still ends a worker that hangs.
#[cfg(not(target_os = "linux"))]
fn confine() {}

fn fail(out: &mut impl Write, e: &ServiceError) -> io::Result<()> {
    let body = serde_json::to_vec(&WireError::from(e)).map_err(io::Error::other)?;
    wire::write_frame(out, wire::FAILED, &[&body])
}

fn image(bytes: &[u8], mime: &str, max_edge: u32, out: &mut impl Write) -> io::Result<()> {
    match images::decode(bytes, mime) {
        Ok(image) => {
            let rgb = images::fit(image, max_edge).into_rgb8();
            let size = [rgb.width().to_le_bytes(), rgb.height().to_le_bytes()].concat();
            wire::write_frame(out, wire::IMAGE, &[&size, rgb.as_raw()])
        }
        Err(e) => fail(out, &e),
    }
}

/// The embedded text of every page, and the density of the scan behind the
/// ones that need reading.
fn texts(bytes: &[u8]) -> Result<Vec<PageText>, ServiceError> {
    let document = OxideExtractor.open(bytes)?;
    Ok((1..=document.page_count())
        .map(|page| {
            let text = document.page_text(page).unwrap_or_default();
            let text = text.trim().to_string();
            let embedded = (!text.is_empty()).then(|| {
                let score = quality::score(&text);
                (text, score)
            });
            let density = quality::needs_ocr(&embedded)
                .then(|| document.page_image_density(page))
                .flatten();
            let (text, score) = embedded.unzip();
            PageText {
                text,
                score: score.unwrap_or_default(),
                density,
            }
        })
        .collect())
}

fn pdf(bytes: &[u8], ocr_dpi: u32, preview_dpi: u32, out: &mut impl Write) -> io::Result<()> {
    let texts = match texts(bytes) {
        Ok(texts) => texts,
        Err(e) => return fail(out, &e),
    };
    let dpis: Vec<u32> = texts
        .iter()
        .map(|t| {
            let embedded = t.text.clone().map(|text| (text, t.score));
            if quality::needs_ocr(&embedded) {
                ocr_dpi
            } else {
                preview_dpi
            }
        })
        .collect();
    let body = serde_json::to_vec(&texts).map_err(io::Error::other)?;
    wire::write_frame(out, wire::TEXT, &[&body])?;
    drop(texts);

    // One page is rendered ahead of what the server has taken; the pipe
    // holds the rest back.
    let pages = match HayroRenderer.open(bytes) {
        Ok(pages) => pages,
        Err(e) => {
            page_failed(out, 1, &e)?;
            return wire::write_frame(out, wire::DONE, &[]);
        }
    };
    for (i, dpi) in dpis.into_iter().enumerate() {
        let n = i as u32 + 1;
        let started = Instant::now();
        match pages.render(n, dpi) {
            Ok(rgb) => {
                let mut head = [0u8; 20];
                head[..4].copy_from_slice(&n.to_le_bytes());
                head[4..8].copy_from_slice(&rgb.width().to_le_bytes());
                head[8..12].copy_from_slice(&rgb.height().to_le_bytes());
                let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                head[12..].copy_from_slice(&micros.to_le_bytes());
                wire::write_frame(out, wire::PAGE, &[&head, rgb.as_raw()])?;
            }
            Err(e) => page_failed(out, n, &e)?,
        }
    }
    wire::write_frame(out, wire::DONE, &[])
}

fn page_failed(out: &mut impl Write, n: u32, e: &ServiceError) -> io::Result<()> {
    let body = serde_json::to_vec(&WireError::from(e)).map_err(io::Error::other)?;
    wire::write_frame(out, wire::PAGE_FAILED, &[&n.to_le_bytes(), &body])
}
