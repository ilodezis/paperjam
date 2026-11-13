//! `paperjam import paperless` against a synthetic Paperless-ngx export.

mod common;

use std::path::Path;

use common::{TestEnv, stderr, stdout};
use paperjam::ids::UserId;
use paperjam::model::document::{GetOptions, Provenance};
use paperjam::model::taxonomy::NewCorrespondent;
use paperjam::model::user::NewUser;
use paperjam::service::paperless::{Report, import, resolve_user};
use paperjam::service::{Actor, App, documents, taxonomy, users};
use serde_json::{Value, json};

fn pdf(tag: &str) -> Vec<u8> {
    format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
}

fn record(model: &str, pk: i64, fields: Value) -> Value {
    json!({"model": model, "pk": pk, "fields": fields})
}

fn document(pk: i64, file: Option<&str>, fields: Value) -> Value {
    let mut r = record("documents.document", pk, fields);
    if let Some(file) = file {
        r["__exported_file_name__"] = json!(file);
        r["__exported_archive_name__"] = json!(format!("archive/{pk}.pdf"));
        r["__exported_thumbnail_name__"] = json!(format!("thumbs/{pk}.webp"));
    }
    r
}

fn vocabulary() -> Vec<Value> {
    vec![
        record("documents.tag", 1, json!({"name": "Taxes"})),
        record("documents.tag", 2, json!({"name": "Home"})),
        record("documents.correspondent", 1, json!({"name": "ACME  Corp"})),
        record("documents.correspondent", 2, json!({"name": "Globex"})),
        record("documents.documenttype", 1, json!({"name": "Invoice"})),
    ]
}

/// Writes a small export: two importable documents, a repeat of the first,
/// one whose file is missing, one deleted in Paperless, and one note.
fn write_export(dir: &Path, split: bool) {
    std::fs::create_dir_all(dir.join("originals")).unwrap();
    std::fs::write(dir.join("originals/a.pdf"), pdf("a")).unwrap();
    std::fs::write(dir.join("originals/copy.pdf"), pdf("a")).unwrap();
    std::fs::write(dir.join("originals/b.txt"), "plain text b\n").unwrap();
    std::fs::write(dir.join("originals/gone.pdf"), pdf("gone")).unwrap();
    std::fs::remove_file(dir.join("originals/gone.pdf")).unwrap();

    let doc1 = document(
        1,
        Some("originals/a.pdf"),
        json!({"title": "Water bill", "created": "2023-04-05", "correspondent": 1,
               "tags": [1, 2], "document_type": 1, "original_filename": "water.pdf",
               "mime_type": "application/pdf", "deleted_at": null}),
    );
    let mut records = vocabulary();
    records.push(doc1.clone());
    records.push(document(
        2,
        Some("originals/b.txt"),
        json!({"title": "Notes", "created": "2022-01-02T10:00:00+01:00", "correspondent": 2,
               "tags": [1], "document_type": null}),
    ));
    records.push(document(
        3,
        Some("originals/gone.pdf"),
        json!({"title": "Lost", "created": "2020-01-01"}),
    ));
    records.push(document(
        4,
        Some("originals/copy.pdf"),
        json!({"title": "Water bill again", "created": "2023-04-06"}),
    ));
    records.push(document(
        5,
        Some("originals/a.pdf"),
        json!({"title": "Trashed", "deleted_at": "2024-01-01T00:00:00Z"}),
    ));
    let note = record(
        "documents.note",
        1,
        json!({"note": "private remark", "document": 1}),
    );
    if split {
        std::fs::write(
            dir.join("originals/a-manifest.json"),
            serde_json::to_vec(&json!([doc1, note])).unwrap(),
        )
        .unwrap();
    } else {
        records.push(note);
    }
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&records).unwrap(),
    )
    .unwrap();
}

async fn admin(app: &App, name: &str, is_admin: bool) -> UserId {
    users::create_user(
        &app.ctx(Actor::cli()),
        NewUser {
            username: name.to_string(),
            display_name: name.to_string(),
            password: secrecy::SecretString::from("correct horse battery".to_string()),
            locale: "en".to_string(),
            is_admin,
        },
    )
    .await
    .unwrap()
    .id
}

async fn counts(app: &App) -> (i64, i64, i64) {
    let one = |sql: &'static str| async move {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(&app.db.read)
            .await
            .unwrap()
    };
    (
        one("SELECT COUNT(*) FROM documents").await,
        one("SELECT COUNT(*) FROM tags").await,
        one("SELECT COUNT(*) FROM correspondents").await,
    )
}

async fn run(app: &App, dir: &Path, user: UserId, dry: bool) -> Report {
    import(app, dir, user, dry).await.unwrap()
}

