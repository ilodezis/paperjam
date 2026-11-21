//! Untrusted files are parsed in a child process (`paperjam extract-worker`):
//! it gives the same answers as the parsers in this process, and when it
//! hangs, dies or talks nonsense, the document fails and the server lives.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TestEnv, fixture};
use paperjam::config::Config;
use paperjam::error::ErrorKind;
use paperjam::extract::worker::Isolation;
use paperjam::extract::{HayroRenderer, OxideExtractor, PageRenderer, TextExtractor, quality};
use paperjam::ids::DocumentId;
use paperjam::jobs::queue::ProcessOptions;
use paperjam::model::document::{DocumentStatus, GetOptions};
use paperjam::pipeline;
use paperjam::service::taxonomy;
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, documents};

const OCR_DPI: u32 = 300;
const PREVIEW_DPI: u32 = 110;

/// A program that stands in for the worker, to act the way a broken one does.
struct Fake {
    program: &'static str,
    args: Vec<String>,
}

impl Fake {
    fn shell(script: &str) -> Fake {
        if cfg!(windows) {
            Fake {
                program: "powershell.exe",
                args: ["-NoProfile", "-NonInteractive", "-Command", script]
                    .map(String::from)
                    .to_vec(),
            }
        } else {
            Fake {
                program: "/bin/sh",
                args: ["-c", script].map(String::from).to_vec(),
            }
        }
    }

    /// Writes its process id to `pid_file`, then sleeps for ten minutes.
    fn hangs(pid_file: &Path) -> Fake {
        let file = pid_file.display();
        if cfg!(windows) {
            Fake::shell(&format!(
                "$PID | Set-Content -Path '{file}'; Start-Sleep 600"
            ))
        } else {
            Fake::shell(&format!("echo $$ > '{file}'; exec sleep 600"))
        }
    }

    /// Exits at once with a complaint and a failing status.
    fn dies() -> Fake {
        if cfg!(windows) {
            Fake::shell("[Console]::Error.WriteLine('boom'); exit 3")
        } else {
            Fake::shell("echo boom >&2; exit 3")
        }
    }

    /// Says something that is not the protocol and exits cleanly.
    fn talks_nonsense() -> Fake {
        if cfg!(windows) {
            Fake::shell("Write-Output 'definitely not frames'")
        } else {
            Fake::shell("echo definitely not frames")
        }
    }

    fn install(self, config: &mut Config) {
        config.extract_worker_program = Some(PathBuf::from(self.program));
        config.extract_worker_args = self.args;
    }
}

async fn app_with(env: &TestEnv, tune: impl FnOnce(&mut Config)) -> App {
    let mut config = env.config();
    tune(&mut config);
    let app = App::open(config).await.expect("open app");
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    app
}

async fn upload_fixture(app: &App, name: &str) -> DocumentId {
    let bytes = std::fs::read(fixture(name)).unwrap();
    upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: name.to_string(),
            ..Default::default()
        },
        bytes.as_slice(),
    )
    .await
    .unwrap()
    .document_id()
}

async fn status(app: &App, id: DocumentId) -> (DocumentStatus, Option<String>) {
    let doc = documents::get_document(&app.ctx(Actor::system()), id, GetOptions::default())
        .await
        .unwrap();
    (doc.status, doc.status_reason)
}

fn is_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&format!(" {pid} "))
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run kill")
            .success()
    }
}

#[tokio::test]
async fn a_hung_worker_is_killed_on_time_and_the_server_keeps_answering() {
    let env = TestEnv::new();
    let pid_file = env.data_dir().join("worker.pid");
    // Long enough for a cold PowerShell to write its pid file.
    let limit = Duration::from_secs(5);
    let app = Arc::new(
        app_with(&env, |config| {
            config.extract_timeout = limit;
            Fake::hangs(&pid_file).install(config);
        })
        .await,
    );
    let hung = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let other = upload_fixture(&app, "scan_ru.png").await;

    let started = Instant::now();
    let processing = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            pipeline::process_document(&app, hung, ProcessOptions::default()).await
        })
    };

    // While the parser hangs, the server does its other work.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let answering = Instant::now();
    status(&app, other).await;
    assert!(answering.elapsed() < Duration::from_secs(1));
    assert!(!processing.is_finished(), "the worker is still hanging");

    let err = processing.await.unwrap().unwrap_err();
    assert!(started.elapsed() >= limit);
    assert!(started.elapsed() < limit + Duration::from_secs(10));
    assert_eq!(err.code, "pdf_corrupt");
    assert_eq!(err.kind, ErrorKind::Invalid);
    assert_eq!(
        status(&app, hung).await,
        (DocumentStatus::Failed, Some("pdf_corrupt".to_string()))
    );

    // The hung process is gone, not left running beside the server.
    let pid: u32 = std::fs::read_to_string(&pid_file)
        .expect("the fake worker never started")
        .trim()
        .trim_start_matches('\u{feff}')
        .parse()
        .expect("a pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while is_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!is_alive(pid), "worker {pid} was not killed");
}

