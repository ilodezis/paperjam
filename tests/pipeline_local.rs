//! The local processing pipeline end to end, against the synthetic corpus.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{TestEnv, fixture, have_tesseract};
use paperjam::extract::tesseract::Tesseract;
use paperjam::ids::DocumentId;
use paperjam::jobs::queue::{ProcessOptions, WorkerPool};
use paperjam::jobs::worker::Worker;
use paperjam::model::document::{
    DocumentPatch, DocumentStatus, DocumentView, GetOptions, OrganizationLink, Provenance, Role,
};
use paperjam::model::fields::FieldValue;
use paperjam::model::taxonomy::{NewCorrespondent, NewPerson};
use paperjam::pipeline;
use paperjam::service::taxonomy;
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, documents};
use tokio_util::sync::CancellationToken;

async fn app_with_types(env: &TestEnv) -> App {
    let app = env.app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    app
}

async fn upload_fixture(app: &App, name: &str) -> DocumentId {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let outcome = upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: name.to_string(),
            ..Default::default()
        },
        bytes.as_slice(),
    )
    .await
    .unwrap();
    outcome.document_id()
}

async fn view(app: &App, id: DocumentId) -> DocumentView {
    documents::get_document(&app.ctx(Actor::system()), id, GetOptions::default())
        .await
        .unwrap()
}

fn field<'a>(
    view: &'a DocumentView,
    key: &str,
) -> Option<&'a paperjam::model::document::FieldEntry> {
    view.fields.iter().find(|f| f.key == key)
}

async fn layer_sources(app: &App, id: DocumentId) -> Vec<String> {
    sqlx::query_scalar("SELECT DISTINCT source FROM text_layers WHERE document_id = ? ORDER BY 1")
        .bind(id)
        .fetch_all(&app.db.read)
        .await
        .unwrap()
}

#[tokio::test]
async fn born_digital_pdf_needs_no_ocr() {
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "contract_ru_digital.pdf").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.title.as_deref(), Some("contract_ru_digital"));
    assert_eq!(doc.title_provenance, Some(Provenance::Local));
    assert_eq!(doc.file.page_count, Some(2));
    assert!(doc.attention.is_empty(), "attention: {:?}", doc.attention);
    assert_eq!(layer_sources(&app, id).await, vec!["embedded"]);
    let text = documents::get_document(
        &app.ctx(Actor::system()),
        id,
        GetOptions { text_page: Some(1) },
    )
    .await
    .unwrap()
    .text
    .unwrap();
    assert!(text.text.contains("Договор аренды"), "got: {}", text.text);
    assert_eq!(text.source, "embedded");
    assert!(app.files.derived_path(id, "thumbnail.webp").exists());
    assert!(app.files.derived_path(id, "page-1.webp").exists());
}

#[tokio::test]
async fn image_only_pdf_goes_through_ocr() {
    if !have_tesseract(&["eng", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "invoice_scan_en.pdf").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(layer_sources(&app, id).await, vec!["tesseract"]);
}

#[tokio::test]
async fn a_scan_whose_only_text_is_a_scanner_stamp_is_read_by_ocr() {
    if !have_tesseract(&["eng", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "stamped_scan.pdf").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    assert_eq!(layer_sources(&app, id).await, vec!["embedded", "tesseract"]);
    let text = documents::get_document(
        &app.ctx(Actor::system()),
        id,
        GetOptions { text_page: Some(1) },
    )
    .await
    .unwrap()
    .text
    .unwrap();
    assert_eq!(text.source, "tesseract");
    assert!(text.text.contains("Quarterly"), "got: {}", text.text);
    // The stamp no longer stands for the page, so the LLM transcribes it.
    let score: f64 = sqlx::query_scalar(
        "SELECT quality_score FROM text_layers WHERE document_id = ? AND source = 'embedded'",
    )
    .bind(id)
    .fetch_one(&app.db.read)
    .await
    .unwrap();
    assert!(
        score < f64::from(paperjam::extract::quality::GOOD_THRESHOLD),
        "{score}"
    );
}

#[tokio::test]
async fn text_file_becomes_an_embedded_layer() {
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let outcome = upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: "notes.md".into(),
            ..Default::default()
        },
        "# Заметки\nКупить молоко".as_bytes(),
    )
    .await
    .unwrap();
    let id = outcome.document_id();

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.title.as_deref(), Some("notes"));
    assert_eq!(layer_sources(&app, id).await, vec!["embedded"]);
}

