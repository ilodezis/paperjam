//! Backup, restore, export and maintenance commands.

mod common;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use common::{TestEnv, fixture, stderr, stdout};
use paperjam::ids::DocumentId;
use paperjam::jobs::queue::ProcessOptions;
use paperjam::pipeline;
use paperjam::search::SearchRequest;
use paperjam::search::sync;
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, backup, export};

/// Two born-digital PDFs and a text note: processing them needs no OCR.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut docs: Vec<(String, Vec<u8>)> = ["contract_ru_digital.pdf", "contract_ro_digital.pdf"]
        .iter()
        .map(|name| (name.to_string(), std::fs::read(fixture(name)).unwrap()))
        .collect();
    docs.push((
        "note.txt".to_string(),
        "Заметка: договор подписан, копия в папке."
            .as_bytes()
            .to_vec(),
    ));
    docs
}

/// Uploads and processes the corpus.
async fn seed(app: &App) -> Vec<(DocumentId, String, Vec<u8>)> {
    let mut docs = Vec::new();
    for (name, bytes) in corpus() {
        let id = upload(
            &app.ctx(Actor::system()),
            UploadInput {
                original_name: name.clone(),
                ..Default::default()
            },
            bytes.as_slice(),
        )
        .await
        .unwrap()
        .document_id();
        pipeline::process_document(app, id, ProcessOptions::default())
            .await
            .unwrap();
        docs.push((id, name, bytes));
    }
    sync::commit(app).await.unwrap();
    docs
}

