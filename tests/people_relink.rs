//! A person created (or renamed, or given an alias) after a document was
//! uploaded is matched against the existing documents by a background job,
//! with no reprocessing and no LLM.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::TestEnv;
use paperjam::cli::serve::job_handler;
use paperjam::ids::{DocumentId, PersonId};
use paperjam::jobs::queue::{ProcessOptions, WorkerPool};
use paperjam::jobs::worker::Worker;
use paperjam::model::document::{DocumentPatch, GetOptions, Provenance};
use paperjam::model::taxonomy::{NewPerson, PersonPatch};
use paperjam::search::{Filters, SearchRequest};
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, documents, taxonomy};
use paperjam::{pipeline, search::sync};
use tokio_util::sync::CancellationToken;

fn new_person(name: &str, aliases: &[&str]) -> NewPerson {
    NewPerson {
        name: name.into(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        notes: String::new(),
    }
}

/// Uploads a text note and runs the local pipeline over it.
async fn note(app: &App, name: &str, text: &str) -> DocumentId {
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
    pipeline::process_document(
        app,
        id,
        ProcessOptions {
            local: true,
            llm: false,
            model_override: None,
        },
    )
    .await
    .unwrap();
    // The upload queued a Process job; the test already ran the pipeline, and
    // a leftover job would reprocess the document and hide a missing relink.
    sqlx::query("DELETE FROM jobs")
        .execute(&app.db.write)
        .await
        .unwrap();
    id
}

async fn people_of(app: &App, id: DocumentId) -> Vec<(PersonId, Provenance)> {
    documents::get_document(&app.ctx(Actor::system()), id, GetOptions::default())
        .await
        .unwrap()
        .people
        .into_iter()
        .map(|p| (p.id, p.provenance))
        .collect()
}

/// Runs the workers until the queue holds no more work.
async fn settle(app: &Arc<App>) {
    let cancel = CancellationToken::new();
    let worker = Worker::spawn(
        Arc::clone(app),
        WorkerPool::Ocr,
        1,
        cancel.clone(),
        job_handler(),
    );
    app.jobs.wake();
    for _ in 0..100 {
        let open: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE state IN ('queued', 'running')")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        if open == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cancel.cancel();
    worker.await.unwrap();
}

async fn app() -> (TestEnv, Arc<App>) {
    let env = TestEnv::new();
    let app = env.app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    (env, Arc::new(app))
}

async fn documents_of_person(app: &App, person: PersonId) -> Vec<DocumentId> {
    let page = sync::search(
        &app.ctx(Actor::system()),
        SearchRequest {
            filters: Filters {
                people: vec![person],
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    page.hits.into_iter().map(|h| h.id).collect()
}

#[tokio::test]
async fn person_created_after_upload_links_without_a_reprocess() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let id = note(&app, "passport.txt", "Passport of Ivan Testov, born 1990").await;
    assert!(people_of(&app, id).await.is_empty());

    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    settle(&app).await;

    assert_eq!(
        people_of(&app, id).await,
        vec![(ivan.id, Provenance::Local)]
    );
    // The search index followed the link.
    assert_eq!(documents_of_person(&app, ivan.id).await, vec![id]);
}

#[tokio::test]
async fn a_person_links_across_transliteration_without_a_latin_alias() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    // The Romanian birth certificate names the father; the person is stored
    // later, in Cyrillic and with a patronymic the document does not print.
    let id = note(
        &app,
        "certificate.txt",
        "CERTIFICAT DE NASTERE Tată: MARACOV ARTIOM",
    )
    .await;
    assert!(people_of(&app, id).await.is_empty());

    let person = taxonomy::create_person(&ctx, new_person("Мараков Артём Иванович", &[]))
        .await
        .unwrap();
    settle(&app).await;

    assert_eq!(
        people_of(&app, id).await,
        vec![(person.id, Provenance::Local)]
    );
}

#[tokio::test]
async fn a_name_extracted_into_a_field_or_a_fact_relinks_too() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    // The scan's OCR line is mangled, but the extraction stored the father
    // in a field and the permitted resident in a fact.
    let id = note(
        &app,
        "lease.txt",
        "CONTRACT DE INCHIRIERE Tat_: M1RAK0V AR910M",
    )
    .await;
    sqlx::query(
        "INSERT INTO field_values (document_id, field_key, value, provenance, page, updated_at) \
         VALUES (?, 'father', '{\"kind\":\"text\",\"value\":\"Maracov Artiom\"}', 'llm', 1, '2026-01-01T00:00:00Z')",
    )
    .bind(id)
    .execute(&app.db.write)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO facts (document_id, key, value, label, provenance, page) \
         VALUES (?, 'permitted_resident', 'Ivan Testov', 'Permitted resident', 'llm', 1)",
    )
    .bind(id)
    .execute(&app.db.write)
    .await
    .unwrap();
    assert!(people_of(&app, id).await.is_empty());

    let artiom = taxonomy::create_person(&ctx, new_person("Мараков Артём Иванович", &[]))
        .await
        .unwrap();
    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    settle(&app).await;

    let mut found = people_of(&app, id).await;
    found.sort_by_key(|(person, _)| person.to_string());
    let mut expected = vec![(artiom.id, Provenance::Local), (ivan.id, Provenance::Local)];
    expected.sort_by_key(|(person, _)| person.to_string());
    assert_eq!(found, expected);
}

#[tokio::test]
async fn a_new_alias_links_and_a_removed_alias_unlinks() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let id = note(&app, "card.txt", "ID card of Marik Testov").await;
    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    settle(&app).await;
    assert!(people_of(&app, id).await.is_empty());

    taxonomy::add_person_alias(&ctx, ivan.id, "Marik Testov")
        .await
        .unwrap();
    settle(&app).await;
    assert_eq!(
        people_of(&app, id).await,
        vec![(ivan.id, Provenance::Local)]
    );

    taxonomy::remove_person_alias(&ctx, ivan.id, "Marik Testov")
        .await
        .unwrap();
    settle(&app).await;
    assert!(people_of(&app, id).await.is_empty());
    assert!(documents_of_person(&app, ivan.id).await.is_empty());
}