#[tokio::test]
async fn scanned_passport_gets_mrz_fields() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "passport_td3.png").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready, "{:?}", doc.status_reason);
    assert_eq!(doc.type_key.as_deref(), Some("travel_passport"));
    assert_eq!(doc.type_provenance, Some(Provenance::Mrz));
    for key in ["number", "expiry_date", "birth_date", "surname"] {
        let entry = field(&doc, key).unwrap_or_else(|| panic!("no {key}: {:?}", doc.fields));
        assert_eq!(entry.provenance, Provenance::Mrz, "{key}");
    }
    assert!(matches!(
        field(&doc, "number").unwrap().value,
        FieldValue::Identifier(_)
    ));
    assert_eq!(
        field(&doc, "surname").unwrap().value,
        FieldValue::Text("TESTOV".into())
    );
    assert!(doc.attention.is_empty(), "attention: {:?}", doc.attention);
}

#[tokio::test]
async fn id_card_gets_mrz_fields() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "id_card_td1.png").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.type_key.as_deref(), Some("id_card"));
    assert!(field(&doc, "number").is_some(), "{:?}", doc.fields);
}

#[tokio::test]
async fn bad_mrz_check_digits_flag_the_document() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "passport_td3_badcheck.png").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert!(
        doc.attention.contains(&"checksum_failed".to_string()),
        "attention: {:?}",
        doc.attention
    );
    assert!(
        field(&doc, "number").is_none(),
        "unverified MRZ must not fill fields"
    );
}

#[tokio::test]
async fn reprocess_keeps_manual_values() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "passport_td3.png").await;
    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let patch: DocumentPatch = serde_json::from_value(serde_json::json!({
        "title": "My passport",
        "fields": {"number": {"kind": "identifier", "value": "ZZ0000001"}}
    }))
    .unwrap();
    documents::update_document(&app.ctx(Actor::system()), id, patch)
        .await
        .unwrap();

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.title.as_deref(), Some("My passport"));
    assert_eq!(doc.title_provenance, Some(Provenance::Manual));
    let number = field(&doc, "number").unwrap();
    assert_eq!(number.provenance, Provenance::Manual);
    assert_eq!(number.value, FieldValue::Identifier("ZZ0000001".into()));
    // Values the user did not touch are refreshed from the MRZ again.
    assert_eq!(field(&doc, "surname").unwrap().provenance, Provenance::Mrz);
}

#[tokio::test]
async fn bad_personal_number_sets_checksum_failed() {
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "contract_ru_digital.pdf").await;
    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();
    let ctx = app.ctx(Actor::system());
    let passport = taxonomy::list_taxonomy(&ctx)
        .await
        .unwrap()
        .types
        .into_iter()
        .find(|t| t.key == "passport")
        .unwrap();
    sqlx::query("UPDATE documents SET type_id = ?, type_provenance = 'manual' WHERE id = ?")
        .bind(passport.id)
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
    // A Romanian CNP whose check digit is wrong (the correct one is 4).
    sqlx::query(
        "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) \
         VALUES (?, 'personal_number', ?, 'local', '2026-01-01T00:00:00Z')",
    )
    .bind(id)
    .bind(r#"{"kind":"identifier","value":"1800101221149"}"#)
    .execute(&app.db.write)
    .await
    .unwrap();

    pipeline::process_document(
        &app,
        id,
        ProcessOptions {
            local: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        view(&app, id)
            .await
            .attention
            .contains(&"checksum_failed".to_string())
    );

    // Editing the field clears the flag at once.
    let patch: DocumentPatch = serde_json::from_value(serde_json::json!({
        "fields": {"personal_number": {"kind": "identifier", "value": "1800101221144"}}
    }))
    .unwrap();
    let edited = documents::update_document(&ctx, id, patch).await.unwrap();
    assert!(
        edited.attention.is_empty(),
        "attention: {:?}",
        edited.attention
    );
}

#[tokio::test]
async fn failure_marks_document_failed() {
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let id = upload_fixture(&app, "encrypted.pdf").await;

    let err = pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code, "pdf_encrypted");

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Failed);
    assert_eq!(doc.status_reason.as_deref(), Some("pdf_encrypted"));
    assert_eq!(doc.attention, vec!["processing_failed".to_string()]);
    // The original is kept.
    assert!(
        app.files
            .open_original(&storage_key(&app, id).await)
            .is_ok()
    );
}

