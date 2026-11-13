//! Tests for the database → index synchronisation.

use super::sync::*;
use super::{Filters, SearchRequest};
use crate::ids::DocumentId;
use crate::model::document::DocumentPatch;
use crate::model::fields::{FieldDef, FieldKind, FieldValue, Labels};
use crate::model::taxonomy::{NewPerson, NewType};
use crate::service::testing::{test_app, test_app_with_langs};
use crate::service::upload::{UploadInput, UploadOutcome, upload};
use crate::service::{Actor, App, Ctx, Via, documents, taxonomy};

fn cli(app: &App) -> Ctx<'_> {
    app.ctx(Actor::cli())
}

fn web(app: &App) -> Ctx<'_> {
    app.ctx(Actor {
        user_id: None,
        via: Via::Web,
    })
}

async fn make_doc(app: &App, name: &str) -> DocumentId {
    let body = format!("%PDF-1.4\n% {name}\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n");
    match upload(
        &cli(app),
        UploadInput {
            original_name: name.to_string(),
            ..UploadInput::default()
        },
        body.as_bytes(),
    )
    .await
    .unwrap()
    {
        UploadOutcome::Created(id) | UploadOutcome::Restored(id) => id,
    }
}

async fn found(app: &App, query: &str) -> Vec<DocumentId> {
    commit(app).await.unwrap();
    search(
        &cli(app),
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

#[tokio::test]
async fn uploaded_document_is_searchable_by_file_name() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "rental lease 2026.pdf").await;
    assert_eq!(found(&app, "lease").await, vec![id]);
    assert!(found(&app, "mortgage").await.is_empty());
}

#[tokio::test]
async fn trash_hides_and_restore_shows_again() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "warranty.pdf").await;
    documents::trash_document(&cli(&app), id).await.unwrap();
    assert!(found(&app, "warranty").await.is_empty());
    documents::restore_document(&cli(&app), id).await.unwrap();
    assert_eq!(found(&app, "warranty").await, vec![id]);
}

