//! Search through the service layer, the one entry point of REST and MCP:
//! what needs the document types or the database, beyond the index itself.

mod common;

use chrono::Datelike;
use common::TestEnv;
use paperjam::ids::DocumentId;
use paperjam::jobs::queue::ProcessOptions;
use paperjam::model::document::DocumentPatch;
use paperjam::model::taxonomy::NewType;
use paperjam::pipeline;
use paperjam::search::{SearchRequest, sync};
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, documents, taxonomy};
use serde_json::json;

async fn text_document(app: &App, name: &str, text: &str) -> DocumentId {
    let outcome = upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: name.into(),
            ..Default::default()
        },
        text.as_bytes(),
    )
    .await
    .unwrap();
    let id = outcome.document_id();
    pipeline::process_document(app, id, ProcessOptions::default())
        .await
        .unwrap();
    id
}

async fn found(app: &App, query: &str) -> Vec<DocumentId> {
    let page = search(app, query).await;
    page.hits.iter().map(|h| h.id).collect()
}

async fn search(app: &App, query: &str) -> paperjam::search::SearchPage {
    sync::search(
        &app.ctx(Actor::system()),
        SearchRequest {
            query: Some(query.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("query {query:?}: {e}"))
}

#[tokio::test]
async fn a_bare_year_on_a_date_field_is_that_year_and_on_a_number_field_a_number() {
    let env = TestEnv::new();
    let app = env.app().await;
    let ctx = app.ctx(Actor::system());
    let permit: NewType = serde_json::from_value(json!({
        "key": "permit",
        "labels": {"en": "Permit"},
        "fields": [
            {"key": "expiry_date", "labels": {"en": "Expires"}, "kind": "date"},
            {"key": "units", "labels": {"en": "Units"}, "kind": "number"},
        ],
    }))
    .unwrap();
    let permit = taxonomy::create_type(&ctx, permit).await.unwrap();

    let early = text_document(&app, "early.txt", "permit early").await;
    let late = text_document(&app, "late.txt", "permit late").await;
    for (id, expiry, units) in [(early, "2030-06-15", "2100"), (late, "2037-03-01", "12")] {
        let patch: DocumentPatch = serde_json::from_value(json!({
            "type_id": permit.id,
            "fields": {
                "expiry_date": {"kind": "date", "value": expiry},
                "units": {"kind": "number", "value": units},
            },
        }))
        .unwrap();
        documents::update_document(&ctx, id, patch).await.unwrap();
    }

    // Before 2037 begins; 2037 itself; from 2037 on; up to the end of 2037.
    assert_eq!(found(&app, "field.expiry_date<2037").await, vec![early]);
    assert_eq!(found(&app, "field.expiry_date=2037").await, vec![late]);
    assert_eq!(found(&app, "field.expiry_date>=2037").await, vec![late]);
    assert_eq!(found(&app, "field.expiry_date>2037").await, vec![]);
    assert_eq!(found(&app, "field.expiry_date<=2037").await.len(), 2);
    // The structured filter takes the same value.
    let page = sync::search(
        &ctx,
        serde_json::from_value(json!({
            "filters": {"fields": [{"key": "expiry_date", "op": "lt", "value": "2037"}]},
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![early]
    );

    // A number field keeps its number: 2100 is not a year.
    assert_eq!(found(&app, "field.units>2037").await, vec![early]);
    assert_eq!(found(&app, "field.units<2037").await, vec![late]);
}

#[tokio::test]
async fn a_query_nothing_fully_matches_falls_back_to_some_of_its_words() {
    let env = TestEnv::new();
    let app = env.app().await;
    let passport = text_document(&app, "passport.txt", "passport of the holder").await;
    let other = text_document(&app, "mother.txt", "mother certificate").await;

    // Both words appear in no one document: the documents with either are shown, flagged.
    let page = search(&app, "passport mother").await;
    assert!(page.partial, "{page:?}");
    let mut ids: Vec<_> = page.hits.iter().map(|h| h.id).collect();
    ids.sort();
    let mut both = vec![passport, other];
    both.sort();
    assert_eq!(ids, both);
    assert_eq!(page.total, 2);

    // A full match is never mixed with partial ones.
    let page = search(&app, "passport holder").await;
    assert!(!page.partial);
    assert_eq!(
        page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![passport]
    );

    // A single word, an explicit OR and a query that matches nothing at all
    // are not flagged.
    assert!(!search(&app, "nothingatall").await.partial);
    assert!(!search(&app, "passport OR nothingatall").await.partial);
    let none = search(&app, "nothing atall").await;
    assert!(!none.partial);
    assert_eq!(none.total, 0);
    // A number is looked up whole: other people's documents are not "some of" it.
    let numeric = search(&app, "passport 4521123456").await;
    assert!(!numeric.partial);
    assert_eq!(numeric.total, 0);
    // A negation is kept: the relaxed pass does not bring back what it excluded.
    let page = search(&app, "passport mother -certificate").await;
    assert_eq!(
        page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![passport]
    );
    assert!(page.partial);
}

/// A document of `type_key` with typed fields set by hand.
async fn typed_document(
    app: &App,
    name: &str,
    type_key: &str,
    fields: serde_json::Value,
) -> DocumentId {
    let ctx = app.ctx(Actor::system());
    let id = text_document(app, name, name).await;
    let type_id: String = sqlx::query_scalar("SELECT id FROM document_types WHERE key = ?")
        .bind(type_key)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    let patch: DocumentPatch =
        serde_json::from_value(json!({"type_id": type_id, "fields": fields})).unwrap();
    documents::update_document(&ctx, id, patch).await.unwrap();
    id
}

fn day(offset: i64) -> chrono::NaiveDate {
    chrono::Utc::now().date_naive() + chrono::Duration::days(offset)
}

fn date_field(date: chrono::NaiveDate) -> serde_json::Value {
    json!({"kind": "date", "value": date.to_string()})
}

#[tokio::test]
async fn due_dates_find_what_is_about_to_expire() {
    let env = TestEnv::new();
    let app = env.app().await;
    let ctx = app.ctx(Actor::system());
    taxonomy::seed_builtin_types(&ctx).await.unwrap();

    let soon = typed_document(
        &app,
        "soon.txt",
        "travel_passport",
        json!({"expiry_date": date_field(day(30))}),
    )
    .await;
    let later = typed_document(
        &app,
        "later.txt",
        "id_card",
        json!({"expiry_date": date_field(day(400))}),
    )
    .await;
    let lapsed = typed_document(
        &app,
        "lapsed.txt",
        "residence_permit",
        json!({"expiry_date": date_field(day(-10))}),
    )
    .await;
    let undated = typed_document(&app, "undated.txt", "id_card", json!({})).await;
    // A Russian internal passport prints no expiry: it is replaced at the
    // 20th birthday, which here falls 60 days from now.
    let twentieth = day(60);
    let born = twentieth
        .with_year(twentieth.year() - 20)
        .unwrap_or(twentieth - chrono::Duration::days(20 * 365 + 5));
    let internal = typed_document(
        &app,
        "internal.txt",
        "passport",
        json!({
            "birth_date": date_field(born),
            "issue_date": date_field(born + chrono::Duration::days(14 * 365)),
            "nationality": {"kind": "text", "value": "RUS"},
        }),
    )
    .await;
    // The same dates on a passport of another country give no due date.
    let foreign = typed_document(
        &app,
        "foreign.txt",
        "passport",
        json!({
            "birth_date": date_field(born),
            "issue_date": date_field(born + chrono::Duration::days(14 * 365)),
            "nationality": {"kind": "text", "value": "UTO"},
        }),
    )
    .await;

    let mut expiring = found(&app, "has:expiring").await;
    expiring.sort();
    let mut expected = vec![soon, lapsed, internal];
    expected.sort();
    assert_eq!(expiring, expected);

    let mut calm = found(&app, "-has:expiring").await;
    calm.sort();
    let mut expected = vec![later, undated, foreign];
    expected.sort();
    assert_eq!(calm, expected);

    assert_eq!(found(&app, "has:due").await.len(), 4);
    let mut without = found(&app, "-has:due").await;
    without.sort();
    let mut expected = vec![undated, foreign];
    expected.sort();
    assert_eq!(without, expected);

    // `due:` compares like `date:`: a day, a month or a year.
    let mut before = found(&app, &format!("due:<{}", day(100))).await;
    before.sort();
    let mut expected = vec![soon, lapsed, internal];
    expected.sort();
    assert_eq!(before, expected);
    assert_eq!(
        found(&app, &format!("due:>={}", day(100))).await,
        vec![later]
    );
    assert_eq!(found(&app, &format!("due:{}", day(30))).await, vec![soon]);
    assert!(found(&app, "due:1999").await.is_empty());
    assert_eq!(
        found(&app, &format!("due:{}", day(400).format("%Y-%m"))).await,
        vec![later]
    );

    // Hits and the document carry the date.
    let page = search(&app, "has:expiring type:travel_passport").await;
    assert_eq!(page.hits[0].due_date, Some(day(30)));
    let view = documents::get_document(&ctx, internal, Default::default())
        .await
        .unwrap();
    assert_eq!(view.due_date, Some(twentieth));
    let view = documents::get_document(&ctx, foreign, Default::default())
        .await
        .unwrap();
    assert_eq!(view.due_date, None);
}

#[tokio::test]
async fn a_malformed_due_date_is_a_filter_error() {
    let env = TestEnv::new();
    let app = env.app().await;
    let error = sync::search(
        &app.ctx(Actor::system()),
        SearchRequest {
            query: Some("due:soonish".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "query_filter");
}

#[tokio::test]
async fn a_hit_says_whether_the_document_has_page_images() {
    let env = TestEnv::new();
    let app = env.app().await;

    // A plain-text file has no page image and a PDF has one; a document that
    // failed before it was rendered has none either.
    let note = text_document(&app, "note.txt", "household note").await;
    let lease = upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: "lease.pdf".into(),
            ..Default::default()
        },
        std::fs::read(common::fixture("contract_ru_digital.pdf"))
            .unwrap()
            .as_slice(),
    )
    .await
    .unwrap()
    .document_id();
    pipeline::process_document(&app, lease, ProcessOptions::default())
        .await
        .unwrap();
    let broken = text_document(&app, "broken.txt", "broken note").await;
    sqlx::query("UPDATE documents SET status = 'failed' WHERE id = ?")
        .bind(broken)
        .execute(&app.db.write)
        .await
        .unwrap();
    sync::index_one(&app, broken).await.unwrap();

    let page = sync::search(&app.ctx(Actor::system()), SearchRequest::default())
        .await
        .unwrap();
    let has_pages = |id: DocumentId| page.hits.iter().find(|h| h.id == id).unwrap().has_pages;
    assert!(!has_pages(note));
    assert!(has_pages(lease));
    assert!(!has_pages(broken));

    // The document view says the same.
    for (id, expected) in [(note, false), (lease, true)] {
        let view = documents::get_document(&app.ctx(Actor::system()), id, Default::default())
            .await
            .unwrap();
        assert_eq!(view.file.has_pages, expected);
    }
}
