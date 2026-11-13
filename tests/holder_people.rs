//! The holder of an identity document becomes a person when nobody is linked
//! yet, so a household archive has its people without anyone adding them
//! first. Other names in a document never create people.

mod common;

use common::TestEnv;
use paperjam::ids::{DocumentId, PersonId};
use paperjam::jobs::queue::ProcessOptions;
use paperjam::model::document::{DocumentPatch, GetOptions, Provenance};
use paperjam::model::fields::FieldValue;
use paperjam::model::taxonomy::PersonView;
use paperjam::pipeline;
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, documents, taxonomy};

async fn app() -> (TestEnv, App) {
    let env = TestEnv::new();
    let app = env.app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    (env, app)
}

/// Uploads a text note, makes it a document of `type_key` with the given
/// text fields (as values read from the document, not typed by a user) and
/// runs the local pipeline over it, which is where the holder is linked.
async fn identity_doc(
    app: &App,
    name: &str,
    text: &str,
    type_key: &str,
    fields: &[(&str, &str)],
) -> DocumentId {
    let id = upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: name.into(),
            ..Default::default()
        },
        text.as_bytes(),
    )
    .await
    .unwrap()
    .document_id();
    sqlx::query("DELETE FROM jobs")
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE documents SET type_id = (SELECT id FROM document_types WHERE key = ?), \
         type_provenance = 'llm' WHERE id = ?",
    )
    .bind(type_key)
    .bind(id)
    .execute(&app.db.write)
    .await
    .unwrap();
    set_fields(app, id, fields, "llm").await;
    reprocess(app, id).await;
    id
}

async fn set_fields(app: &App, id: DocumentId, fields: &[(&str, &str)], provenance: &str) {
    for (key, value) in fields {
        let json = serde_json::json!(FieldValue::Text((*value).into())).to_string();
        sqlx::query(
            "INSERT OR REPLACE INTO field_values (document_id, field_key, value, provenance, updated_at) \
             VALUES (?, ?, ?, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(key)
        .bind(json)
        .bind(provenance)
        .execute(&app.db.write)
        .await
        .unwrap();
    }
}

async fn reprocess(app: &App, id: DocumentId) {
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

async fn all_people(app: &App) -> Vec<PersonView> {
    taxonomy::list_taxonomy(&app.ctx(Actor::system()))
        .await
        .unwrap()
        .people
}

const TESTOV_PAGE: &str =
    "Паспорт / Passport\nФамилия / Surname ТЕСТОВ / TESTOV\nИмя / Given names ИВАН / IVAN\n";

#[tokio::test]
async fn a_passport_holder_without_people_becomes_a_person() {
    let (_env, app) = app().await;
    let id = identity_doc(
        &app,
        "passport.txt",
        TESTOV_PAGE,
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН")],
    )
    .await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    // The interface language is English, so the Latin spelling names the
    // person and the Cyrillic one the next document is matched by.
    assert_eq!(people[0].name, "Ivan Testov");
    assert_eq!(people[0].aliases, vec!["Иван Тестов".to_string()]);
    assert_eq!(
        people_of(&app, id).await,
        vec![(people[0].id, Provenance::Local)]
    );
}

#[tokio::test]
async fn a_russian_interface_names_the_person_in_cyrillic() {
    let (_env, app) = app().await;
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, locale, created_at) \
         VALUES ('u1', 'ru', 'Ru', 'x', 'ru', '2026-01-01T00:00:00Z')",
    )
    .execute(&app.db.write)
    .await
    .unwrap();
    let id = identity_doc(
        &app,
        "passport.txt",
        TESTOV_PAGE,
        "passport",
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
    )
    .await;
    // The upload had no user; attribute it and run the pipeline again over a
    // fresh state to see the language apply.
    sqlx::query("DELETE FROM document_people WHERE document_id = ?")
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("DELETE FROM people")
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("UPDATE documents SET added_by = 'u1' WHERE id = ?")
        .bind(id)
        .execute(&app.db.write)
        .await
        .unwrap();
    reprocess(&app, id).await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Иван Тестов");
    assert_eq!(people[0].aliases, vec!["Ivan Testov".to_string()]);
}

#[tokio::test]
async fn the_same_holder_in_the_other_script_is_not_added_twice() {
    let (_env, app) = app().await;
    let first = identity_doc(
        &app,
        "internal.txt",
        "Паспорт ТЕСТОВ ИВАН",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН")],
    )
    .await;
    let second = identity_doc(
        &app,
        "travel.txt",
        "Passport TESTOV IVAN",
        "travel_passport",
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
    )
    .await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people_of(&app, first).await[0].0, people[0].id);
    assert_eq!(people_of(&app, second).await[0].0, people[0].id);
}

