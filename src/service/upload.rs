//! Upload: sniff the type, deduplicate by content hash, store the original,
//! create the document and queue its processing.

use serde::Deserialize;
use serde_json::json;
use sqlx::SqliteConnection;
use tokio::io::AsyncRead;

use crate::error::{Result, ServiceError};
use crate::ids::{DocumentId, PersonId, TagId, TypeId};
use crate::jobs::queue::{JobKind, ProcessOptions};
use crate::search::sync;
use crate::service::{Ctx, events};
use crate::time;

/// How many leading bytes are inspected to identify a file.
pub(crate) const SNIFF_BYTES: usize = 8192;
const MAX_NAME_CHARS: usize = 255;
/// ISO base media brands that mean HEIC/HEIF image data.
const HEIC_BRANDS: [&[u8; 4]; 8] = [
    b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs",
];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UploadInput {
    /// Display name only; it is never used to build a path.
    pub original_name: String,
    pub declared_mime: Option<String>,
    #[serde(default)]
    pub local_only: bool,
    #[serde(default)]
    pub tags: Vec<TagId>,
    #[serde(default)]
    pub people: Vec<PersonId>,
    #[serde(default)]
    pub type_id: Option<TypeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    Created(DocumentId),
    /// The same bytes were in the trash; that document is active again.
    Restored(DocumentId),
}

impl UploadOutcome {
    pub fn document_id(self) -> DocumentId {
        match self {
            UploadOutcome::Created(id) | UploadOutcome::Restored(id) => id,
        }
    }
}

fn unsupported() -> ServiceError {
    ServiceError::invalid(
        "unsupported_type",
        "supported files are PDF, JPEG, PNG, WebP, TIFF, HEIC and plain text",
    )
}

fn is_heic(head: &[u8]) -> bool {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return false;
    }
    let brand = &head[8..12];
    if HEIC_BRANDS.iter().any(|b| b.as_slice() == brand) {
        return true;
    }
    // `mif1`/`msf1` are generic image brands (AVIF uses them too): HEIC only
    // if a compatible brand says so.
    if brand == b"mif1" || brand == b"msf1" {
        let boxed = &head[..head.len().min(64)];
        return boxed
            .windows(4)
            .any(|w| HEIC_BRANDS.iter().any(|b| b.as_slice() == w));
    }
    false
}

fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // The sniff window may cut a multi-byte character in half.
        Err(e) => e.error_len().is_none(),
    }
}

/// Markup that browsers execute. Rejected even when it sniffs as text.
fn looks_like_markup(head: &[u8]) -> bool {
    let window = String::from_utf8_lossy(&head[..head.len().min(2048)]).to_lowercase();
    let start = window.trim_start_matches('\u{feff}').trim_start();
    window.contains("<svg")
        || start.starts_with("<?xml")
        || start.starts_with("<!doctype")
        || start.starts_with("<html")
}

/// Identifies a file from its leading bytes. The declared type and the name
/// only break ties (Markdown vs plain text); they are never trusted.
pub fn detect_mime(
    head: &[u8],
    original_name: &str,
    declared: Option<&str>,
) -> Result<&'static str> {
    if head.is_empty() {
        return Err(ServiceError::invalid("empty_file", "the file is empty"));
    }
    if is_heic(head) {
        return Ok("image/heic");
    }
    if let Some(kind) = infer::get(head) {
        return match kind.mime_type() {
            "application/pdf" => Ok("application/pdf"),
            "image/jpeg" => Ok("image/jpeg"),
            "image/png" => Ok("image/png"),
            "image/webp" => Ok("image/webp"),
            "image/tiff" => Ok("image/tiff"),
            _ => Err(unsupported()),
        };
    }
    if looks_like_text(head) && !looks_like_markup(head) {
        let lower = original_name.to_lowercase();
        let markdown = lower.ends_with(".md")
            || lower.ends_with(".markdown")
            || declared.is_some_and(|d| d.eq_ignore_ascii_case("text/markdown"));
        return Ok(if markdown {
            "text/markdown"
        } else {
            "text/plain"
        });
    }
    Err(unsupported())
}

/// Strips control and direction-override characters and caps the length;
/// slashes and dots are kept because the name is only ever displayed.
pub fn clean_display_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
                )
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "document".to_string();
    }
    trimmed.chars().take(MAX_NAME_CHARS).collect()
}

fn duplicate(id: DocumentId) -> ServiceError {
    ServiceError::conflict("duplicate", "this file is already in the archive")
        .with_details(json!({"document_id": id}))
}