async fn hits(app: &App, query: &str) -> Vec<DocumentId> {
    sync::search(
        &app.ctx(Actor::system()),
        SearchRequest {
            query: Some(query.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .hits
    .into_iter()
    .map(|h| h.id)
    .collect()
}

fn files_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[tokio::test]
async fn backup_restore_roundtrip() {
    let source = TestEnv::new();
    let app = source.app().await;
    let docs = seed(&app).await;
    let before = hits(&app, "договор").await;
    assert!(!before.is_empty());

    let snapshot = tempfile::tempdir().unwrap();
    let target = snapshot.path().join("snap");
    let manifest = backup::backup(&app, &target).await.unwrap();
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.documents, 3);
    assert_eq!(manifest.files.len(), 3);
    assert!(target.join("manifest.json").exists());
    assert!(target.join("paperjam.db").exists());
    drop(app);

    // Restore into a different, empty data directory.
    let restored = TestEnv::new();
    backup::restore(&target, restored.data_dir(), false).unwrap();
    let app = restored.app().await;
    sync::ensure_current(&app).await.unwrap();

    assert_eq!(hits(&app, "договор").await, before);
    for (id, name, bytes) in &docs {
        let view = paperjam::service::documents::get_document(
            &app.ctx(Actor::system()),
            *id,
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(&view.file.original_name, name);
        let file = paperjam::service::documents::original_file(&app.ctx(Actor::system()), *id)
            .await
            .unwrap();
        let restored_bytes = std::fs::read(
            restored
                .data_dir()
                .join("originals")
                .join(&file.storage_key),
        )
        .unwrap();
        assert_eq!(&restored_bytes, bytes, "{name}");
    }
}

#[tokio::test]
async fn restore_refuses_a_data_dir_that_is_not_empty() {
    let source = TestEnv::new();
    let app = source.app().await;
    seed(&app).await;
    let snapshot = tempfile::tempdir().unwrap();
    let target = snapshot.path().join("snap");
    backup::backup(&app, &target).await.unwrap();
    drop(app);

    // `source` still holds a database: restoring over it needs --force.
    let err = backup::restore(&target, source.data_dir(), false).unwrap_err();
    assert_eq!(err.code, "data_dir_not_empty");
    backup::restore(&target, source.data_dir(), true).unwrap();
}

#[tokio::test]
async fn backup_refuses_a_target_that_is_not_empty() {
    let env = TestEnv::new();
    let app = env.app().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("something"), b"x").unwrap();
    let err = backup::backup(&app, dir.path()).await.unwrap_err();
    assert_eq!(err.code, "backup_target_not_empty");
}

#[tokio::test]
async fn restore_detects_a_damaged_original() {
    let source = TestEnv::new();
    let app = source.app().await;
    seed(&app).await;
    let snapshot = tempfile::tempdir().unwrap();
    let target = snapshot.path().join("snap");
    backup::backup(&app, &target).await.unwrap();
    drop(app);

    // Damage one original in the snapshot (break a hard link first).
    let victim = files_under(&target.join("originals"))
        .into_keys()
        .next()
        .unwrap();
    let path = target.join("originals").join(&victim);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, bytes).unwrap();

    let restored = TestEnv::new();
    let err = backup::restore(&target, restored.data_dir(), false).unwrap_err();
    assert_eq!(err.code, "backup_corrupt");
}

#[tokio::test]
async fn restore_rejects_invalid_storage_key_in_manifest() {
    let source = TestEnv::new();
    let app = source.app().await;
    seed(&app).await;
    let snapshot = tempfile::tempdir().unwrap();
    let target = snapshot.path().join("snap");
    backup::backup(&app, &target).await.unwrap();
    drop(app);

    // Tamper with manifest.json to introduce a malicious path traversal storage key.
    let manifest_path = target.join("manifest.json");
    let manifest_raw = std::fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
    manifest["files"][0]["storage_key"] = serde_json::json!("../evil");
    std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

    let restored = TestEnv::new();
    let target_parent = restored.data_dir().parent().unwrap().to_path_buf();
    let evil_path = target_parent.join("evil");

    let err = backup::restore(&target, restored.data_dir(), false).unwrap_err();
    assert_eq!(err.code, "backup_corrupt");
    assert!(
        !evil_path.exists(),
        "nothing must be written outside target dir"
    );
}

#[tokio::test]
async fn export_contains_every_original_byte_identical() {
    let env = TestEnv::new();
    let app = env.app().await;
    let docs = seed(&app).await;

    let out = tempfile::tempdir().unwrap();
    let zip_path = out.path().join("export.zip");
    let summary = export::export(&app, &zip_path).await.unwrap();
    assert_eq!(summary.documents, 3);

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
    let names: Vec<String> = archive
        .file_names()
        .filter_map(Result::ok)
        .map(|n| n.to_string())
        .collect();
    assert!(names.contains(&"README.txt".to_string()));
    assert!(names.contains(&"taxonomy.json".to_string()));

    for (id, name, bytes) in &docs {
        let ext = Path::new(name).extension().unwrap().to_str().unwrap();
        let mut original = Vec::new();
        archive
            .by_name(&format!("documents/{id}/original.{ext}"))
            .unwrap()
            .read_to_end(&mut original)
            .unwrap();
        assert_eq!(&original, bytes, "{name}");

        let mut metadata = String::new();
        archive
            .by_name(&format!("documents/{id}/metadata.json"))
            .unwrap()
            .read_to_string(&mut metadata)
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(metadata["file"]["original_name"], name.as_str());
        assert!(metadata["fields"].is_array());
        assert!(
            metadata["text_layers"]
                .as_array()
                .is_some_and(|l| !l.is_empty()),
            "{metadata}"
        );
    }
    assert!(
        names.iter().any(|n| n.contains("/text/1-embedded.txt")),
        "{names:?}"
    );
}

#[tokio::test]
async fn reindex_restores_search_after_index_deleted() {
    let env = TestEnv::new();
    let app = env.app().await;
    let docs = seed(&app).await;
    drop(app);
    std::fs::remove_dir_all(env.data_dir().join("index")).unwrap();

    let out = env.run(&["reindex"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("3"), "{}", stdout(&out));

    let app = env.app().await;
    let found = hits(&app, "договор").await;
    assert!(docs.iter().any(|(id, ..)| found.contains(id)), "{found:?}");
}

#[tokio::test]
async fn cli_backup_and_restore_work_end_to_end() {
    let env = TestEnv::new();
    let app = env.app().await;
    seed(&app).await;
    drop(app);
    let snapshot = tempfile::tempdir().unwrap();
    let target = snapshot.path().join("snap");

    let out = env.run(&["backup", target.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));

    let fresh = TestEnv::new();
    let out = fresh.run(&["restore", target.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Not empty any more: a second restore needs --force.
    let out = fresh.run(&["restore", target.to_str().unwrap()]);
    assert!(!out.status.success());
    let out = fresh.run(&["restore", target.to_str().unwrap(), "--force"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[tokio::test]
async fn reprocess_queues_documents_and_migrate_status_lists_migrations() {
    let env = TestEnv::new();
    let app = env.app().await;
    let docs = seed(&app).await;
    drop(app);

    let out = env.run(&["reprocess", "--all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("queued 3"), "{}", stdout(&out));
    let id = docs[0].0.to_string();
    let out = env.run(&["reprocess", &id]);
    assert!(out.status.success(), "{}", stderr(&out));

    let out = env.run(&["migrate", "status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("0001"), "{}", stdout(&out));

    let app = env.app().await;
    // Re-queued runs of the same document are merged into one job.
    assert_eq!(app.jobs.counts().await.unwrap().queued, 3);
}

/// What older versions wrote as a cached image: lossless WebP.
fn lossless_webp(edge: u32) -> Vec<u8> {
    let image = image::open(fixture("scan_ru.png"))
        .unwrap()
        .thumbnail(edge, edge);
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image.to_rgb8())
        .write_to(&mut out, image::ImageFormat::WebP)
        .unwrap();
    out.into_inner()
}

#[tokio::test]
async fn rerender_rebuilds_old_lossless_images_and_leaves_the_rest() {
    let env = TestEnv::new();
    let app = env.app().await;
    let docs = seed(&app).await;
    let pdf = docs
        .iter()
        .find(|d| d.1 == "contract_ru_digital.pdf")
        .unwrap()
        .0;
    let derived = |name: &str| app.files.derived_path(pdf, name);
    let (page_old, thumb_old) = (lossless_webp(1600), lossless_webp(512));
    std::fs::write(derived("page-1.webp"), &page_old).unwrap();
    std::fs::write(derived("thumbnail.webp"), &thumb_old).unwrap();
    let page_2 = std::fs::read(derived("page-2.webp")).unwrap();
    drop(app);

    let out = env.run(&["rerender"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report = stdout(&out);
    assert!(report.contains("3 documents"), "{report}");
    assert!(report.contains("1 page renders"), "{report}");
    assert!(report.contains("1 thumbnails"), "{report}");

    let app = env.app().await;
    let page = std::fs::read(app.files.derived_path(pdf, "page-1.webp")).unwrap();
    let thumb = std::fs::read(app.files.derived_path(pdf, "thumbnail.webp")).unwrap();
    assert!(page.len() < page_old.len() && thumb.len() < thumb_old.len());
    assert!(!paperjam::pipeline::rerender::is_lossless(&page));
    assert!(!paperjam::pipeline::rerender::is_lossless(&thumb));
    // A file the current version wrote is not touched.
    assert_eq!(
        std::fs::read(app.files.derived_path(pdf, "page-2.webp")).unwrap(),
        page_2
    );

    // Nothing is left to rebuild; one document can be named.
    let out = env.run(&["rerender", &pdf.to_string()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("0 page renders"), "{}", stdout(&out));
}