#[tokio::test]
async fn imports_the_originals_with_curated_metadata() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    // Already known under a differently spaced and cased name.
    taxonomy::create_correspondent(
        &app.ctx(Actor::cli()),
        NewCorrespondent {
            name: "Acme Corp".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    taxonomy::create_tag(&app.ctx(Actor::cli()), "taxes", None)
        .await
        .unwrap();
    let export = tempfile::tempdir().unwrap();
    write_export(export.path(), false);

    let r = run(&app, export.path(), user, false).await;
    assert_eq!(
        (r.imported, r.duplicates, r.trashed_in_paperless),
        (2, 1, 1)
    );
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].0, 3);
    assert_eq!(r.tags_created, 2); // Home, paperless:Invoice
    assert_eq!(r.correspondents_created, 1); // Globex
    assert_eq!(r.notes_dropped, 1);

    let ctx = app.ctx(Actor::cli());
    let id: paperjam::ids::DocumentId = sqlx::query_scalar(
        "SELECT d.id FROM documents d JOIN files f ON f.document_id = d.id WHERE f.original_name = 'water.pdf'",
    )
    .fetch_one(&app.db.read)
    .await
    .unwrap();
    let doc = documents::get_document(&ctx, id, GetOptions::default())
        .await
        .unwrap();
    // The original is stored as is, and the metadata is protected like hand edits.
    assert_eq!(doc.file.sha256, paperjam::hashing::sha256_hex(&pdf("a")));
    assert_eq!(doc.title.as_deref(), Some("Water bill"));
    assert_eq!(doc.title_provenance, Some(Provenance::Manual));
    assert_eq!(doc.document_date.unwrap().to_string(), "2023-04-05");
    assert_eq!(doc.date_provenance, Some(Provenance::Manual));
    assert_eq!(doc.added_by, Some(user));
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].name, "Acme Corp");
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Manual));
    let mut tags: Vec<_> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    tags.sort();
    assert_eq!(tags, ["Home", "paperless:Invoice", "taxes"]);
    assert!(doc.tags.iter().all(|t| t.provenance == Provenance::Manual));

    // The time zone suffix does not move the date.
    let date: String =
        sqlx::query_scalar("SELECT document_date FROM documents WHERE title = 'Notes'")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert_eq!(date, "2022-01-02");

    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    assert_eq!(jobs, 2);
}

#[tokio::test]
async fn rerun_skips_everything_and_creates_nothing() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    let export = tempfile::tempdir().unwrap();
    write_export(export.path(), false);

    run(&app, export.path(), user, false).await;
    let before = counts(&app).await;
    let r = run(&app, export.path(), user, false).await;
    assert_eq!((r.imported, r.duplicates), (0, 3));
    assert_eq!((r.tags_created, r.correspondents_created), (0, 0));
    assert_eq!(counts(&app).await, before);
}

#[tokio::test]
async fn dry_run_writes_nothing_and_predicts_the_run() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    let export = tempfile::tempdir().unwrap();
    write_export(export.path(), false);

    let r = run(&app, export.path(), user, true).await;
    assert_eq!(counts(&app).await, (0, 0, 0));
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    assert_eq!(stored, 0);
    let real = run(&app, export.path(), user, false).await;
    assert_eq!(
        (
            r.imported,
            r.duplicates,
            r.tags_created,
            r.correspondents_created
        ),
        (
            real.imported,
            real.duplicates,
            real.tags_created,
            real.correspondents_created
        )
    );
    assert_eq!(r.failed, real.failed);
}

#[tokio::test]
async fn split_manifest_is_read_once() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    let export = tempfile::tempdir().unwrap();
    write_export(export.path(), true);

    let r = run(&app, export.path(), user, false).await;
    assert_eq!((r.imported, r.duplicates, r.notes_dropped), (2, 1, 1));
}

#[tokio::test]
async fn unsafe_paths_and_bad_exports_are_refused() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;

    let export = tempfile::tempdir().unwrap();
    std::fs::write(
        export.path().join("manifest.json"),
        serde_json::to_vec(&[document(1, Some("../outside.pdf"), json!({"title": "x"}))]).unwrap(),
    )
    .unwrap();
    let r = run(&app, export.path(), user, false).await;
    assert_eq!((r.imported, r.failed.len()), (0, 1));

    let empty = tempfile::tempdir().unwrap();
    assert!(import(&app, empty.path(), user, false).await.is_err());
    let zip = empty.path().join("export.zip");
    std::fs::write(&zip, b"PK").unwrap();
    let err = import(&app, &zip, user, false).await.unwrap_err();
    assert!(err.to_string().contains("unzip"));

    std::fs::write(export.path().join("metadata.json"), r#"{"__crypto__": {}}"#).unwrap();
    let err = import(&app, export.path(), user, false).await.unwrap_err();
    assert!(err.to_string().contains("encrypted"));
}