#[tokio::test]
async fn renaming_a_person_relinks() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let id = note(&app, "letter.txt", "Dear Anna Ionescu, your policy").await;
    let person = taxonomy::create_person(&ctx, new_person("Placeholder Name", &[]))
        .await
        .unwrap();
    settle(&app).await;
    assert!(people_of(&app, id).await.is_empty());

    taxonomy::update_person(
        &ctx,
        person.id,
        PersonPatch {
            name: Some("Anna Ionescu".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    settle(&app).await;
    assert_eq!(
        people_of(&app, id).await,
        vec![(person.id, Provenance::Local)]
    );
}

#[tokio::test]
async fn people_chosen_by_hand_are_never_relinked() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let chosen = note(&app, "a.txt", "Contract for Boris Petrov and Ivan Testov").await;
    let cleared = note(
        &app,
        "b.txt",
        "Contract for Boris Petrov and Ivan Testov v2",
    )
    .await;
    let boris = taxonomy::create_person(&ctx, new_person("Boris Petrov", &[]))
        .await
        .unwrap();
    settle(&app).await;
    assert_eq!(people_of(&app, chosen).await.len(), 1);

    // One document is set by hand to somebody else, the other is cleared by hand.
    let other = taxonomy::create_person(&ctx, new_person("Somebody Else", &[]))
        .await
        .unwrap();
    for (doc, people) in [(chosen, vec![other.id]), (cleared, vec![])] {
        documents::update_document(
            &ctx,
            doc,
            DocumentPatch {
                people: Some(people),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    taxonomy::add_person_alias(&ctx, boris.id, "B. Petrov")
        .await
        .unwrap();
    settle(&app).await;

    assert_eq!(
        people_of(&app, chosen).await,
        vec![(other.id, Provenance::Manual)]
    );
    assert!(
        people_of(&app, cleared).await.is_empty(),
        "a person the user removed by hand comes back as {:?}",
        ivan.id
    );
}

#[tokio::test]
async fn relinking_a_person_on_request_restores_a_lost_link() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let id = note(&app, "a.txt", "Passport of Ivan Testov").await;
    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    settle(&app).await;
    assert_eq!(people_of(&app, id).await.len(), 1);

    // Links that drifted away (an older version, a restored backup) are not
    // noticed until somebody asks.
    sqlx::query("DELETE FROM document_people")
        .execute(&app.db.write)
        .await
        .unwrap();
    settle(&app).await;
    assert!(people_of(&app, id).await.is_empty());

    taxonomy::relink_person(&ctx, ivan.id).await.unwrap();
    settle(&app).await;
    assert_eq!(
        people_of(&app, id).await,
        vec![(ivan.id, Provenance::Local)]
    );
}

#[tokio::test]
async fn relinking_an_unknown_person_is_not_found() {
    let (_env, app) = app().await;
    let error = taxonomy::relink_person(&app.ctx(Actor::system()), PersonId::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "person_not_found");
}

#[tokio::test]
async fn a_cleared_document_stays_cleared_through_a_reprocess() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let ivan = taxonomy::create_person(&ctx, new_person("Ivan Testov", &[]))
        .await
        .unwrap();
    let id = note(&app, "a.txt", "Passport of Ivan Testov").await;
    assert_eq!(people_of(&app, id).await.len(), 1);

    documents::update_document(
        &ctx,
        id,
        DocumentPatch {
            people: Some(vec![]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    pipeline::process_document(
        &app,
        id,
        ProcessOptions {
            local: true,
            llm: false,
            model_override: None,
        },
    )
    .await
    .unwrap();
    assert!(
        people_of(&app, id).await.is_empty(),
        "{:?} came back",
        ivan.id
    );
}