/// A Romanian ID makes "Ivan Testov"; then a Russian passport of the same
/// holder, linked to that person, is added by a user who reads Cyrillic.
async fn latin_person_then_cyrillic_passport(app: &App) -> (PersonId, DocumentId) {
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, locale, created_at) \
         VALUES ('u1', 'ru', 'Ru', 'x', 'ru', '2026-01-01T00:00:00Z')",
    )
    .execute(&app.db.write)
    .await
    .unwrap();
    identity_doc(
        app,
        "id.txt",
        "Carte de identitate TESTOV IVAN",
        "id_card",
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
    )
    .await;
    let people = all_people(app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people[0].name, "Ivan Testov");
    let person = people[0].id;

    let passport = identity_doc(
        app,
        "passport.txt",
        "Паспорт ТЕСТОВ ИВАН",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН")],
    )
    .await;
    // As the model links it: to the existing person, nobody else.
    sqlx::query("DELETE FROM document_people WHERE document_id = ?")
        .bind(passport)
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("DELETE FROM people WHERE id <> ?")
        .bind(person)
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO document_people (document_id, person_id, provenance) VALUES (?, ?, 'llm')",
    )
    .bind(passport)
    .bind(person)
    .execute(&app.db.write)
    .await
    .unwrap();
    sqlx::query("UPDATE documents SET added_by = 'u1' WHERE id = ?")
        .bind(passport)
        .execute(&app.db.write)
        .await
        .unwrap();
    (person, passport)
}

#[tokio::test]
async fn a_person_named_in_latin_takes_the_cyrillic_name_of_a_later_passport() {
    let (_env, app) = app().await;
    let (person, passport) = latin_person_then_cyrillic_passport(&app).await;
    reprocess(&app, passport).await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people[0].id, person);
    assert_eq!(people[0].name, "Иван Тестов");
    assert!(people[0].aliases.contains(&"Ivan Testov".to_string()));
}

#[tokio::test]
async fn a_name_read_in_both_spellings_still_names_the_holder() {
    let (_env, app) = app().await;
    let id = identity_doc(
        &app,
        "id.txt",
        "Identity card TESTOVA / ТЕСТОВА ANNA / АННА",
        "id_card",
        &[
            ("surname", "TESTOVA / ТЕСТОВА"),
            ("given_names", "ANNA / АННА"),
        ],
    )
    .await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people[0].name, "Anna Testova");
    assert_eq!(people_of(&app, id).await[0].0, people[0].id);
}

#[tokio::test]
async fn a_person_renamed_by_hand_keeps_the_name() {
    let (_env, app) = app().await;
    let (person, passport) = latin_person_then_cyrillic_passport(&app).await;
    sqlx::query(
        "INSERT INTO events (actor_user_id, via, action, at, diff) \
         VALUES ('u1', 'web', 'person.update', '2026-01-02T00:00:00Z', json_object('id', ?))",
    )
    .bind(person.to_string())
    .execute(&app.db.write)
    .await
    .unwrap();
    reprocess(&app, passport).await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people[0].name, "Ivan Testov");
}

#[tokio::test]
async fn a_zone_verified_name_links_with_mrz_provenance() {
    let (_env, app) = app().await;
    let id = identity_doc(
        &app,
        "travel.txt",
        "Passport",
        "travel_passport",
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
    )
    .await;
    sqlx::query("DELETE FROM document_people")
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("DELETE FROM people")
        .execute(&app.db.write)
        .await
        .unwrap();
    set_fields(
        &app,
        id,
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
        "mrz",
    )
    .await;
    // A reprocess would drop values the zone does not support here, so the
    // relink pass (which reads the stored values) does the linking.
    pipeline::people::relink_all(&app).await.unwrap();

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].name, "Ivan Testov");
    assert_eq!(
        people_of(&app, id).await,
        vec![(people[0].id, Provenance::Mrz)]
    );
}

#[tokio::test]
async fn a_passport_with_a_patronymic_names_the_person_surname_given_patronymic() {
    let (_env, app) = app().await;
    let id = identity_doc(
        &app,
        "internal.txt",
        "Паспорт РФ ТЕСТОВ ИВАН ПЕТРОВИЧ",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН ПЕТРОВИЧ")],
    )
    .await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "{people:?}");
    assert_eq!(people[0].name, "Тестов Иван Петрович");
    assert_eq!(
        people[0].aliases,
        vec![
            "Иван Тестов".to_string(),
            "Иван Петрович Тестов".to_string()
        ]
    );
    assert_eq!(
        people_of(&app, id).await,
        vec![(people[0].id, Provenance::Local)]
    );
}