#[tokio::test]
async fn the_user_is_the_named_one_or_the_only_admin() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert!(resolve_user(&app, None).await.is_err());
    let alice = admin(&app, "alice", true).await;
    let bob = admin(&app, "bob", false).await;
    assert_eq!(resolve_user(&app, None).await.unwrap(), alice);
    assert_eq!(resolve_user(&app, Some("bob")).await.unwrap(), bob);
    assert!(resolve_user(&app, Some("nobody")).await.is_err());
    admin(&app, "carol", true).await;
    assert!(resolve_user(&app, None).await.is_err());
}

#[test]
fn the_command_reports_and_exits_nonzero_on_failures() {
    let env = TestEnv::new();
    let out = env.run_with_password(
        &["user", "add", "alice", "--admin"],
        "correct horse battery",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let export = tempfile::tempdir().unwrap();
    write_export(export.path(), false);
    let dir = export.path().to_str().unwrap();

    let out = env.run(&["import", "paperless", dir, "--dry-run"]);
    let text = stdout(&out);
    assert!(text.contains("would import: 2"), "{text}");
    assert!(text.contains("document 3:"), "{text}");
    assert!(!text.contains("Water"), "titles are never printed");
    assert!(!out.status.success());

    let out = env.run(&["import", "paperless", dir]);
    assert!(stdout(&out).contains("imported: 2"));
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_leaving_the_export_are_refused() {
    use std::os::unix::fs::symlink;
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.pdf"), pdf("secret")).unwrap();
    std::fs::write(
        outside.path().join("x-manifest.json"),
        serde_json::to_vec(&[document(
            9,
            Some("originals/in.pdf"),
            json!({"title": "Hidden"}),
        )])
        .unwrap(),
    )
    .unwrap();

    let export = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(export.path().join("originals")).unwrap();
    std::fs::write(export.path().join("originals/in.pdf"), pdf("in")).unwrap();
    symlink(
        outside.path().join("secret.pdf"),
        export.path().join("originals/link.pdf"),
    )
    .unwrap();
    symlink(outside.path(), export.path().join("linked")).unwrap();
    std::fs::write(
        export.path().join("manifest.json"),
        serde_json::to_vec(&[
            document(1, Some("originals/link.pdf"), json!({"title": "Link"})),
            document(2, Some("linked/secret.pdf"), json!({"title": "Dir"})),
            document(3, Some("originals/in.pdf"), json!({"title": "Fine"})),
        ])
        .unwrap(),
    )
    .unwrap();

    let r = run(&app, export.path(), user, false).await;
    assert_eq!(r.imported, 1);
    let mut failed = r.failed.clone();
    failed.sort();
    assert_eq!(
        failed,
        [
            (1, "the file path points outside the export".to_string()),
            (2, "the file path points outside the export".to_string()),
        ]
    );
    // The sidecar behind the symlinked directory was not read.
    assert_eq!(counts(&app).await.0, 1);
}

#[tokio::test]
async fn a_refused_tag_does_not_stop_the_other_metadata() {
    let env = TestEnv::new();
    let app = env.app().await;
    let user = admin(&app, "alice", true).await;
    let export = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(export.path().join("originals")).unwrap();
    std::fs::write(export.path().join("originals/a.pdf"), pdf("a")).unwrap();
    let long = "x".repeat(200);
    let mut records = vec![
        record("documents.tag", 1, json!({"name": long})),
        record("documents.tag", 2, json!({"name": "Good Tag"})),
    ];
    records.push(document(
        1,
        Some("originals/a.pdf"),
        json!({"title": "Kept", "created": "2021-02-03", "tags": [1, 2]}),
    ));
    std::fs::write(
        export.path().join("manifest.json"),
        serde_json::to_vec(&records).unwrap(),
    )
    .unwrap();

    let dry = run(&app, export.path(), user, true).await;
    assert_eq!((dry.tags_created, dry.tags_skipped), (1, 1));

    let r = run(&app, export.path(), user, false).await;
    assert_eq!((r.imported, r.tags_created, r.tags_skipped), (1, 1, 1));
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].0, 1);
    assert!(r.failed[0].1.starts_with("stored, but"));

    let ctx = app.ctx(Actor::cli());
    let id: paperjam::ids::DocumentId = sqlx::query_scalar("SELECT id FROM documents")
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    let doc = documents::get_document(&ctx, id, GetOptions::default())
        .await
        .unwrap();
    assert_eq!(doc.title.as_deref(), Some("Kept"));
    assert_eq!(doc.document_date.unwrap().to_string(), "2021-02-03");
    let tags: Vec<_> = doc.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tags, ["Good Tag"]);
}