async fn storage_key(app: &App, id: DocumentId) -> String {
    sqlx::query_scalar("SELECT storage_key FROM files WHERE document_id = ?")
        .bind(id)
        .fetch_one(&app.db.read)
        .await
        .unwrap()
}

#[tokio::test]
async fn local_matching_finds_people_and_correspondents() {
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    let ctx = app.ctx(Actor::system());
    let person = taxonomy::create_person(
        &ctx,
        NewPerson {
            name: "Ivan Testov".into(),
            aliases: vec!["Иван Тестов".into()],
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&app, "contract_ru_digital.pdf").await;
    let text = text_of(&app, id).await;

    // The fixture text names nobody, so a person that is not in it does not match.
    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();
    assert!(view(&app, id).await.people.is_empty(), "text: {text}");

    // A correspondent whose name appears in the text is picked up.
    let word = text
        .split_whitespace()
        .find(|w| w.chars().count() >= 6 && w.chars().all(char::is_alphabetic))
        .unwrap()
        .to_string();
    let corr = taxonomy::create_correspondent(
        &ctx,
        NewCorrespondent {
            name: word,
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();
    let doc = view(&app, id).await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].id, corr.id);
    assert_eq!(doc.organizations[0].role, Role::Unspecified);
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Local));

    // Organizations and people set by hand are never touched.
    let patch = DocumentPatch {
        organizations: Some(vec![OrganizationLink {
            id: corr.id,
            role: Role::IssuedBy,
        }]),
        people: Some(vec![person.id]),
        ..Default::default()
    };
    documents::update_document(&ctx, id, patch).await.unwrap();
    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();
    let doc = view(&app, id).await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].role, Role::IssuedBy);
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Manual));
    assert_eq!(doc.people.len(), 1);
    assert_eq!(doc.people[0].provenance, Provenance::Manual);
}

async fn text_of(app: &App, id: DocumentId) -> String {
    pipeline::process_document(app, id, ProcessOptions::default())
        .await
        .unwrap();
    documents::get_document(
        &app.ctx(Actor::system()),
        id,
        GetOptions { text_page: Some(1) },
    )
    .await
    .unwrap()
    .text
    .unwrap()
    .text
}

#[tokio::test]
async fn workers_process_uploads_through_the_job_queue() {
    let env = TestEnv::new();
    let app = Arc::new(app_with_types(&env).await);
    let cancel = CancellationToken::new();
    let worker = Worker::spawn(
        Arc::clone(&app),
        WorkerPool::Ocr,
        1,
        cancel.clone(),
        pipeline::handler(),
    );
    let id = upload_fixture(&app, "contract_ru_digital.pdf").await;
    app.jobs.wake();

    let mut status = DocumentStatus::Queued;
    for _ in 0..100 {
        status = view(&app, id).await.status;
        if status == DocumentStatus::Ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cancel.cancel();
    worker.await.unwrap();
    assert_eq!(status, DocumentStatus::Ready);
}

#[tokio::test]
async fn sideways_passport_on_a4_is_found_by_its_mrz() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    // Orientation detection gives up on this sheet: the passport is small and
    // the sheet lies a quarter turn clockwise.
    let id = upload_fixture(&app, "passport_td3_sideways_a4.png").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready, "{:?}", doc.status_reason);
    assert_eq!(doc.type_key.as_deref(), Some("travel_passport"));
    for key in ["number", "expiry_date", "birth_date", "surname"] {
        let entry = field(&doc, key).unwrap_or_else(|| panic!("no {key}: {:?}", doc.fields));
        assert_eq!(entry.provenance, Provenance::Mrz, "{key}");
    }
    // The page that was found by turning it is cached upright and cut out of
    // the sheet: a passport lying down, not an A4 page with one in a corner.
    let (width, height): (i64, i64) =
        sqlx::query_as("SELECT width, height FROM pages WHERE document_id = ? AND n = 1")
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert!(width > height, "render is {width}x{height}");
    assert!(
        width * height < 1240 * 1754 * 6 / 10,
        "render is {width}x{height}"
    );
    // The lossy render is small and the fields can still be read off it.
    let render = std::fs::read(app.files.derived_path(id, "page-1.webp")).unwrap();
    assert!(render.len() <= 150_000, "render is {} bytes", render.len());
    let thumbnail = std::fs::read(app.files.derived_path(id, "thumbnail.webp")).unwrap();
    assert!(
        thumbnail.len() <= 20_000,
        "thumbnail is {} bytes",
        thumbnail.len()
    );
    let page = image::load_from_memory(&render).unwrap();
    let ocr = Tesseract::new("eng").ocr(&page).await.unwrap();
    assert!(
        ocr.text.contains("12345675UT08503127M3209011"),
        "got: {}",
        ocr.text
    );
}