#[tokio::test]
async fn a_passport_with_a_patronymic_matches_existing_short_name() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let ivan = taxonomy::create_person(
        &ctx,
        paperjam::model::taxonomy::NewPerson {
            name: "Иван Тестов".into(),
            aliases: vec![],
            notes: String::new(),
        },
    )
    .await
    .unwrap();

    let id = identity_doc(
        &app,
        "internal.txt",
        "Паспорт РФ ТЕСТОВ ИВАН ПЕТРОВИЧ",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН ПЕТРОВИЧ")],
    )
    .await;

    let people = all_people(&app).await;
    assert_eq!(people.len(), 1, "no duplicate person created");
    assert_eq!(people[0].id, ivan.id);
    assert_eq!(
        people_of(&app, id).await,
        vec![(ivan.id, Provenance::Local)]
    );
}

#[tokio::test]
async fn people_set_by_hand_are_left_alone() {
    let (_env, app) = app().await;
    let ctx = app.ctx(Actor::system());
    let cleared = identity_doc(
        &app,
        "cleared.txt",
        "Паспорт ТЕСТОВ ИВАН",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН")],
    )
    .await;
    // The user removed the person that was created; it must not come back.
    documents::update_document(
        &ctx,
        cleared,
        DocumentPatch {
            people: Some(vec![]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM people")
        .execute(&app.db.write)
        .await
        .unwrap();
    reprocess(&app, cleared).await;
    assert!(people_of(&app, cleared).await.is_empty());
    assert!(all_people(&app).await.is_empty());

    // A person chosen by hand stays the only one.
    let chosen = identity_doc(
        &app,
        "chosen.txt",
        "Passport PETROV BORIS",
        "travel_passport",
        &[("surname", "PETROV"), ("given_names", "BORIS")],
    )
    .await;
    let other = taxonomy::create_person(
        &ctx,
        paperjam::model::taxonomy::NewPerson {
            name: "Somebody Else".into(),
            aliases: vec![],
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM document_people WHERE document_id = ?")
        .bind(chosen)
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("DELETE FROM people WHERE id != ?")
        .bind(other.id)
        .execute(&app.db.write)
        .await
        .unwrap();
    documents::update_document(
        &ctx,
        chosen,
        DocumentPatch {
            people: Some(vec![other.id]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    reprocess(&app, chosen).await;
    assert_eq!(
        people_of(&app, chosen).await,
        vec![(other.id, Provenance::Manual)]
    );
    assert_eq!(all_people(&app).await.len(), 1);
}

#[tokio::test]
async fn the_parents_on_a_birth_certificate_create_nobody() {
    let (_env, app) = app().await;
    identity_doc(
        &app,
        "birth.txt",
        "Свидетельство о рождении ТЕСТОВ ПЁТР, отец ТЕСТОВ ИВАН, мать ТЕСТОВА АННА",
        "birth_certificate",
        &[
            ("surname", "ТЕСТОВ"),
            ("given_names", "ПЁТР"),
            ("father", "ТЕСТОВ ИВАН"),
            ("mother", "ТЕСТОВА АННА"),
        ],
    )
    .await;
    assert!(all_people(&app).await.is_empty());
}

#[tokio::test]
async fn relinking_brings_existing_documents_along() {
    let (_env, app) = app().await;
    let first = identity_doc(
        &app,
        "internal.txt",
        "Паспорт ТЕСТОВ ИВАН",
        "passport",
        &[("surname", "ТЕСТОВ"), ("given_names", "ИВАН")],
    )
    .await;
    let second = identity_doc(
        &app,
        "licence.txt",
        "Driving licence TESTOV IVAN",
        "driver_license",
        &[("surname", "TESTOV"), ("given_names", "IVAN")],
    )
    .await;
    // Documents as they were stored before holders became people.
    sqlx::query("DELETE FROM document_people")
        .execute(&app.db.write)
        .await
        .unwrap();
    sqlx::query("DELETE FROM people")
        .execute(&app.db.write)
        .await
        .unwrap();

    let done = pipeline::people::relink_all(&app).await.unwrap();
    assert_eq!(done.created, 1);
    assert_eq!(done.changed, 2);
    let people = all_people(&app).await;
    assert_eq!(people.len(), 1);
    assert_eq!(people_of(&app, first).await[0].0, people[0].id);
    assert_eq!(people_of(&app, second).await[0].0, people[0].id);

    // A second run has nothing left to do.
    let again = pipeline::people::relink_all(&app).await.unwrap();
    assert_eq!((again.created, again.changed), (0, 0));
}