#[tokio::test]
async fn a_worker_that_dies_fails_the_document_with_the_code_of_a_corrupt_file() {
    let env = TestEnv::new();
    let app = app_with(&env, |config| Fake::dies().install(config)).await;
    let pdf = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let image = upload_fixture(&app, "scan_ru.png").await;

    let err = pipeline::process_document(&app, pdf, ProcessOptions::default())
        .await
        .unwrap_err();
    assert_eq!((err.code, err.kind), ("pdf_corrupt", ErrorKind::Invalid));
    assert_eq!(
        status(&app, pdf).await,
        (DocumentStatus::Failed, Some("pdf_corrupt".to_string()))
    );

    let err = pipeline::process_document(&app, image, ProcessOptions::default())
        .await
        .unwrap_err();
    assert_eq!((err.code, err.kind), ("image_corrupt", ErrorKind::Invalid));
    assert_eq!(
        status(&app, image).await,
        (DocumentStatus::Failed, Some("image_corrupt".to_string()))
    );
}

#[tokio::test]
async fn a_worker_that_breaks_the_protocol_is_a_corrupt_file_too() {
    let env = TestEnv::new();
    let app = app_with(&env, |config| Fake::talks_nonsense().install(config)).await;
    let pdf = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let err = pipeline::process_document(&app, pdf, ProcessOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "pdf_corrupt");
}

#[tokio::test]
async fn a_worker_that_cannot_start_is_the_servers_trouble_and_is_retried() {
    let env = TestEnv::new();
    let app = app_with(&env, |config| {
        config.extract_worker_program = Some(env.data_dir().join("no-such-program"));
        config.extract_worker_args = Vec::new();
    })
    .await;
    let pdf = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let err = pipeline::process_document(&app, pdf, ProcessOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.kind),
        ("worker_unavailable", ErrorKind::Internal)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_worker_starts_with_none_of_the_servers_environment() {
    let env = TestEnv::new();
    let dump = env.data_dir().join("worker.env");
    let app = app_with(&env, |config| {
        Fake::shell(&format!("env > '{}'", dump.display())).install(config);
    })
    .await;
    let pdf = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let _ = pipeline::process_document(&app, pdf, ProcessOptions::default()).await;
    let seen = std::fs::read_to_string(&dump).expect("the fake worker ran");
    for name in ["HOME=", "PAPERJAM_", "OPENROUTER", "SECRET"] {
        assert!(!seen.contains(name), "the worker saw {name}: {seen}");
    }
}

fn isolation(env: &TestEnv) -> Isolation {
    Isolation::new(&env.config())
}

fn in_process_text(bytes: &[u8]) -> Vec<(Option<String>, Option<f32>)> {
    let document = OxideExtractor.open(bytes).unwrap();
    (1..=document.page_count())
        .map(|page| {
            let text = document
                .page_text(page)
                .unwrap_or_default()
                .trim()
                .to_string();
            let embedded = (!text.is_empty()).then(|| {
                let score = quality::score(&text);
                (text, score)
            });
            let density = quality::needs_ocr(&embedded)
                .then(|| document.page_image_density(page))
                .flatten();
            (embedded.map(|(text, _)| text), density)
        })
        .collect()
}

#[tokio::test]
async fn the_worker_reads_pdfs_exactly_as_this_process_does() {
    let env = TestEnv::new();
    let isolation = isolation(&env);
    for name in [
        "contract_ru_digital.pdf",
        "invoice_scan_en.pdf",
        "statement_multi_page.pdf",
        "stamped_scan.pdf",
    ] {
        let bytes = Arc::new(std::fs::read(fixture(name)).unwrap());
        let mut run = isolation
            .open_pdf(Arc::clone(&bytes), OCR_DPI, PREVIEW_DPI)
            .await
            .unwrap();

        let expected = in_process_text(&bytes);
        let got: Vec<_> = run
            .texts
            .iter()
            .map(|t| (t.text.clone(), t.density))
            .collect();
        assert_eq!(got, expected, "{name}: text and scan density");

        let renderer = HayroRenderer.open(&bytes).unwrap();
        for (i, page) in run.texts.clone().iter().enumerate() {
            let n = i as u32 + 1;
            let embedded = page.text.clone().map(|text| (text, page.score));
            let dpi = if quality::needs_ocr(&embedded) {
                paperjam::extract::pdf::effective_ocr_dpi(OCR_DPI, page.density)
            } else {
                PREVIEW_DPI
            };
            let (got_n, rendered, _) = run.pages.recv().await.expect("a page per page");
            assert_eq!(got_n, n, "{name}: pages come in order");
            assert!(
                rendered.unwrap() == renderer.render(n, dpi).unwrap(),
                "{name} page {n}: the render differs"
            );
        }
        assert!(
            run.pages.recv().await.is_none(),
            "{name}: nothing after the last page"
        );
    }
}

#[tokio::test]
async fn a_pdf_that_cannot_be_read_fails_with_its_own_code() {
    let env = TestEnv::new();
    let isolation = isolation(&env);
    let open = |bytes: Vec<u8>| isolation.open_pdf(Arc::new(bytes), OCR_DPI, PREVIEW_DPI);

    // Refused before any page: the worker reports the parser's own error.
    for (bytes, code) in [
        (
            std::fs::read(fixture("encrypted.pdf")).unwrap(),
            "pdf_encrypted",
        ),
        (b"%PDF-1.4 definitely not a pdf".to_vec(), "pdf_corrupt"),
    ] {
        let err = open(bytes).await.err().expect("the PDF opened");
        assert_eq!((err.code, err.kind), (code, ErrorKind::Invalid));
    }

    // A truncated file the parsers may recover from: whatever they make of
    // it, the worker answers (no hang, no crash) and any failure is a
    // corrupt-file code.
    match open(std::fs::read(fixture("corrupt.pdf")).unwrap()).await {
        Err(err) => assert_eq!(err.code, "pdf_corrupt"),
        Ok(mut run) => {
            let (n, rendered, _) = run.pages.recv().await.expect("a verdict on page 1");
            assert_eq!(n, 1);
            if let Err(err) = rendered {
                assert_eq!(err.code, "pdf_corrupt");
            }
        }
    }
}

#[tokio::test]
async fn the_worker_decodes_images_exactly_as_this_process_does() {
    let env = TestEnv::new();
    let isolation = isolation(&env);
    for (name, mime) in [
        ("scan_ru.png", "image/png"),
        ("passport_td3.png", "image/png"),
        ("receipt_photo_skewed.jpg", "image/jpeg"),
    ] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        for max_edge in [3508, 400] {
            let expected = paperjam::extract::image::fit(
                paperjam::extract::image::decode(&bytes, mime).unwrap(),
                max_edge,
            )
            .into_rgb8();
            let got = isolation
                .decode_image(bytes.clone(), mime, max_edge)
                .await
                .unwrap();
            assert!(got == expected, "{name} at {max_edge}: the pixels differ");
        }
    }
}