#[tokio::test]
async fn sideways_id_card_on_a4_is_cut_out_turned_and_read() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_types(&env).await;
    // A card the size of a card on an A4 sheet with dust on the glass, laid
    // down a quarter turn counter-clockwise.
    let id = upload_fixture(&app, "id_card_td1_sideways_a4.png").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready, "{:?}", doc.status_reason);
    assert_eq!(doc.type_key.as_deref(), Some("id_card"));
    for key in ["number", "expiry_date", "birth_date"] {
        let entry = field(&doc, key).unwrap_or_else(|| panic!("no {key}: {:?}", doc.fields));
        assert_eq!(entry.provenance, Provenance::Mrz, "{key}");
    }
    // The page shows the card alone, the right way up.
    let (width, height): (i64, i64) =
        sqlx::query_as("SELECT width, height FROM pages WHERE document_id = ? AND n = 1")
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert!(width > height, "render is {width}x{height}");
    assert!(
        width * height < 1600 * 1600 / 2,
        "render is {width}x{height}"
    );
}

#[tokio::test]
async fn a_skewed_photo_is_shown_from_its_clean_copy() {
    // The shape of the page is what is checked, not its words: English is enough.
    if !have_tesseract(&["eng", "osd"]) {
        return;
    }
    let env = TestEnv::new();
    let app = env.app_with_langs("eng").await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    // A receipt photographed at an angle, on a table.
    let id = upload_fixture(&app, "receipt_photo_skewed.jpg").await;

    pipeline::process_document(&app, id, ProcessOptions::default())
        .await
        .unwrap();

    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready, "{:?}", doc.status_reason);
    let derived = |name: &str| app.files.derived_path(id, name);
    use paperjam::extract::PageRenderer as _;
    let clean_pdf = std::fs::read(derived("clean.pdf")).unwrap();
    let clean_page = paperjam::extract::HayroRenderer
        .open(&clean_pdf)
        .unwrap()
        .render(1, 300)
        .unwrap();
    let aspect = |w: u32, h: u32| w as f32 / h as f32;

    // The viewer's page is the clean page (same shape), not the photo.
    let page = std::fs::read(derived("page-1.webp")).unwrap();
    let shown = image::load_from_memory(&page).unwrap();
    let expected = aspect(clean_page.width(), clean_page.height());
    let got = aspect(shown.width(), shown.height());
    assert!(
        (got - expected).abs() / expected < 0.02,
        "{got} vs {expected}"
    );
    // The page as it was read is kept beside it, and is not the same picture.
    let original = std::fs::read(derived("original-1.webp")).unwrap();
    assert_ne!(original, page);
    let read = image::load_from_memory(&original).unwrap();
    let old = aspect(read.width(), read.height());
    assert!(
        (old - expected).abs() / expected > 0.02,
        "{old} vs {expected}"
    );
    // The pages table and the thumbnail follow what is shown.
    let (width, height): (i64, i64) =
        sqlx::query_as("SELECT width, height FROM pages WHERE document_id = ? AND n = 1")
            .bind(id)
            .fetch_one(&app.db.read)
            .await
            .unwrap();
    assert_eq!(
        (width, height),
        (shown.width().into(), shown.height().into())
    );
    let thumbnail =
        image::load_from_memory(&std::fs::read(derived("thumbnail.webp")).unwrap()).unwrap();
    let thumb = aspect(thumbnail.width(), thumbnail.height());
    assert!(
        (thumb - expected).abs() / expected < 0.1,
        "{thumb} vs {expected}"
    );
}