#[tokio::test]
async fn hits_say_whether_they_are_trashed() {
    let (_dir, app) = test_app().await;
    let live = make_doc(&app, "invoice live.pdf").await;
    let trashed = make_doc(&app, "invoice old.pdf").await;
    documents::trash_document(&cli(&app), trashed)
        .await
        .unwrap();
    commit(&app).await.unwrap();
    let page = search(
        &cli(&app),
        SearchRequest {
            query: Some("invoice".into()),
            filters: Filters {
                include_trashed: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let trashed_flag = |id: DocumentId| page.hits.iter().find(|h| h.id == id).map(|h| h.trashed);
    assert_eq!(trashed_flag(trashed), Some(true));
    assert_eq!(trashed_flag(live), Some(false));
}

#[tokio::test]
async fn purge_removes_the_document_from_the_index() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "receipt.pdf").await;
    documents::trash_document(&cli(&app), id).await.unwrap();
    documents::purge_document(&web(&app), id).await.unwrap();
    commit(&app).await.unwrap();
    let with_trash = search(
        &cli(&app),
        SearchRequest {
            filters: Filters {
                include_trashed: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(with_trash.total, 0);
}

#[tokio::test]
async fn person_alias_edit_reaches_their_documents() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "policy.pdf").await;
    let person = taxonomy::create_person(
        &cli(&app),
        NewPerson {
            name: "Ivan Testov".into(),
            aliases: vec![],
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    documents::update_document(
        &cli(&app),
        id,
        DocumentPatch {
            people: Some(vec![person.id]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found(&app, "Testov").await, vec![id]);
    assert!(found(&app, "Zebulon").await.is_empty());

    taxonomy::add_person_alias(&cli(&app), person.id, "Zebulon")
        .await
        .unwrap();
    assert_eq!(found(&app, "Zebulon").await, vec![id]);
    assert_eq!(
        found(&app, &format!("person:{}", person.id)).await,
        vec![id]
    );
}

#[tokio::test]
async fn deleting_a_tag_removes_it_from_the_index() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "tagged.pdf").await;
    let tag = taxonomy::create_tag(&cli(&app), "warranty", None)
        .await
        .unwrap();
    documents::update_document(
        &cli(&app),
        id,
        DocumentPatch {
            tags: Some(vec![tag.id]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found(&app, "tag:warranty").await, vec![id]);
    taxonomy::delete_tag(&cli(&app), tag.id).await.unwrap();
    assert!(found(&app, "tag:warranty").await.is_empty());
}

#[tokio::test]
async fn manual_field_values_are_searchable_and_shown_on_the_hit() {
    let (_dir, app) = test_app().await;
    let kind = taxonomy::create_type(
        &cli(&app),
        NewType {
            key: "passport".into(),
            labels: Labels::from([("en".into(), "Passport".into())]),
            fields: vec![FieldDef {
                key: "number".into(),
                labels: Labels::from([("en".into(), "Number".into())]),
                kind: FieldKind::Identifier,
                validator: None,
                required: false,
                issuer: false,
            }],
        },
    )
    .await
    .unwrap();
    let id = make_doc(&app, "scan0001.pdf").await;
    documents::update_document(
        &cli(&app),
        id,
        DocumentPatch {
            type_id: Some(Some(kind.id)),
            fields: [(
                "number".to_string(),
                Some(FieldValue::Identifier("AB1234567".into())),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found(&app, "field.number:AB1234567").await, vec![id]);
    assert_eq!(found(&app, "type:passport").await, vec![id]);
    let page = search(
        &cli(&app),
        SearchRequest {
            query: Some("AB1234567".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let hit = &page.hits[0];
    assert_eq!(
        hit.key_fields.get("number"),
        Some(&FieldValue::Identifier("AB1234567".into()))
    );
    assert_eq!(hit.type_key.as_deref(), Some("passport"));
}

#[tokio::test]
async fn shown_layer_is_displayed_and_other_layers_are_still_searched() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "layers.pdf").await;
    for (source, text, quality) in [
        ("embedded", "the exact embedded words", 0.9),
        ("tesseract", "noisy zzzocr reading", 0.5),
    ] {
        sqlx::query(
            "INSERT INTO text_layers (document_id, page, source, text, quality_score, created_at) \
             VALUES (?, 1, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(source)
        .bind(text)
        .bind(quality)
        .bind(crate::time::now_ts())
        .execute(&app.db.write)
        .await
        .unwrap();
    }
    refresh(&app, id).await;
    let mut conn = app.db.read.acquire().await.unwrap();
    let doc = build_index_doc(&mut conn, &app.files, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(doc.texts, vec!["the exact embedded words"]);
    assert_eq!(doc.other_texts, vec!["noisy zzzocr reading"]);
    drop(conn);
    assert_eq!(found(&app, "zzzocr").await, vec![id]);
}

#[tokio::test]
async fn ensure_current_rebuilds_an_emptied_index() {
    let (_dir, app) = test_app().await;
    let a = make_doc(&app, "alpha.pdf").await;
    let b = make_doc(&app, "beta.pdf").await;
    commit(&app).await.unwrap();
    assert_eq!(app.index.num_docs(), 2);

    app.index.clear().unwrap();
    commit(&app).await.unwrap();
    assert_eq!(app.index.num_docs(), 0);

    ensure_current(&app).await.unwrap();
    assert_eq!(app.index.num_docs(), 2);
    assert_eq!(found(&app, "alpha").await, vec![a]);
    assert_eq!(found(&app, "beta").await, vec![b]);
}

#[tokio::test]
async fn search_sees_changes_that_are_not_committed_yet() {
    let (_dir, app) = test_app().await;
    let id = make_doc(&app, "fresh-upload.pdf").await;
    // No explicit commit: the background committer has not run.
    let hits = search(
        &cli(&app),
        SearchRequest {
            query: Some("fresh".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .hits;
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![id]);
}

async fn typed_doc(
    app: &App,
    name: &str,
    type_key: &str,
    fields: Vec<(&str, FieldValue)>,
) -> DocumentId {
    let tax = taxonomy::list_taxonomy(&cli(app)).await.unwrap();
    let kind = tax.types.iter().find(|t| t.key == type_key).unwrap();
    let id = make_doc(app, name).await;
    documents::update_document(
        &cli(app),
        id,
        DocumentPatch {
            type_id: Some(Some(kind.id)),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), Some(v)))
                .collect(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn type_names_in_both_languages_find_internal_and_travel_passports() {
    let (_dir, app) = test_app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    let internal = typed_doc(&app, "scan1.pdf", "passport", vec![]).await;
    let travel = typed_doc(&app, "scan2.pdf", "travel_passport", vec![]).await;
    let receipt = typed_doc(&app, "scan3.pdf", "receipt", vec![]).await;

    let sorted = |mut v: Vec<DocumentId>| {
        v.sort();
        v
    };
    for query in ["паспорт", "паспорта", "passport", "passports"] {
        assert_eq!(
            sorted(found(&app, query).await),
            sorted(vec![internal, travel]),
            "{query}"
        );
    }
    for query in ["загранпаспорт", "заграничный паспорт", "travel passport"]
    {
        assert_eq!(found(&app, query).await, vec![travel], "{query}");
    }
    assert_eq!(found(&app, "чек").await, vec![receipt]);
    assert_eq!(found(&app, "receipt").await, vec![receipt]);

    // A renamed label is searched as soon as it is saved.
    let tax = taxonomy::list_taxonomy(&cli(&app)).await.unwrap();
    let kind = tax.types.iter().find(|t| t.key == "receipt").unwrap();
    let mut labels = kind.labels.clone();
    labels.insert("en".into(), "Till slip".into());
    taxonomy::update_type(
        &cli(&app),
        kind.id,
        crate::model::taxonomy::TypePatch {
            labels: Some(labels),
            fields: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(found(&app, "till slip").await, vec![receipt]);
}

/// One document of every built-in type, then each query must find exactly the
/// document of the type it names. `queries` are (query, type key).
async fn assert_type_queries(ocr_langs: &str, queries: &[(&str, &str)]) {
    let (_dir, app) = test_app_with_langs(ocr_langs).await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    let mut by_key = std::collections::HashMap::new();
    for (_, key) in queries {
        if !by_key.contains_key(key) {
            let id = typed_doc(&app, &format!("{key}.pdf"), key, vec![]).await;
            by_key.insert(*key, id);
        }
    }
    for (query, key) in queries {
        assert_eq!(found(&app, query).await, vec![by_key[key]], "{query}");
    }
}

#[tokio::test]
async fn german_type_names_find_their_types() {
    assert_type_queries(
        "eng+deu",
        &[
            ("Reisepass", "travel_passport"),
            ("Reisepässe", "travel_passport"),
            ("Personalausweis", "id_card"),
            ("Aufenthaltstitel", "residence_permit"),
            ("Führerschein", "driver_license"),
            ("fuhrerscheine", "driver_license"),
            ("Geburtsurkunde", "birth_certificate"),
            ("Heiratsurkunde", "marriage_certificate"),
            ("Versicherungspolice", "insurance_policy"),
            ("Kontoauszug", "bank_statement"),
            ("Rechnung", "invoice"),
            ("Rechnungen", "invoice"),
            ("Quittung", "receipt"),
            ("Garantieschein", "warranty"),
            ("Steuerbescheid", "tax_document"),
            ("Arztbrief", "medical_document"),
        ],
    )
    .await;
}

#[tokio::test]
async fn french_type_names_find_their_types() {
    assert_type_queries(
        "eng+fra",
        &[
            ("passeport", "travel_passport"),
            ("carte d'identité", "id_card"),
            ("titre de séjour", "residence_permit"),
            ("permis de conduire", "driver_license"),
            ("acte de naissance", "birth_certificate"),
            ("acte de mariage", "marriage_certificate"),
            ("police d'assurance", "insurance_policy"),
            ("relevé bancaire", "bank_statement"),
            ("facture", "invoice"),
            ("factures", "invoice"),
            ("reçu", "receipt"),
            ("garantie", "warranty"),
            ("avis d'imposition", "tax_document"),
            ("certificat médical", "medical_document"),
        ],
    )
    .await;
}

#[tokio::test]
async fn spanish_type_names_find_their_types() {
    assert_type_queries(
        "eng+spa",
        &[
            ("pasaporte", "travel_passport"),
            ("documento de identidad", "id_card"),
            ("permiso de residencia", "residence_permit"),
            ("permiso de conducir", "driver_license"),
            ("certificado de nacimiento", "birth_certificate"),
            ("certificado de matrimonio", "marriage_certificate"),
            ("póliza de seguro", "insurance_policy"),
            ("extracto bancario", "bank_statement"),
            ("factura", "invoice"),
            ("facturas", "invoice"),
            ("recibo", "receipt"),
            ("garantía", "warranty"),
            ("declaración de la renta", "tax_document"),
            ("informe médico", "medical_document"),
        ],
    )
    .await;
}

#[tokio::test]
async fn italian_type_names_find_their_types() {
    assert_type_queries(
        "eng+ita",
        &[
            ("passaporto", "travel_passport"),
            ("carta d'identità", "id_card"),
            ("permesso di soggiorno", "residence_permit"),
            ("patente", "driver_license"),
            ("patenti", "driver_license"),
            ("certificato di nascita", "birth_certificate"),
            ("certificato di matrimonio", "marriage_certificate"),
            ("polizza assicurativa", "insurance_policy"),
            ("estratto conto", "bank_statement"),
            ("fattura", "invoice"),
            ("fatture", "invoice"),
            ("scontrino", "receipt"),
            ("garanzia", "warranty"),
            ("dichiarazione dei redditi", "tax_document"),
            ("certificato medico", "medical_document"),
        ],
    )
    .await;
}

/// Polish has no stemmer in the index, so only the listed forms match.
#[tokio::test]
async fn polish_type_names_find_their_types() {
    assert_type_queries(
        "eng+pol",
        &[
            ("paszport", "travel_passport"),
            ("dowód osobisty", "id_card"),
            ("karta pobytu", "residence_permit"),
            ("prawo jazdy", "driver_license"),
            ("akt urodzenia", "birth_certificate"),
            ("akt małżeństwa", "marriage_certificate"),
            ("polisa ubezpieczeniowa", "insurance_policy"),
            ("wyciąg bankowy", "bank_statement"),
            ("faktura", "invoice"),
            ("paragon", "receipt"),
            ("gwarancja", "warranty"),
            ("deklaracja podatkowa", "tax_document"),
            ("dokument medyczny", "medical_document"),
        ],
    )
    .await;
}

#[tokio::test]
async fn hits_show_the_types_summary_fields_in_order() {
    let (_dir, app) = test_app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    let id = typed_doc(
        &app,
        "scan.pdf",
        "travel_passport",
        vec![
            ("surname", FieldValue::Text("Testov".into())),
            ("given_names", FieldValue::Text("Ivan".into())),
            (
                "birth_date",
                FieldValue::Date(chrono::NaiveDate::from_ymd_opt(1990, 1, 2).unwrap()),
            ),
            ("number", FieldValue::Identifier("AP 1234567".into())),
            (
                "expiry_date",
                FieldValue::Date(chrono::NaiveDate::from_ymd_opt(2031, 5, 1).unwrap()),
            ),
        ],
    )
    .await;
    commit(&app).await.unwrap();
    let page = search(
        &cli(&app),
        SearchRequest {
            query: Some("AP1234567".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].id, id);
    let keys: Vec<&str> = page.hits[0].key_fields.keys().map(String::as_str).collect();
    assert_eq!(keys, ["number", "expiry_date"]);
    assert_eq!(found(&app, "field.number:\"AP 1234567\"").await, vec![id]);
}

#[tokio::test]
async fn hits_show_the_summary_when_nothing_is_searched_for() {
    let (_dir, app) = test_app().await;
    let with = make_doc(&app, "lease.pdf").await;
    let without = make_doc(&app, "receipt.pdf").await;
    documents::update_document(
        &cli(&app),
        with,
        DocumentPatch {
            summary: Some(Some("Lease of a flat, signed in May.".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    commit(&app).await.unwrap();
    let page = search(&cli(&app), SearchRequest::default()).await.unwrap();
    let snippet = |id: DocumentId| {
        page.hits
            .iter()
            .find(|h| h.id == id)
            .unwrap()
            .snippet
            .clone()
    };
    assert_eq!(snippet(with), "Lease of a flat, signed in May.");
    assert_ne!(snippet(without), "Lease of a flat, signed in May.");

    // With a text query the summary is not swapped in for the fragment.
    let page = search(
        &cli(&app),
        SearchRequest {
            query: Some("lease".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_ne!(page.hits[0].snippet, "Lease of a flat, signed in May.");
}