/// Checks that every referenced tag, person and type exists.
async fn check_options(tx: &mut SqliteConnection, input: &UploadInput) -> Result<()> {
    for tag in &input.tags {
        exists(
            tx,
            "SELECT 1 FROM tags WHERE id = ?",
            tag.to_string(),
            "tag_not_found",
            "no such tag",
        )
        .await?;
    }
    for person in &input.people {
        exists(
            tx,
            "SELECT 1 FROM people WHERE id = ?",
            person.to_string(),
            "person_not_found",
            "no such person",
        )
        .await?;
    }
    if let Some(ty) = input.type_id {
        exists(
            tx,
            "SELECT 1 FROM document_types WHERE id = ?",
            ty.to_string(),
            "type_not_found",
            "no such document type",
        )
        .await?;
    }
    Ok(())
}

async fn exists(
    tx: &mut SqliteConnection,
    sql: &'static str,
    id: String,
    code: &'static str,
    message: &'static str,
) -> Result<()> {
    let found: Option<(i64,)> = sqlx::query_as(sql).bind(id).fetch_optional(tx).await?;
    found
        .map(|_| ())
        .ok_or_else(|| ServiceError::not_found(code, message))
}

pub async fn upload(
    ctx: &Ctx<'_>,
    input: UploadInput,
    body: impl AsyncRead + Unpin,
) -> Result<UploadOutcome> {
    let app = ctx.app;
    let staged = app
        .files
        .stage_stream(body, app.config.max_upload_bytes)
        .await?;
    let head = staged.head(SNIFF_BYTES).await.map_err(|_| {
        ServiceError::internal("storage_error", "could not read the uploaded file back")
    })?;
    let mime = detect_mime(&head, &input.original_name, input.declared_mime.as_deref())?;
    let name = clean_display_name(&input.original_name);

    // Uploads and purges of identical bytes are serialized from here on.
    let _guard = app.locks.lock(&staged.sha256).await;

    let existing: Option<(DocumentId, Option<String>)> = sqlx::query_as(
        "SELECT f.document_id, d.trashed_at FROM files f \
         JOIN documents d ON d.id = f.document_id WHERE f.sha256 = ?",
    )
    .bind(&staged.sha256)
    .fetch_optional(&app.db.write)
    .await?;

    match existing {
        Some((id, None)) => Err(duplicate(id)),
        Some((id, Some(_))) => {
            let now = time::now_ts();
            let mut tx = app.db.write.begin().await?;
            sqlx::query(
                "UPDATE documents SET trashed_at = NULL, updated_at = ?, updated_by = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(ctx.actor.user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            events::record(
                &mut tx,
                &ctx.actor,
                "document.restore",
                Some(id),
                json!({"via_upload": true}),
            )
            .await?;
            tx.commit().await?;
            sync::refresh(app, id).await;
            Ok(UploadOutcome::Restored(id))
        }
        None => {
            let id = DocumentId::new();
            let now = time::now_ts();
            let mut tx = app.db.write.begin().await?;
            check_options(&mut tx, &input).await?;
            // Only now does the blob enter the store (still under the lock).
            let blob = app.files.commit(staged).await?;

            sqlx::query(
                "INSERT INTO documents \
                 (id, type_id, type_provenance, local_only, status, added_by, updated_by, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 'queued', ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(input.type_id)
            .bind(input.type_id.map(|_| "manual"))
            .bind(input.local_only)
            .bind(ctx.actor.user_id)
            .bind(ctx.actor.user_id)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO files (document_id, sha256, mime, size, original_name, storage_key) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(&blob.sha256)
            .bind(mime)
            .bind(blob.size as i64)
            .bind(&name)
            .bind(&blob.storage_key)
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                    ServiceError::conflict("duplicate", "this file is already in the archive")
                }
                other => other.into(),
            })?;
            for tag in &input.tags {
                sqlx::query(
                    "INSERT OR IGNORE INTO document_tags (document_id, tag_id, provenance) VALUES (?, ?, 'manual')",
                )
                .bind(id)
                .bind(tag)
                .execute(&mut *tx)
                .await?;
            }
            for person in &input.people {
                sqlx::query(
                    "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) VALUES (?, ?, 'manual')",
                )
                .bind(id)
                .bind(person)
                .execute(&mut *tx)
                .await?;
            }
            app.jobs
                .enqueue(&mut tx, JobKind::Process, id, ProcessOptions::default())
                .await?;
            events::record(
                &mut tx,
                &ctx.actor,
                "document.upload",
                Some(id),
                json!({"mime": mime, "size": blob.size}),
            )
            .await?;
            tx.commit().await?;
            sync::refresh(app, id).await;
            app.jobs.wake();
            Ok(UploadOutcome::Created(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::jobs::queue::{JobKind, ProcessOptions};
    use crate::model::fields::Labels;
    use crate::model::taxonomy::{NewPerson, NewType};
    use crate::service::testing::test_app;
    use crate::service::{Actor, App, Via, taxonomy};

    pub(crate) fn pdf(tag: &str) -> Vec<u8> {
        format!("%PDF-1.4\n% {tag}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n").into_bytes()
    }

    fn input(name: &str) -> UploadInput {
        UploadInput {
            original_name: name.to_string(),
            ..UploadInput::default()
        }
    }

    fn user_ctx(app: &App) -> Ctx<'_> {
        app.ctx(Actor::cli())
    }

    async fn up(app: &App, name: &str, bytes: &[u8]) -> Result<UploadOutcome> {
        upload(&user_ctx(app), input(name), bytes).await
    }

    // --- sniffing ---------------------------------------------------------

    #[test]
    fn detects_the_allowed_types() {
        let cases: Vec<(Vec<u8>, &str, &str)> = vec![
            (pdf("x"), "a.pdf", "application/pdf"),
            (
                vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0],
                "a.png",
                "image/png",
            ),
            (
                vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'],
                "a.jpg",
                "image/jpeg",
            ),
            (b"RIFF\x24\0\0\0WEBPVP8 ".to_vec(), "a.webp", "image/webp"),
            (b"II*\0\x08\0\0\0\0\0\0\0".to_vec(), "a.tif", "image/tiff"),
            (b"MM\0*\0\0\0\x08\0\0\0\0".to_vec(), "a.tif", "image/tiff"),
            (
                b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec(),
                "a.heic",
                "image/heic",
            ),
            (
                b"\0\0\0\x18ftypmif1\0\0\0\0mif1heic".to_vec(),
                "a.heif",
                "image/heic",
            ),
            (
                "Просто текст, ёжик.\n".as_bytes().to_vec(),
                "note.txt",
                "text/plain",
            ),
            (b"# Title\n\nbody".to_vec(), "note.md", "text/markdown"),
            (b"# Title\n\nbody".to_vec(), "note", "text/plain"),
        ];
        for (bytes, name, expected) in cases {
            assert_eq!(detect_mime(&bytes, name, None).unwrap(), expected, "{name}");
        }
        // The declared type may only pick markdown for already-textual bytes.
        assert_eq!(
            detect_mime(b"plain", "x", Some("text/markdown")).unwrap(),
            "text/markdown"
        );
        assert_eq!(
            detect_mime(&pdf("x"), "x.md", Some("text/markdown")).unwrap(),
            "application/pdf"
        );
    }

    #[test]
    fn rejects_svg_executables_archives_and_binary_junk() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let svg_with_prolog = b"<?xml version=\"1.0\"?>\n<!-- c -->\n<SVG xmlns=\"x\"></SVG>";
        let cases: Vec<(&[u8], &str)> = vec![
            (svg, "logo.svg"),
            (svg, "logo.png"),
            (svg_with_prolog, "x.txt"),
            (b"MZ\x90\0\x03\0\0\0", "setup.exe"),
            (b"PK\x03\x04\x14\0\0\0\x08\0", "doc.docx"),
            (&[0, 1, 2, 3, 4, 5, 0, 0, 7], "blob.bin"),
        ];
        for (bytes, name) in cases {
            let err = detect_mime(bytes, name, None).unwrap_err();
            assert_eq!(
                (err.kind, err.code),
                (ErrorKind::Invalid, "unsupported_type"),
                "{name}"
            );
        }
    }

    #[test]
    fn empty_file_is_rejected() {
        let err = detect_mime(b"", "a.pdf", None).unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Invalid, "empty_file"));
    }

    #[test]
    fn display_names_lose_control_characters_but_keep_everything_else() {
        assert_eq!(
            clean_display_name("Договор №5 — копия 😀.pdf"),
            "Договор №5 — копия 😀.pdf"
        );
        assert_eq!(clean_display_name("../../etc/passwd"), "../../etc/passwd");
        assert_eq!(clean_display_name("a\0b\r\nc\u{202e}d.pdf"), "abcd.pdf");
        assert_eq!(clean_display_name("   "), "document");
        assert_eq!(clean_display_name(""), "document");
        assert_eq!(clean_display_name(&"x".repeat(400)).chars().count(), 255);
    }

    // --- upload -----------------------------------------------------------

    #[tokio::test]
    async fn upload_creates_queued_document() {
        let (_dir, app) = test_app().await;
        let alice = crate::service::users::create_user(
            &app.ctx(Actor::cli()),
            crate::model::user::NewUser {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: secrecy::SecretString::from("correct horse".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap();
        let ctx = app.ctx(Actor::user(alice.id, Via::Web));

        let outcome = upload(&ctx, input("Contract.pdf"), &pdf("one")[..])
            .await
            .unwrap();
        let UploadOutcome::Created(id) = outcome else {
            panic!("expected Created, got {outcome:?}")
        };

        let (status, added_by, local_only, title): (
            String,
            Option<crate::ids::UserId>,
            bool,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, added_by, local_only, title FROM documents WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        assert_eq!(status, "queued");
        assert_eq!(added_by, Some(alice.id));
        assert!(!local_only, "local_only defaults to false");
        assert_eq!(title, None, "titles come from processing");

        let (mime, size, name, sha, key): (String, i64, String, String, String) = sqlx::query_as(
            "SELECT mime, size, original_name, sha256, storage_key FROM files WHERE document_id = ?",
        )
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        assert_eq!(mime, "application/pdf");
        assert_eq!(size as usize, pdf("one").len());
        assert_eq!(name, "Contract.pdf");
        assert_eq!(sha, crate::hashing::sha256_hex(&pdf("one")));
        // The original is stored byte for byte.
        let mut stored = Vec::new();
        std::io::Read::read_to_end(&mut app.files.open_original(&key).unwrap(), &mut stored)
            .unwrap();
        assert_eq!(stored, pdf("one"));

        // Processing is queued, with everything on.
        let job = app.jobs.claim(&[JobKind::Process]).await.unwrap().unwrap();
        assert_eq!(job.document_id, Some(id));
        assert_eq!(job.process_options(), ProcessOptions::default());

        // The upload is audited.
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM events WHERE action = 'document.upload' AND document_id = ?",
        )
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn upload_options_apply_with_manual_provenance() {
        let (_dir, app) = test_app().await;
        let ctx = user_ctx(&app);
        let person = taxonomy::create_person(
            &ctx,
            NewPerson {
                name: "Marik Testov".into(),
                ..NewPerson::default()
            },
        )
        .await
        .unwrap();
        let tag = taxonomy::create_tag(&ctx, "warranty", None).await.unwrap();
        let ty = taxonomy::create_type(
            &ctx,
            NewType {
                key: "receipt".into(),
                labels: Labels::from([("en".into(), "Receipt".into())]),
                fields: vec![],
            },
        )
        .await
        .unwrap();

        let id = upload(
            &ctx,
            UploadInput {
                original_name: "r.pdf".into(),
                local_only: true,
                tags: vec![tag.id],
                people: vec![person.id],
                type_id: Some(ty.id),
                ..UploadInput::default()
            },
            &pdf("opts")[..],
        )
        .await
        .unwrap()
        .document_id();

        let (local_only, type_id, type_prov): (bool, Option<TypeId>, Option<String>) =
            sqlx::query_as(
                "SELECT local_only, type_id, type_provenance FROM documents WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert!(local_only);
        assert_eq!(type_id, Some(ty.id));
        assert_eq!(type_prov.as_deref(), Some("manual"));
        let (tp,): (String,) =
            sqlx::query_as("SELECT provenance FROM document_tags WHERE document_id = ?")
                .bind(id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        let (pp,): (String,) =
            sqlx::query_as("SELECT provenance FROM document_people WHERE document_id = ?")
                .bind(id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!((tp.as_str(), pp.as_str()), ("manual", "manual"));
    }

    #[tokio::test]
    async fn unknown_option_ids_are_not_found_and_leave_no_trace() {
        let (_dir, app) = test_app().await;
        let ctx = user_ctx(&app);
        for bad in [
            UploadInput {
                tags: vec![TagId::new()],
                ..input("a.pdf")
            },
            UploadInput {
                people: vec![PersonId::new()],
                ..input("a.pdf")
            },
            UploadInput {
                type_id: Some(TypeId::new()),
                ..input("a.pdf")
            },
        ] {
            let err = upload(&ctx, bad, &pdf("bad")[..]).await.unwrap_err();
            assert_eq!(err.kind, ErrorKind::NotFound);
        }
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(n, 0);
        // The same bytes can still be uploaded afterwards.
        assert!(up(&app, "a.pdf", &pdf("bad")).await.is_ok());
    }

    #[tokio::test]
    async fn duplicate_upload_conflicts_with_existing_id() {
        let (_dir, app) = test_app().await;
        let first = up(&app, "a.pdf", &pdf("dup")).await.unwrap().document_id();
        let err = up(&app, "renamed.pdf", &pdf("dup")).await.unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Conflict, "duplicate"));
        assert_eq!(err.details.unwrap()["document_id"], first.to_string());
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn upload_of_trashed_duplicate_restores_it() {
        let (_dir, app) = test_app().await;
        let first = up(&app, "a.pdf", &pdf("trash"))
            .await
            .unwrap()
            .document_id();
        sqlx::query("UPDATE documents SET trashed_at = '2026-01-01T00:00:00.000Z' WHERE id = ?")
            .bind(first)
            .execute(&app.db.write)
            .await
            .unwrap();

        let outcome = up(&app, "a-again.pdf", &pdf("trash")).await.unwrap();

        assert_eq!(outcome, UploadOutcome::Restored(first));
        let (trashed,): (Option<String>,) =
            sqlx::query_as("SELECT trashed_at FROM documents WHERE id = ?")
                .bind(first)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(trashed, None);
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(n, 1, "no copy was created");
    }

    #[tokio::test]
    async fn concurrent_identical_uploads_create_one_document() {
        let (_dir, app) = test_app().await;
        let bytes = pdf("race");
        let ctx = user_ctx(&app);
        let (a, b, c) = tokio::join!(
            upload(&ctx, input("a.pdf"), &bytes[..]),
            upload(&ctx, input("b.pdf"), &bytes[..]),
            upload(&ctx, input("c.pdf"), &bytes[..]),
        );
        let results = [a, b, c];
        let created = results
            .iter()
            .filter(|r| matches!(r, Ok(UploadOutcome::Created(_))))
            .count();
        let conflicts = results
            .iter()
            .filter(|r| matches!(r, Err(e) if e.code == "duplicate"))
            .count();
        assert_eq!((created, conflicts), (1, 2));
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(n, 1);
        let (jobs,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM jobs")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(jobs, 1);
    }

    #[tokio::test]
    async fn svg_is_rejected() {
        let (_dir, app) = test_app().await;
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let err = up(&app, "evil.svg", svg).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Invalid, "unsupported_type")
        );
        // Renaming it does not help.
        assert!(up(&app, "evil.png", svg).await.is_err());
        // And nothing was stored.
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert!(
            std::fs::read_dir(_dir.path().join("tmp"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            std::fs::read_dir(_dir.path().join("originals"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversize_upload_is_rejected_cleanly() {
        let (dir, app) = test_app().await;
        // Shrink the limit for this app.
        let mut config = (*app.config).clone();
        config.max_upload_bytes = 64;
        let small = App {
            config: std::sync::Arc::new(config),
            ..app
        };
        let err = up(&small, "big.pdf", &pdf(&"x".repeat(200)))
            .await
            .unwrap_err();
        assert_eq!(err.code, "file_too_large");
        assert!(
            std::fs::read_dir(dir.path().join("tmp"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[tokio::test]
    async fn empty_and_unsupported_files_are_rejected() {
        let (_dir, app) = test_app().await;
        assert_eq!(up(&app, "a.pdf", b"").await.unwrap_err().code, "empty_file");
        assert_eq!(
            up(&app, "setup.exe", b"MZ\x90\0\x03\0\0\0")
                .await
                .unwrap_err()
                .code,
            "unsupported_type"
        );
    }

    #[tokio::test]
    async fn heic_is_accepted_even_without_the_feature() {
        let (_dir, app) = test_app().await;
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic....";
        assert!(up(&app, "IMG_0001.HEIC", heic).await.is_ok());
    }

    #[tokio::test]
    async fn hostile_file_names_are_only_labels() {
        let (dir, app) = test_app().await;
        let id = up(&app, "../../escape-probe.pdf", &pdf("name"))
            .await
            .unwrap()
            .document_id();
        let (name,): (String,) =
            sqlx::query_as("SELECT original_name FROM files WHERE document_id = ?")
                .bind(id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(name, "../../escape-probe.pdf");
        // The blob lives under its hash, nothing was written outside the store.
        assert!(
            !dir.path()
                .join("..")
                .join("..")
                .join("escape-probe.pdf")
                .exists()
        );
        let stored: Vec<_> = std::fs::read_dir(dir.path().join("originals"))
            .unwrap()
            .collect();
        assert_eq!(stored.len(), 1);
    }
}