#[tokio::test]
async fn an_image_that_cannot_be_decoded_fails_with_its_own_code() {
    let env = TestEnv::new();
    let isolation = isolation(&env);
    let err = isolation
        .decode_image(b"definitely not an image".to_vec(), "image/png", 3508)
        .await
        .unwrap_err();
    assert_eq!((err.code, err.kind), ("image_corrupt", ErrorKind::Invalid));
}

#[cfg(not(feature = "heic"))]
#[tokio::test]
async fn heic_without_the_feature_fails_cleanly_through_the_worker() {
    let env = TestEnv::new();
    let bytes = std::fs::read(fixture("photo.heic")).unwrap();
    let err = isolation(&env)
        .decode_image(bytes, "image/heic", 3508)
        .await
        .unwrap_err();
    assert_eq!(err.code, "heic_unsupported");
}

#[cfg(feature = "heic")]
#[tokio::test]
async fn heic_is_decoded_by_the_worker() {
    let env = TestEnv::new();
    let bytes = std::fs::read(fixture("photo.heic")).unwrap();
    let image = isolation(&env)
        .decode_image(bytes, "image/heic", 3508)
        .await
        .unwrap();
    assert!(image.width() > 0 && image.height() > 0);
}

/// An RGB PNG of nothing but black: tiny to store, 243 MB to decode.
#[cfg(target_os = "linux")]
fn black_png(side: u32) -> Vec<u8> {
    use image::ImageEncoder;
    let pixels = vec![0u8; side as usize * side as usize * 3];
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_worker_cannot_grow_past_its_memory_limit() {
    let env = TestEnv::new();
    let bomb = black_png(9000); // 81 megapixels: under the pixel limit
    let mut config = env.config();

    config.extract_memory_mb = 0;
    let free = Isolation::new(&config)
        .decode_image(bomb.clone(), "image/png", 1000)
        .await;
    assert!(free.is_ok(), "without a limit the image decodes");

    config.extract_memory_mb = 128;
    let capped = Isolation::new(&config)
        .decode_image(bomb, "image/png", 1000)
        .await
        .unwrap_err();
    assert_eq!(capped.code, "image_corrupt");
}
