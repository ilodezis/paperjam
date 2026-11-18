//! The LLM steps A-C inside the pipeline, against the mock OpenRouter.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{MockOpenRouter, MockReply, TestEnv, fixture, have_tesseract};
use paperjam::ids::{DocumentId, UserId};
use paperjam::jobs::queue::{ProcessOptions, WorkerPool};
use paperjam::jobs::worker::Worker;
use paperjam::llm::LlmClient;
use paperjam::llm::steps::Steps;
use paperjam::model::document::{
    DocumentPatch, DocumentStatus, DocumentView, FieldEntry, GetOptions, OrganizationLink,
    Provenance, Role,
};
use paperjam::model::fields::FieldValue;
use paperjam::model::taxonomy::{NewCorrespondent, NewPerson};
use paperjam::model::user::NewUser;
use paperjam::pipeline;
use paperjam::search::{SearchRequest, sync};
use paperjam::service::upload::{UploadInput, upload};
use paperjam::service::{Actor, App, Via, documents, taxonomy, users};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Rig {
    _env: TestEnv,
    mock: MockOpenRouter,
    app: Arc<App>,
}

/// An app with seeded types whose LLM client talks to a fresh mock and
/// retries without waiting.
async fn rig(vars: &[(&str, &str)]) -> Rig {
    let env = TestEnv::new();
    let mock = MockOpenRouter::start().await;
    let mut app = env.app_with_llm(&mock, vars).await;
    app.llm = LlmClient::from_config(&app.config).map(|c| c.with_backoff(Duration::from_millis(5)));
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    Rig {
        _env: env,
        mock,
        app: Arc::new(app),
    }
}

async fn upload_fixture(app: &App, name: &str, local_only: bool) -> DocumentId {
    let bytes = std::fs::read(fixture(name)).unwrap();
    upload(
        &app.ctx(Actor::system()),
        UploadInput {
            original_name: name.to_string(),
            local_only,
            ..Default::default()
        },
        bytes.as_slice(),
    )
    .await
    .unwrap()
    .document_id()
}

async fn user_with_locale(app: &App, name: &str, locale: &str) -> UserId {
    users::create_user(
        &app.ctx(Actor::cli()),
        NewUser {
            username: name.to_string(),
            display_name: name.to_string(),
            password: secrecy::SecretString::from("correct horse battery".to_string()),
            locale: locale.to_string(),
            is_admin: false,
        },
    )
    .await
    .unwrap()
    .id
}

async fn upload_fixture_as(app: &App, name: &str, user: UserId) -> DocumentId {
    let bytes = std::fs::read(fixture(name)).unwrap();
    upload(
        &app.ctx(Actor::user(user, Via::Web)),
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

async fn view(app: &App, id: DocumentId) -> DocumentView {
    documents::get_document(&app.ctx(Actor::system()), id, GetOptions::default())
        .await
        .unwrap()
}

fn field<'a>(doc: &'a DocumentView, key: &str) -> Option<&'a FieldEntry> {
    doc.fields.iter().find(|f| f.key == key)
}

async fn sources(app: &App, id: DocumentId) -> Vec<String> {
    sqlx::query_scalar("SELECT DISTINCT source FROM text_layers WHERE document_id = ? ORDER BY 1")
        .bind(id)
        .fetch_all(&app.db.read)
        .await
        .unwrap()
}

async fn usage_steps(app: &App) -> Vec<String> {
    sqlx::query_scalar("SELECT step FROM llm_usage ORDER BY id")
        .fetch_all(&app.db.read)
        .await
        .unwrap()
}

async fn process_all(app: &App, id: DocumentId) {
    pipeline::process_document(app, id, ProcessOptions::default())
        .await
        .unwrap();
}

/// A valid classification answer; `extra` overrides or adds properties.
fn classify_json(extra: Value) -> Value {
    let mut base = json!({
        "type_key": "contract",
        "title": "Contract — Lease agreement",
        "document_date": "2024-03-01",
        "organizations": [{"id": null, "new_name": "Acme Rentals", "role": "issued_by", "confident": true}],
        "new_tags": [],
        "summary": "A lease of a flat.",
        "language": "en",
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn classify_reply(extra: Value) -> MockReply {
    MockReply::ok(&classify_json(extra))
}

/// A valid answer for step C: every field of the type present, `null` unless
/// overridden.
async fn fields_json(app: &App, type_key: &str, values: &[(&str, Value)]) -> Value {
    fields_json_naming(app, type_key, values, &[]).await
}

/// Like [`fields_json`], and the model also says that the issuer fields in
/// `organizations` name an organization (every other issuer field is a place).
async fn fields_json_naming(
    app: &App,
    type_key: &str,
    values: &[(&str, Value)],
    organizations: &[&str],
) -> Value {
    let defs: String = sqlx::query_scalar("SELECT fields FROM document_types WHERE key = ?")
        .bind(type_key)
        .fetch_one(&app.db.read)
        .await
        .unwrap();
    let defs: Vec<Value> = serde_json::from_str(&defs).unwrap();
    let mut fields = serde_json::Map::new();
    for def in defs {
        let key = def["key"].as_str().unwrap().to_string();
        // Unknown is an empty value of the field's shape and page 0.
        let empty = match def["kind"].as_str().unwrap() {
            "money" => json!({"amount": "", "currency": ""}),
            "list" => json!([]),
            _ => json!(""),
        };
        let mut value = values.iter().find(|(k, _)| *k == key).map_or(
            json!({"value": empty, "page": 0}),
            |(_, v)| json!({"value": v, "page": 1}),
        );
        if def["issuer"] == json!(true) {
            value["names_organization"] = json!(organizations.contains(&key.as_str()));
        }
        fields.insert(key, value);
    }
    json!({"fields": fields, "facts": []})
}

async fn fields_reply(app: &App, type_key: &str, values: &[(&str, Value)]) -> MockReply {
    MockReply::ok(&fields_json(app, type_key, values).await)
}

async fn fields_reply_naming(
    app: &App,
    type_key: &str,
    values: &[(&str, Value)],
    organizations: &[&str],
) -> MockReply {
    MockReply::ok(&fields_json_naming(app, type_key, values, organizations).await)
}

fn transcription_reply(text: &str) -> MockReply {
    MockReply::ok(&json!({"pages": [{"page": 1, "markdown": text, "illegible": false}]}))
}

// --- the steps ---------------------------------------------------------------

#[tokio::test]
async fn born_digital_skips_transcription() {
    let r = rig(&[]).await;
    r.mock.route("classification", classify_reply(json!({})));
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[("subject", json!("Rent of a flat"))]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let sent = r.mock.requests();
    let names: Vec<_> = sent.iter().filter_map(|q| q.schema_name()).collect();
    assert_eq!(names, ["classification", "field_extraction"]);
    // Only the classification sees the first page, as a small image.
    let images: Vec<_> = sent.iter().map(|q| q.image_count()).collect();
    assert_eq!(images, [1, 0]);
    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    // The type label the model put in front is dropped: the type is a chip.
    assert_eq!(doc.title.as_deref(), Some("Lease agreement"));
    assert_eq!(doc.title_provenance, Some(Provenance::Llm));
    assert_eq!(doc.type_key.as_deref(), Some("contract"));
    assert_eq!(doc.type_provenance, Some(Provenance::Llm));
    assert_eq!(doc.document_date.unwrap().to_string(), "2024-03-01");
    assert_eq!(doc.summary.as_deref(), Some("A lease of a flat."));
    assert_eq!(doc.language.as_deref(), Some("en"));
    assert_eq!(doc.organizations.len(), 1);
    let correspondent = &doc.organizations[0];
    assert_eq!(correspondent.name, "Acme Rentals");
    assert_eq!(correspondent.role, Role::IssuedBy);
    assert_eq!(correspondent.provenance, Some(Provenance::Llm));
    let subject = field(&doc, "subject").unwrap();
    assert_eq!(subject.provenance, Provenance::Llm);
    assert_eq!(subject.value, FieldValue::Text("Rent of a flat".into()));
    assert_eq!(subject.page, Some(1));
    assert!(doc.attention.is_empty(), "attention: {:?}", doc.attention);
    assert_eq!(sources(&r.app, id).await, vec!["embedded"]);
    assert_eq!(usage_steps(&r.app).await, ["classify", "fields"]);
}

#[tokio::test]
async fn scan_goes_through_three_steps() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    r.mock.route(
        "transcription",
        transcription_reply("# Паспорт гражданина\nТестов Иван"),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "other"})),
    );
    let mut reply = fields_json(&r.app, "other", &[]).await;
    reply["facts"] =
        json!([{"key": "stamp", "label": "Stamp", "value": "round blue stamp", "page": 1}]);
    r.mock.route("field_extraction", MockReply::ok(&reply));
    let id = upload_fixture(&r.app, "scan_ru.png", false).await;

    process_all(&r.app, id).await;

    let sent = r.mock.requests();
    let names: Vec<_> = sent.iter().filter_map(|q| q.schema_name()).collect();
    assert_eq!(
        names,
        ["transcription", "classification", "field_extraction"]
    );
    let images: Vec<_> = sent.iter().map(|q| q.image_count()).collect();
    assert_eq!(images, [1, 0, 0], "images go out in the first step only");
    // The cached WebP render goes out as it is, not re-encoded.
    let image = sent[0].body["messages"][1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "image_url")
        .unwrap()["image_url"]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        image.starts_with("data:image/webp;base64,"),
        "{}",
        &image[..40]
    );
    // Step B read the transcription, not only the OCR.
    let user_text = sent[1].body["messages"][1]["content"].to_string();
    assert!(user_text.contains("Тестов Иван"), "{user_text}");

    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.type_key.as_deref(), Some("other"));
    assert_eq!(sources(&r.app, id).await, vec!["llm", "tesseract"]);
    let text = documents::get_document(
        &r.app.ctx(Actor::system()),
        id,
        GetOptions { text_page: Some(1) },
    )
    .await
    .unwrap()
    .text
    .unwrap();
    assert_eq!(text.source, "llm");
    assert!(text.text.contains("Паспорт гражданина"));
    assert_eq!(doc.facts.len(), 1);
    assert_eq!(doc.facts[0].key, "stamp");
    assert_eq!(doc.facts[0].provenance, Provenance::Llm);
    assert_eq!(
        usage_steps(&r.app).await,
        ["transcribe", "classify", "fields"]
    );
}

#[tokio::test]
async fn local_only_document_never_calls_llm() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", true).await;

    process_all(&r.app, id).await;

    assert_eq!(r.mock.request_count(), 0);
    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.title_provenance, Some(Provenance::Local));
    assert!(doc.attention.is_empty());
    assert!(usage_steps(&r.app).await.is_empty());

    // The step runner itself refuses such a document.
    let client = r.app.llm.as_ref().unwrap();
    let err = Steps::new(&r.app, client, id, &ProcessOptions::default())
        .await
        .err()
        .expect("local_only documents are refused");
    assert_eq!(err.code, "llm_not_allowed");
}

#[tokio::test]
async fn without_a_key_the_llm_layer_is_off() {
    let env = TestEnv::new();
    let app = env.app().await;
    taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
        .await
        .unwrap();
    assert!(app.llm.is_none());
    let id = upload_fixture(&app, "contract_ru_digital.pdf", false).await;
    process_all(&app, id).await;
    let doc = view(&app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert!(doc.attention.is_empty());
}

#[tokio::test]
async fn llm_picks_existing_person_by_alias() {
    let r = rig(&[]).await;
    let ctx = r.app.ctx(Actor::system());
    let person = taxonomy::create_person(
        &ctx,
        NewPerson {
            name: "Marik Testov".into(),
            aliases: vec!["Maric Testov".into(), "Марик Тестов".into()],
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    r.mock.route(
        "classification",
        classify_reply(json!({"people": [person.id.to_string()]})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    // The model saw the person with every alias, by id.
    let asked = r.mock.requests()[0].body["messages"][1]["content"].to_string();
    assert!(asked.contains(&person.id.to_string()), "{asked}");
    assert!(asked.contains("Maric Testov"), "{asked}");
    let doc = view(&r.app, id).await;
    assert_eq!(doc.people.len(), 1);
    assert_eq!(doc.people[0].id, person.id);
    assert_eq!(doc.people[0].provenance, Provenance::Llm);
}

#[tokio::test]
async fn llm_cannot_create_people() {
    // An id that is not in the archive, with and without other people around.
    for with_person in [true, false] {
        let r = rig(&[]).await;
        if with_person {
            taxonomy::create_person(
                &r.app.ctx(Actor::system()),
                NewPerson {
                    name: "Real Person".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        r.mock.route(
            "classification",
            classify_reply(json!({"people": ["019a0000-0000-7000-8000-000000000000"]})),
        );
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

        process_all(&r.app, id).await;

        let doc = view(&r.app, id).await;
        assert_eq!(doc.status, DocumentStatus::Ready);
        assert!(doc.people.is_empty(), "with_person={with_person}");
        assert_eq!(doc.attention, ["llm_failed"], "with_person={with_person}");
        assert_eq!(doc.title_provenance, Some(Provenance::Local));
        assert_eq!(r.mock.request_count(), 1);
    }
}

#[tokio::test]
async fn new_tags_are_offered_only_when_allowed() {
    for allowed in [false, true] {
        let vars: &[(&str, &str)] = if allowed {
            &[]
        } else {
            &[("PAPERJAM_LLM_CREATE_TAGS", "false")]
        };
        let r = rig(vars).await;
        taxonomy::create_tag(&r.app.ctx(Actor::system()), "warranty", None)
            .await
            .unwrap();
        let mut reply = classify_json(json!({"tags": ["warranty"]}));
        if allowed {
            reply["new_tags"] = json!(["rent"]);
        } else {
            // The schema does not offer new tags, so the answer has none.
            reply.as_object_mut().unwrap().remove("new_tags");
        }
        r.mock.route("classification", MockReply::ok(&reply));
        r.mock.route(
            "field_extraction",
            fields_reply(&r.app, "contract", &[]).await,
        );
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

        process_all(&r.app, id).await;

        let schema = &r.mock.requests()[0].body["response_format"]["json_schema"]["schema"];
        assert_eq!(schema["properties"].get("new_tags").is_some(), allowed);
        let doc = view(&r.app, id).await;
        assert!(doc.attention.is_empty(), "{:?}", doc.attention);
        let mut tags: Vec<_> = doc.tags.iter().map(|t| t.name.as_str()).collect();
        tags.sort_unstable();
        let expected: &[&str] = if allowed {
            &["rent", "warranty"]
        } else {
            &["warranty"]
        };
        assert_eq!(tags, expected);
        assert!(doc.tags.iter().all(|t| t.provenance == Provenance::Llm));
    }
}

#[tokio::test]
async fn a_known_correspondent_is_reused_not_duplicated() {
    let r = rig(&[]).await;
    let known = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        paperjam::model::taxonomy::NewCorrespondent {
            name: "ACME Rentals".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    // The model proposes a new name that is the known one in other spelling.
    r.mock.route(
        "classification",
        classify_reply(
            json!({"organizations": [{"id": null, "new_name": "Acme  rentals", "role": "issued_by", "confident": true}]}),
        ),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.organizations[0].id, known.id);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM correspondents")
        .fetch_one(&r.app.db.read)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn model_override_replaces_both_models() {
    let r = rig(&[]).await;
    r.mock.route("classification", classify_reply(json!({})));
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            model_override: Some("vendor/other-model".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    for request in r.mock.requests() {
        assert_eq!(request.body["model"], "vendor/other-model");
    }
}

#[tokio::test]
async fn llm_metadata_and_transcription_are_searchable() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"title": "Zebrafish tenancy", "summary": "Marmalade clause."})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;
    sync::commit(&r.app).await.unwrap();

    for query in ["zebrafish", "marmalade"] {
        let page = sync::search(
            &r.app.ctx(Actor::system()),
            SearchRequest {
                query: Some(query.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
            [id],
            "{query}"
        );
    }
}

// --- failures keep the local results ---------------------------------------------

#[tokio::test]
async fn llm_garbage_keeps_local_results() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    // (what OpenRouter answers to every request, requests until giving up)
    let storms = [
        ("invalid json", MockReply::raw("Sure! {\"pages\": ["), 1),
        ("refusal", MockReply::refusal("I cannot help with this."), 1),
        (
            "truncated",
            MockReply::truncated("{\"pages\": [{\"page\": 1, \"mark"),
            1,
        ),
        ("empty", MockReply::empty(), 1),
        (
            "schema mismatch",
            MockReply::ok(&json!({"pages": "not a list"})),
            1,
        ),
        ("5xx storm", MockReply::status(503), 4),
    ];
    for (name, reply, requests) in storms {
        let r = rig(&[]).await;
        r.mock.set_fallback(reply);
        let id = upload_fixture(&r.app, "passport_td3.png", false).await;

        process_all(&r.app, id).await;

        let doc = view(&r.app, id).await;
        assert_eq!(doc.status, DocumentStatus::Ready, "{name}");
        assert!(
            doc.attention.contains(&"llm_failed".to_string()),
            "{name}: {:?}",
            doc.attention
        );
        assert_eq!(r.mock.request_count(), requests, "{name}");
        // Local results are intact ...
        assert_eq!(doc.type_key.as_deref(), Some("travel_passport"), "{name}");
        assert_eq!(doc.type_provenance, Some(Provenance::Mrz), "{name}");
        assert_eq!(
            field(&doc, "surname").map(|f| f.provenance),
            Some(Provenance::Mrz),
            "{name}"
        );
        assert!(
            sources(&r.app, id).await.contains(&"tesseract".to_string()),
            "{name}"
        );
        // ... and nothing from the model was written.
        assert!(
            !sources(&r.app, id).await.contains(&"llm".to_string()),
            "{name}"
        );
        assert!(
            doc.fields.iter().all(|f| f.provenance != Provenance::Llm),
            "{name}"
        );
        assert!(
            doc.facts.iter().all(|f| f.provenance != Provenance::Llm),
            "{name}"
        );
        assert_eq!(doc.title_provenance, Some(Provenance::Local), "{name}");
        assert!(doc.summary.is_none(), "{name}");
        // (the passport's holder becomes a person locally, never through the model)
        assert!(
            doc.people.iter().all(|p| p.provenance != Provenance::Llm),
            "{name}"
        );
        assert!(doc.tags.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn turning_local_only_on_mid_flight_discards_the_late_answer() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({})).delayed(Duration::from_millis(800)),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    let app = Arc::clone(&r.app);
    let run = tokio::spawn(async move { process_all(&app, id).await });
    wait_for("the classification request", || async {
        r.mock.request_count() == 1
    })
    .await;
    // The user flips "never send to the cloud" while the answer is on its way.
    documents::update_document(
        &r.app.ctx(Actor::system()),
        id,
        DocumentPatch {
            local_only: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run.await.unwrap();

    let doc = view(&r.app, id).await;
    assert!(doc.local_only);
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_ne!(doc.title_provenance, Some(Provenance::Llm));
    assert!(doc.summary.is_none());
    assert!(doc.organizations.is_empty());
    assert_eq!(r.mock.request_count(), 1, "step C must not start");
    assert!(
        doc.attention.is_empty(),
        "not a failure of the LLM: {:?}",
        doc.attention
    );
}

#[tokio::test]
async fn a_failed_step_keeps_the_steps_before_it() {
    let r = rig(&[]).await;
    r.mock.route("classification", classify_reply(json!({})));
    r.mock.route("field_extraction", MockReply::raw("not json"));
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.title.as_deref(), Some("Lease agreement"));
    assert!(doc.fields.iter().all(|f| f.provenance != Provenance::Llm));
    assert_eq!(doc.attention, ["llm_failed"]);
    // The unusable answer still cost money, and it is on the books.
    assert_eq!(usage_steps(&r.app).await, ["classify", "fields"]);
}

#[tokio::test]
async fn a_successful_rerun_clears_llm_failed_and_replaces_llm_values() {
    let r = rig(&[]).await;
    r.mock.set_fallback(MockReply::status(500));
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    process_all(&r.app, id).await;
    assert_eq!(view(&r.app, id).await.attention, ["llm_failed"]);

    r.mock
        .route("classification", classify_reply(json!({"title": "First"})));
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[("subject", json!("one"))]).await,
    );
    process_all(&r.app, id).await;
    let doc = view(&r.app, id).await;
    assert!(doc.attention.is_empty(), "{:?}", doc.attention);
    assert_eq!(doc.title.as_deref(), Some("First"));

    r.mock
        .route("classification", classify_reply(json!({"title": "Second"})));
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "contract",
            &[("amount", json!({"amount": "99.50", "currency": "eur"}))],
        )
        .await,
    );
    process_all(&r.app, id).await;
    let doc = view(&r.app, id).await;
    assert_eq!(doc.title.as_deref(), Some("Second"));
    assert!(
        field(&doc, "subject").is_none(),
        "the old LLM value is replaced"
    );
    assert_eq!(
        field(&doc, "amount").unwrap().value,
        FieldValue::Money {
            amount: "99.50".parse().unwrap(),
            currency: "EUR".into()
        }
    );
}

// --- budget ----------------------------------------------------------------------

#[tokio::test]
async fn budget_stop_between_steps_keeps_partial() {
    let r = rig(&[("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "0.001")]).await;
    // Step B alone spends more than the whole budget.
    r.mock.route(
        "classification",
        MockReply::ok_with_usage(&classify_json(json!({})), 1000, 200, 0.002),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    assert_eq!(r.mock.request_count(), 1, "step C is skipped");
    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(
        doc.title.as_deref(),
        Some("Lease agreement"),
        "step B is kept"
    );
    assert_eq!(doc.attention, ["llm_budget"]);
    assert_eq!(usage_steps(&r.app).await, ["classify"]);
}

/// A generic endpoint reports no cost: the configured prices keep the budget
/// working, and without them the spend is recorded as zero (tokens still
/// counted) so the budget cannot stop anything.
#[tokio::test]
async fn generic_budget_uses_the_price_table() {
    let r = rig(&[
        ("PAPERJAM_LLM_PROVIDER", "generic"),
        ("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "0.001"),
        (
            "PAPERJAM_LLM_PRICES",
            "anthropic/claude-haiku-5.5=1000:1000",
        ),
    ])
    .await;
    r.mock.route(
        "classification",
        MockReply::ok_with_usage(&classify_json(json!({})), 1000, 200, 0.0).without_cost(),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await.without_cost(),
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    assert_eq!(r.mock.request_count(), 1, "step C is skipped");
    assert!(r.mock.requests()[0].body.get("provider").is_none());
    assert_eq!(view(&r.app, id).await.attention, ["llm_budget"]);
    let cost: String = sqlx::query_scalar("SELECT cost_usd FROM llm_usage")
        .fetch_one(&r.app.db.read)
        .await
        .unwrap();
    // 1000 * 1000 / 1e6 + 200 * 1000 / 1e6
    assert_eq!(
        cost.parse::<rust_decimal::Decimal>().unwrap(),
        "1.2".parse().unwrap()
    );
}

#[tokio::test]
async fn generic_without_prices_records_tokens_at_zero_cost() {
    let r = rig(&[
        ("PAPERJAM_LLM_PROVIDER", "generic"),
        ("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "0.001"),
    ])
    .await;
    r.mock.route(
        "classification",
        MockReply::ok_with_usage(&classify_json(json!({})), 1000, 200, 0.0).without_cost(),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await.without_cost(),
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    assert_eq!(r.mock.request_count(), 2, "nothing is ever over budget");
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT input_tokens, cost_usd FROM llm_usage")
        .fetch_all(&r.app.db.read)
        .await
        .unwrap();
    assert!(
        rows.len() >= 2 && rows.iter().all(|(t, c)| *t > 0 && c == "0"),
        "{rows:?}"
    );
}

#[tokio::test]
async fn an_exhausted_budget_makes_no_requests() {
    let r = rig(&[("PAPERJAM_LLM_MONTHLY_BUDGET_USD", "0.01")]).await;
    sqlx::query(
        "INSERT INTO llm_usage (model, step, input_tokens, output_tokens, cost_usd, created_at) \
         VALUES ('m', 'classify', 1, 1, '0.01', ?)",
    )
    .bind(paperjam::time::now_ts())
    .execute(&r.app.db.write)
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    assert_eq!(r.mock.request_count(), 0);
    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    assert_eq!(doc.attention, ["llm_budget"]);
}

// --- provenance ----------------------------------------------------------------------

#[tokio::test]
async fn manual_fields_survive_llm() {
    let r = rig(&[]).await;
    let ctx = r.app.ctx(Actor::system());
    let contract = taxonomy::list_taxonomy(&ctx)
        .await
        .unwrap()
        .types
        .into_iter()
        .find(|t| t.key == "contract")
        .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let patch = DocumentPatch {
        title: Some(Some("My lease".into())),
        type_id: Some(Some(contract.id)),
        summary: Some(Some("My own summary".into())),
        fields: [("subject".to_string(), Some(FieldValue::Text("Mine".into())))].into(),
        ..Default::default()
    };
    documents::update_document(&ctx, id, patch).await.unwrap();

    // The model disagrees with the user on everything.
    r.mock.route(
        "classification",
        classify_reply(
            json!({"type_key": "invoice", "title": "Model title", "summary": "Model summary"}),
        ),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "contract",
            &[
                ("subject", json!("Model subject")),
                ("parties", json!(["A", "B"])),
            ],
        )
        .await,
    );
    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.title.as_deref(), Some("My lease"));
    assert_eq!(doc.title_provenance, Some(Provenance::Manual));
    assert_eq!(doc.type_key.as_deref(), Some("contract"));
    assert_eq!(doc.type_provenance, Some(Provenance::Manual));
    assert_eq!(doc.summary.as_deref(), Some("My own summary"));
    let subject = field(&doc, "subject").unwrap();
    assert_eq!(subject.provenance, Provenance::Manual);
    assert_eq!(subject.value, FieldValue::Text("Mine".into()));
    let parties = field(&doc, "parties").unwrap();
    assert_eq!(parties.provenance, Provenance::Llm);
    assert_eq!(
        parties.value,
        FieldValue::List(vec!["A".into(), "B".into()])
    );
}

#[tokio::test]
async fn an_mrz_value_beats_the_model_and_a_disagreement_is_flagged() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    r.mock
        .route("transcription", transcription_reply("Passport of Testov"));
    r.mock.route(
        "classification",
        // (the holder becomes a person only after the model checked the
        // names, so the schema asks for no `people` yet)
        classify_reply(json!({"type_key": "travel_passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "travel_passport",
            &[
                ("number", json!("ZZ9999999")),
                ("issuing_authority", json!("Test Authority")),
            ],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "passport_td3.png", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.status, DocumentStatus::Ready);
    let number = field(&doc, "number").unwrap();
    assert_eq!(number.provenance, Provenance::Mrz);
    assert_ne!(number.value, FieldValue::Identifier("ZZ9999999".into()));
    assert!(
        doc.attention.contains(&"mrz_mismatch".to_string()),
        "{:?}",
        doc.attention
    );
    assert_eq!(
        field(&doc, "issuing_authority").unwrap().provenance,
        Provenance::Llm
    );
}

// --- scheduling ----------------------------------------------------------------------

async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..300 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn llm_follow_up_runs_in_the_llm_pool() {
    let r = rig(&[]).await;
    r.mock.route("classification", classify_reply(json!({})));
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    // Only the OCR pool runs: the document gets its local results and an
    // LLM-only job is left behind.
    let cancel = CancellationToken::new();
    let ocr = Worker::spawn(
        Arc::clone(&r.app),
        WorkerPool::Ocr,
        1,
        cancel.clone(),
        pipeline::handler(),
    );
    wait_for("the local phase", || async {
        view(&r.app, id).await.status == DocumentStatus::Ready
            && r.app.jobs.counts().await.unwrap().queued == 1
    })
    .await;
    assert_eq!(
        r.mock.request_count(),
        0,
        "the OCR pool never calls the LLM"
    );
    assert_eq!(
        view(&r.app, id).await.title_provenance,
        Some(Provenance::Local)
    );
    let payload: String = sqlx::query_scalar("SELECT payload FROM jobs WHERE state = 'queued'")
        .fetch_one(&r.app.db.read)
        .await
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["local"], false);
    assert_eq!(payload["llm"], true);

    // The LLM pool picks it up.
    let llm = Worker::spawn(
        Arc::clone(&r.app),
        WorkerPool::Llm,
        1,
        cancel.clone(),
        pipeline::handler(),
    );
    wait_for("the LLM phase", || async {
        let doc = view(&r.app, id).await;
        doc.status == DocumentStatus::Ready && doc.title_provenance == Some(Provenance::Llm)
    })
    .await;
    assert_eq!(r.mock.request_count(), 2);
    cancel.cancel();
    let _ = ocr.await;
    let _ = llm.await;
}

/// The OCR pool's half of a run is not its end: a holder named only by the
/// zone (OCR noise in a names line verifies like the rest) must wait for the
/// model's reading, and a zone name the page contradicts never becomes one.
#[tokio::test]
async fn a_holder_waits_for_the_llm_follow_up_before_becoming_a_person() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    r.mock
        .route("transcription", transcription_reply("Passport of Petrov"));
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "travel_passport", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "travel_passport",
            &[("surname", json!("PETROV")), ("given_names", json!("IVAN"))],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "passport_td3.png", false).await;

    let cancel = CancellationToken::new();
    let ocr = Worker::spawn(
        Arc::clone(&r.app),
        WorkerPool::Ocr,
        1,
        cancel.clone(),
        pipeline::handler(),
    );
    wait_for("the local phase", || async {
        view(&r.app, id).await.status == DocumentStatus::Ready
            && r.app.jobs.counts().await.unwrap().queued == 1
    })
    .await;
    assert!(
        view(&r.app, id).await.people.is_empty(),
        "the zone's name became a person before the model read the page"
    );

    let llm = Worker::spawn(
        Arc::clone(&r.app),
        WorkerPool::Llm,
        1,
        cancel.clone(),
        pipeline::handler(),
    );
    wait_for("the LLM phase", || async {
        let doc = view(&r.app, id).await;
        doc.status == DocumentStatus::Ready && doc.title_provenance == Some(Provenance::Llm)
    })
    .await;
    let doc = view(&r.app, id).await;
    assert!(
        doc.attention.contains(&"mrz_mismatch".to_string()),
        "{:?}",
        doc.attention
    );
    assert!(doc.people.is_empty(), "{:?}", doc.people);
    let people = taxonomy::list_taxonomy(&r.app.ctx(Actor::system()))
        .await
        .unwrap()
        .people;
    assert!(people.is_empty(), "{people:?}");
    cancel.cancel();
    let _ = ocr.await;
    let _ = llm.await;
}

#[tokio::test]
async fn a_bad_mrz_flag_survives_the_llm_only_follow_up() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    r.mock.set_fallback(MockReply::status(500));
    let id = upload_fixture(&r.app, "passport_td3_badcheck.png", false).await;
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            llm: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        view(&r.app, id)
            .await
            .attention
            .contains(&"checksum_failed".to_string())
    );

    // The follow-up recomputes the flags; the verdict on the scan stays.
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            local: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let attention = view(&r.app, id).await.attention;
    assert!(
        attention.contains(&"checksum_failed".to_string()),
        "{attention:?}"
    );
    assert!(
        attention.contains(&"llm_failed".to_string()),
        "{attention:?}"
    );
}

// --- language, titles, issuers and normalization -------------------------------------

/// What the classification request told the model, system prompt first.
fn classify_request(r: &Rig) -> (String, String) {
    let sent = r.mock.requests();
    let q = sent
        .iter()
        .rev()
        .find(|q| q.schema_name() == Some("classification"))
        .expect("a classification request");
    (
        q.body["messages"][0]["content"].to_string(),
        q.body["messages"][1]["content"].to_string(),
    )
}

async fn classify_only(r: &Rig, answer: Value, id: DocumentId) -> DocumentView {
    r.mock.route("classification", classify_reply(answer));
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "contract", &[]).await,
    );
    process_all(&r.app, id).await;
    view(&r.app, id).await
}

#[tokio::test]
async fn the_model_writes_in_the_language_of_the_user_who_uploaded() {
    for (locale, language, label) in [
        ("ru", "Russian", "Договор / Contract"),
        ("en", "English", "Contract / Договор"),
    ] {
        let r = rig(&[]).await;
        let user = user_with_locale(&r.app, "alice", locale).await;
        let id = upload_fixture_as(&r.app, "contract_ru_digital.pdf", user).await;
        classify_only(&r, json!({}), id).await;

        let (_, asked) = classify_request(&r);
        assert!(
            asked.contains(&format!("Output language: {language}")),
            "{asked}"
        );
        assert!(asked.contains(&format!("- contract: {label}")), "{asked}");
    }
}

#[tokio::test]
async fn a_document_without_an_uploader_is_described_in_english() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    classify_only(&r, json!({}), id).await;
    let (_, asked) = classify_request(&r);
    assert!(asked.contains("Output language: English"), "{asked}");
}

/// The model's request parts of one kind, in order.
fn parts<'a>(q: &'a common::RecordedRequest, kind: &str) -> Vec<&'a Value> {
    q.body["messages"][1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|part| part["type"] == kind)
        .collect()
}

#[tokio::test]
async fn a_born_digital_classification_carries_a_small_render_of_page_one() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    classify_only(&r, json!({}), id).await;

    let sent = r.mock.requests();
    let classify = sent
        .iter()
        .find(|q| q.schema_name() == Some("classification"))
        .unwrap();
    let images = parts(classify, "image_url");
    assert_eq!(images.len(), 1, "page 1, once");
    let url = images[0]["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/"), "{}", &url[..30]);
    // The preview is low resolution: a few hundred KB at the most.
    assert!(url.len() < 400_000, "{} bytes", url.len());
    // The text goes along, and the model is told what the image is for.
    let text = parts(classify, "text")
        .iter()
        .map(|p| p["text"].as_str().unwrap_or_default())
        .collect::<String>();
    assert!(text.contains("first page is attached"), "{text}");
    assert!(text.contains("BEGIN DOCUMENT"), "{text}");
    // The fields step stays text-only.
    let fields = sent
        .iter()
        .find(|q| q.schema_name() == Some("field_extraction"))
        .unwrap();
    assert_eq!(fields.image_count(), 0);
}

#[tokio::test]
async fn a_local_only_born_digital_document_sends_neither_text_nor_image() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", true).await;
    r.mock.route("classification", classify_reply(json!({})));
    process_all(&r.app, id).await;
    assert_eq!(r.mock.request_count(), 0);
}

#[tokio::test]
async fn the_title_has_no_type_label_in_front_and_no_correspondent_before_it() {
    let r = rig(&[]).await;
    let user = user_with_locale(&r.app, "alice", "ru").await;
    let id = upload_fixture_as(&r.app, "contract_ru_digital.pdf", user).await;

    // The model puts the correspondent first and writes the type in Russian.
    let doc = classify_only(
        &r,
        json!({
            "title": "Acme Rentals - Договор — Flat on Main Street, 2024",
            "organizations": [{"id": null, "new_name": "Acme Rentals", "role": "issued_by", "confident": true}],
        }),
        id,
    )
    .await;

    assert_eq!(doc.title.as_deref(), Some("Flat on Main Street, 2024"));
    assert_eq!(doc.organizations[0].name, "Acme Rentals");
}

#[tokio::test]
async fn classification_prompt_instructs_issue_year_for_identity_documents() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    classify_only(&r, json!({}), id).await;
    let (system, _) = classify_request(&r);
    assert!(
        system.contains("Identity documents and personal certificates: the holder, then the year of issue (never the year of expiry)"),
        "expected updated title rule in classify prompt: {system}"
    );
    assert!(
        system.contains("Never select a listed correspondent whose name is only a place"),
        "expected rule against selecting bare places in classify prompt: {system}"
    );
}

#[tokio::test]
async fn identity_document_title_with_issue_year_is_kept() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({
            "type_key": "passport",
            "title": "Иван Тестов, 2021",
            "organizations": []
        }),
        id,
    )
    .await;
    assert_eq!(doc.title.as_deref(), Some("Иван Тестов, 2021"));
}

#[tokio::test]
async fn a_title_that_starts_with_a_known_correspondent_loses_it() {
    let r = rig(&[]).await;
    taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Springfield Registry".into(),
            aliases: vec!["SR".into(), "Спрингфилд".into()],
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    // Not the correspondent the model chose, only one the archive knows.
    let doc = classify_only(
        &r,
        json!({
            "title": "Springfield Registry: Contract — Flat, 2024",
            "organizations": [{"id": null, "new_name": null, "role": "unspecified", "confident": false}],
        }),
        id,
    )
    .await;

    assert_eq!(doc.title.as_deref(), Some("Flat, 2024"));
}

#[tokio::test]
async fn an_issuer_the_model_is_unsure_of_is_not_set() {
    let r = rig(&[]).await;
    let known = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Agency of Public Services".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    // The summary says the authority is illegible; the picks say otherwise.
    for pick in [
        json!([{"id": known.id.to_string(), "new_name": null, "role": "issued_by", "confident": false}]),
        json!([{"id": null, "new_name": "Agentia Servicii Publice", "role": "issued_by", "confident": false}]),
    ] {
        let doc = classify_only(
            &r,
            json!({
                "organizations": pick,
                "summary": "The issuing authority is illegible.",
            }),
            id,
        )
        .await;
        assert!(doc.organizations.is_empty(), "{pick}");
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM correspondents")
        .fetch_one(&r.app.db.read)
        .await
        .unwrap();
    assert_eq!(count, 1, "no correspondent was created from a guess");
}

#[tokio::test]
async fn a_reprocess_that_is_unsure_clears_the_issuer_the_model_set_before() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(&r, json!({}), id).await;
    assert_eq!(doc.organizations[0].name, "Acme Rentals");

    let doc = classify_only(
        &r,
        json!({"organizations": [{"id": null, "new_name": null, "role": "unspecified", "confident": false}]}),
        id,
    )
    .await;
    assert!(doc.organizations.is_empty());

    // A correspondent the user chose is not the model's to clear.
    let ctx = r.app.ctx(Actor::system());
    let mine = taxonomy::create_correspondent(
        &ctx,
        NewCorrespondent {
            name: "My bank".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    documents::update_document(
        &ctx,
        id,
        DocumentPatch {
            organizations: Some(vec![OrganizationLink {
                id: mine.id,
                role: Role::IssuedBy,
            }]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let doc = classify_only(
        &r,
        json!({"organizations": [{"id": null, "new_name": null, "role": "unspecified", "confident": false}]}),
        id,
    )
    .await;
    assert_eq!(doc.organizations[0].id, mine.id);
}

#[tokio::test]
async fn a_correspondent_name_never_carries_the_field_label() {
    for (given, expected) in [
        ("Issuing authority: Acme Registry", Some("Acme Registry")),
        ("Autoritatea emitentă: Acme Registry", Some("Acme Registry")),
        ("Issuing authority:", None),
        ("[illegible]", None),
        ("N/A", None),
    ] {
        let r = rig(&[]).await;
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
        let doc = classify_only(
            &r,
            json!({"organizations": [{"id": null, "new_name": given, "role": "issued_by", "confident": true}]}),
            id,
        )
        .await;
        assert_eq!(
            doc.organizations.first().map(|c| c.name.as_str()),
            expected,
            "{given}"
        );
    }
}

#[tokio::test]
async fn llm_write_multiple_organizations_roles_and_manual_protection() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    // 1. Classification with 2 confident organizations (roles issued_by, party) and 1 not-confident.
    let doc = classify_only(
        &r,
        json!({
            "organizations": [
                {"id": null, "new_name": "Alpha Issuer", "role": "issued_by", "confident": true},
                {"id": null, "new_name": "Beta Counterparty", "role": "party", "confident": true},
                {"id": null, "new_name": "Gamma Unsure", "role": "employer", "confident": false},
            ]
        }),
        id,
    )
    .await;

    assert_eq!(doc.organizations.len(), 2);
    assert_eq!(doc.organizations[0].name, "Alpha Issuer");
    assert_eq!(doc.organizations[0].role, Role::IssuedBy);
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Llm));
    assert_eq!(doc.organizations[1].name, "Beta Counterparty");
    assert_eq!(doc.organizations[1].role, Role::Party);
    assert_eq!(doc.organizations[1].provenance, Some(Provenance::Llm));

    // 2. Subsequent classification replaces the old LLM list.
    let doc = classify_only(
        &r,
        json!({
            "organizations": [
                {"id": null, "new_name": "Delta Employer", "role": "employer", "confident": true},
            ]
        }),
        id,
    )
    .await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].name, "Delta Employer");
    assert_eq!(doc.organizations[0].role, Role::Employer);

    // 3. User sets manual organization; subsequent LLM write must NOT overwrite manual rows.
    let ctx = r.app.ctx(Actor::system());
    let manual_org = taxonomy::create_correspondent(
        &ctx,
        NewCorrespondent {
            name: "Manual Firm".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    documents::update_document(
        &ctx,
        id,
        DocumentPatch {
            organizations: Some(vec![OrganizationLink {
                id: manual_org.id,
                role: Role::ReceivedFrom,
            }]),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let doc = classify_only(
        &r,
        json!({
            "organizations": [
                {"id": null, "new_name": "Should Be Ignored", "role": "issued_by", "confident": true},
            ]
        }),
        id,
    )
    .await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].id, manual_org.id);
    assert_eq!(doc.organizations[0].name, "Manual Firm");
    assert_eq!(doc.organizations[0].role, Role::ReceivedFrom);
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Manual));
}

#[tokio::test]
async fn an_unsure_model_keeps_the_local_organization_match() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let ctx = r.app.ctx(Actor::system());
    let local_org = taxonomy::create_correspondent(
        &ctx,
        NewCorrespondent {
            name: "Пример-Инвест".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    // The pipeline's own local matching finds it in the contract text.
    // A model that is not sure of anything leaves the local match alone.
    let doc = classify_only(
        &r,
        json!({
            "organizations": [
                {"id": null, "new_name": "Guess", "role": "issued_by", "confident": false},
            ]
        }),
        id,
    )
    .await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].id, local_org.id);
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Local));

    // A confident answer replaces it.
    let doc = classify_only(
        &r,
        json!({
            "organizations": [
                {"id": null, "new_name": "Sure Issuer", "role": "issued_by", "confident": true},
            ]
        }),
        id,
    )
    .await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].name, "Sure Issuer");
    assert_eq!(doc.organizations[0].provenance, Some(Provenance::Llm));
}

#[tokio::test]
async fn enumerable_fields_and_places_are_normalized_after_the_model() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[
                ("sex", json!("МУЖ.")),
                ("birth_place", json!("Place of birth: SPRINGFIELD")),
                ("birth_date", json!("15.04.2012")),
                (
                    "issuing_authority",
                    json!("Autoritatea emitentă: Acme Registry"),
                ),
                ("surname", json!("TESTOV")),
            ],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(
        field(&doc, "sex").unwrap().value,
        FieldValue::Text("M".into())
    );
    assert_eq!(
        field(&doc, "birth_place").unwrap().value,
        FieldValue::Text("Springfield".into())
    );
    assert_eq!(
        field(&doc, "birth_date").unwrap().value,
        FieldValue::Date(chrono::NaiveDate::from_ymd_opt(2012, 4, 15).unwrap())
    );
    assert_eq!(
        field(&doc, "issuing_authority").unwrap().value,
        FieldValue::Text("Acme Registry".into())
    );
    assert_eq!(
        field(&doc, "surname").unwrap().value,
        FieldValue::Text("TESTOV".into())
    );
}

#[tokio::test]
async fn the_document_date_is_read_in_the_common_day_first_form_too() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(&r, json!({"document_date": "11.07.2024"}), id).await;
    assert_eq!(doc.document_date.unwrap().to_string(), "2024-07-11");
}

// --- grounding the model's answers in the document ------------------------------------

#[tokio::test]
async fn a_picked_correspondent_the_document_does_not_back_falls_back_to_the_printed_name() {
    let r = rig(&[]).await;
    let other_office = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Rental Office 12345".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({"organizations": [{
            "id": other_office.id.to_string(),
            "new_name": "Тествилль Аренда",
            "role": "issued_by",
            "confident": true,
        }]}),
        id,
    )
    .await;

    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_ne!(doc.organizations[0].id, other_office.id);
    assert_eq!(doc.organizations[0].name, "Тествилль Аренда");
}

async fn correspondent_count(app: &App) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM correspondents")
        .fetch_one(&app.db.read)
        .await
        .unwrap()
}

/// One organization written three ways: stored and answered with spaces just
/// inside the quotes or without them, it is one correspondent.
#[tokio::test]
async fn a_name_that_differs_only_in_quote_spacing_is_not_a_second_correspondent() {
    const CLEAN: &str = "ООО \"ТЕСТОВАЯ ЛАБОРАТОРИЯ\"";
    const SPACED: &str = "ООО \" ТЕСТОВАЯ ЛАБОРАТОРИЯ\"";
    let answer = |name: &str| {
        json!({"organizations": [{
            "id": null, "new_name": name, "role": "issued_by", "confident": true,
        }]})
    };
    // An old row with the stray space, a new answer without it, and the other
    // way round.
    for (stored, printed) in [(SPACED, CLEAN), (CLEAN, SPACED)] {
        let r = rig(&[]).await;
        let known = taxonomy::create_correspondent(
            &r.app.ctx(Actor::system()),
            NewCorrespondent {
                name: stored.into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
        let doc = classify_only(&r, answer(printed), id).await;

        assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
        assert_eq!(doc.organizations[0].id, known.id, "{printed}");
        assert_eq!(correspondent_count(&r.app).await, 1, "no duplicate");
    }
}

#[tokio::test]
async fn a_new_organization_is_stored_with_a_tidy_name() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({"organizations": [{
            "id": null,
            "new_name": "ООО \"  ТЕСТОВАЯ ЛАБОРАТОРИЯ \"",
            "role": "issued_by",
            "confident": true,
        }]}),
        id,
    )
    .await;
    assert_eq!(doc.organizations.len(), 1);
    assert_eq!(doc.organizations[0].name, "ООО \"ТЕСТОВАЯ ЛАБОРАТОРИЯ\"");
}

/// The Russian fixture prints «Пример-Инвест» and none of the Latin names
/// below, as a correspondent stored in English against a Russian document.
#[tokio::test]
async fn an_alias_in_the_documents_language_backs_the_correspondent_the_model_picked() {
    let r = rig(&[]).await;
    let stored = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Hearthstone Lettings".into(),
            aliases: vec!["Пример-Инвест".into()],
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({"organizations": [{
            "id": stored.id.to_string(),
            "new_name": null,
            "role": "issued_by",
            "confident": true,
        }]}),
        id,
    )
    .await;

    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(doc.organizations[0].id, stored.id);
    assert_eq!(correspondent_count(&r.app).await, 1, "no duplicate");
}

#[tokio::test]
async fn the_name_the_model_read_backs_a_stored_correspondent_the_text_lacks() {
    let r = rig(&[]).await;
    let stored = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Hearthstone Lettings".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({"organizations": [{
            "id": stored.id.to_string(),
            "new_name": "Hearthstone Lettings Ltd",
            "role": "issued_by",
            "confident": true,
        }]}),
        id,
    )
    .await;

    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(doc.organizations[0].id, stored.id);
    assert_eq!(correspondent_count(&r.app).await, 1, "no duplicate");
}

#[tokio::test]
async fn a_summary_sentence_with_a_year_the_document_lacks_is_dropped() {
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    let doc = classify_only(
        &r,
        json!({"summary": "A lease signed by a tenant born in 1994. It was signed in 2026."}),
        id,
    )
    .await;

    assert_eq!(doc.summary.as_deref(), Some("It was signed in 2026."));
}

#[tokio::test]
async fn the_document_number_does_not_become_the_personal_number() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "passport", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[
                ("series", json!("45 09")),
                ("number", json!("123456")),
                // Series digits, number and the MRZ check digit run together.
                ("personal_number", json!("4501234568")),
            ],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert!(field(&doc, "number").is_some());
    assert!(field(&doc, "personal_number").is_none(), "{:?}", doc.fields);
    assert!(
        !doc.attention.contains(&"checksum_failed".to_string()),
        "{:?}",
        doc.attention
    );
}

#[tokio::test]
async fn facts_get_a_stable_key_and_a_label_for_people() {
    let r = rig(&[]).await;
    r.mock.route("classification", classify_reply(json!({})));
    let mut reply = fields_json(&r.app, "contract", &[]).await;
    reply["facts"] = json!([{
        "key": "registration_removed_2024",
        "label": "Снят с регистрационного учёта",
        "value": "23.07.2024",
        "page": 1,
    }]);
    r.mock.route("field_extraction", MockReply::ok(&reply));
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.facts.len(), 1);
    assert_eq!(doc.facts[0].key, "registration_removed");
    assert_eq!(
        doc.facts[0].label.as_deref(),
        Some("Снят с регистрационного учёта")
    );
    // The fields request asks for labels in the uploader's language.
    let sent = r.mock.requests();
    let fields_request = sent
        .iter()
        .find(|q| q.schema_name() == Some("field_extraction"))
        .unwrap();
    assert!(
        fields_request.body["messages"][1]["content"]
            .to_string()
            .contains("Output language: English"),
    );
}

#[tokio::test]
async fn the_issuer_field_links_the_issuing_organization_when_classification_found_none() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "birth_certificate", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply_naming(
            &r.app,
            "birth_certificate",
            &[(
                "registry_office",
                json!("Civil Registry Office of Testville"),
            )],
            &["registry_office"],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(
        doc.organizations[0].name,
        "Civil Registry Office of Testville"
    );
    assert_eq!(doc.organizations[0].role, Role::IssuedBy);
}

#[tokio::test]
async fn an_issuer_field_that_names_only_a_place_links_nothing() {
    // The model says the value is a place: one word or several, no link.
    for place in ["TESTVILLE", "Testville District, Northern Region"] {
        let r = rig(&[]).await;
        r.mock.route(
            "classification",
            classify_reply(json!({"type_key": "passport", "organizations": []})),
        );
        r.mock.route(
            "field_extraction",
            fields_reply_naming(
                &r.app,
                "passport",
                &[("issuing_authority", json!(place))],
                &[],
            )
            .await,
        );
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

        process_all(&r.app, id).await;

        let doc = view(&r.app, id).await;
        assert!(
            doc.organizations.is_empty(),
            "{place}: {:?}",
            doc.organizations
        );
        assert_eq!(
            field(&doc, "issuing_authority").unwrap().value,
            FieldValue::Text(place.into()),
            "the field itself is still stored"
        );
    }
}

#[tokio::test]
async fn an_issuer_field_naming_a_known_organization_links_it_whatever_the_model_says() {
    // The model's names_organization varies between runs on the same issuer;
    // a name the directory already holds is an organization either way.
    let r = rig(&[]).await;
    let known = taxonomy::create_correspondent(
        &r.app.ctx(Actor::system()),
        NewCorrespondent {
            name: "Main Office of Testing".into(),
            aliases: vec![],
        },
    )
    .await
    .unwrap();
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "passport", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply_naming(
            &r.app,
            "passport",
            &[("issuing_authority", json!("MAIN OFFICE OF TESTING"))],
            &[],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(doc.organizations[0].id, known.id);
    assert_eq!(doc.organizations[0].role, Role::IssuedBy);
}

#[tokio::test]
async fn a_one_word_company_in_an_issuer_field_is_linked() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "invoice", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply_naming(
            &r.app,
            "invoice",
            &[("issuer", json!("Moldcell"))],
            &["issuer"],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(doc.organizations[0].name, "Moldcell");
    assert_eq!(doc.organizations[0].role, Role::IssuedBy);
}

#[tokio::test]
async fn an_issuer_field_of_a_user_type_works_through_its_flag_not_its_key() {
    use paperjam::model::fields::{FieldDef, FieldKind, Labels};
    use paperjam::model::taxonomy::NewType;

    let r = rig(&[]).await;
    let field_def = |key: &str, issuer: bool| FieldDef {
        key: key.into(),
        labels: Labels::from([("en".to_string(), key.to_string())]),
        kind: FieldKind::Text,
        validator: None,
        required: false,
        issuer,
    };
    taxonomy::create_type(
        &r.app.ctx(Actor::system()),
        NewType {
            key: "pet_licence".into(),
            labels: Labels::from([("en".to_string(), "Pet licence".to_string())]),
            // `issuer` is a key the old fixed list knew; here it is no issuer.
            fields: vec![field_def("issuer", false), field_def("awarding_body", true)],
        },
    )
    .await
    .unwrap();
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "pet_licence", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        MockReply::ok(&json!({
            "fields": {
                "issuer": {"value": "Wrong Key Council", "page": 1},
                "awarding_body": {"value": "Kennel Club", "page": 1, "names_organization": true},
            },
            "facts": [],
        })),
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.organizations.len(), 1, "{:?}", doc.organizations);
    assert_eq!(doc.organizations[0].name, "Kennel Club");
}

#[tokio::test]
async fn a_verified_zone_in_the_transcription_sets_the_type_and_values() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    // The ICAO specimen as a model copies it: the names line runs long.
    r.mock.route(
        "transcription",
        transcription_reply(
            "# PASSPORT\n\nPPUTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<<<<<<<<<\n\
             L898902C36UTO7408122F1204159ZE184226B<<<<<10",
        ),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "other", "organizations": []})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "travel_passport", &[("number", json!("L898902C3"))]).await,
    );
    let id = upload_fixture(&r.app, "scan_ru.png", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.type_key.as_deref(), Some("travel_passport"));
    assert_eq!(doc.type_provenance, Some(Provenance::Mrz));
    let number = field(&doc, "number").expect("number");
    assert_eq!(number.provenance, Provenance::Mrz);
    assert_eq!(number.value, FieldValue::Identifier("L898902C3".into()));
    assert_eq!(field(&doc, "surname").unwrap().provenance, Provenance::Mrz);
}

/// A Russian internal passport of the made-up ЯТЕСТОВА АННА ПЕТРОВНА whose
/// names line reads `names`; the second line verifies.
async fn ru_passport_with_names_line(names: &str) -> (Rig, DocumentId) {
    let r = rig(&[]).await;
    let line1 = format!("{names}{}", "<".repeat(44 - names.len()));
    r.mock.route(
        "transcription",
        transcription_reply(&format!(
            "# ПАСПОРТ\n\n{line1}\n4501234568RUS9001158M<<<<<<<9100201770001<42"
        )),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[
                ("surname", json!("ЯТЕСТОВА")),
                ("given_names", json!("АННА ПЕТРОВНА")),
            ],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "scan_ru.png", false).await;
    process_all(&r.app, id).await;
    (r, id)
}

async fn people_count(app: &App) -> usize {
    taxonomy::list_taxonomy(&app.ctx(Actor::system()))
        .await
        .unwrap()
        .people
        .len()
}

#[tokio::test]
async fn a_garbled_zone_surname_gives_way_to_the_printed_one_and_creates_nobody() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    // `8` (Я) read as `B`, a stray K: the zone says БТЕСТОВАК.
    let (r, id) = ru_passport_with_names_line("PNRUSBTESTOVAK<<ANNA<PETROVNA").await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.type_key.as_deref(), Some("passport"));
    let surname = field(&doc, "surname").unwrap();
    assert_eq!(surname.value, FieldValue::Text("ЯТЕСТОВА".into()));
    assert_eq!(surname.provenance, Provenance::Llm);
    // The given names agree, so the zone's stay.
    assert_eq!(
        field(&doc, "given_names").unwrap().provenance,
        Provenance::Mrz
    );
    // The digits of the zone still win.
    assert_eq!(field(&doc, "number").unwrap().provenance, Provenance::Mrz);
    assert!(
        doc.attention.contains(&"mrz_mismatch".to_string()),
        "{:?}",
        doc.attention
    );
    // A contested holder name is for the user to confirm.
    assert!(doc.people.is_empty(), "{:?}", doc.people);
    assert_eq!(people_count(&r.app).await, 0);
}

#[tokio::test]
async fn a_zone_surname_with_an_ocr_confusion_is_replaced_quietly() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let (r, id) = ru_passport_with_names_line("PNRUSBTESTOVA<<ANNA<PETROVNA").await;

    let doc = view(&r.app, id).await;
    let surname = field(&doc, "surname").unwrap();
    assert_eq!(surname.value, FieldValue::Text("ЯТЕСТОВА".into()));
    assert_eq!(surname.provenance, Provenance::Llm);
    assert!(
        !doc.attention.contains(&"mrz_mismatch".to_string()),
        "{:?}",
        doc.attention
    );
    assert_eq!(doc.people.len(), 1, "{:?}", doc.people);
    assert!(
        doc.people[0].name.starts_with("Ятестова") || doc.people[0].name.ends_with("Yatestova"),
        "{}",
        doc.people[0].name
    );
}

#[tokio::test]
async fn new_tags_are_normalized_and_never_repeat_what_the_archive_names() {
    // (what the model proposes, the tags the document ends up with)
    let cases: [([&str; 2], &[&str]); 2] = [
        // An existing tag in other letter case and with a stray space is
        // reused, not duplicated; a `#` and a full stop are not part of a tag.
        (["Здоровье ", "#Работа."], &["здоровье", "работа"]),
        // The document type and a person are no topics.
        (["contract", "Ivan Testov"], &[]),
    ];
    for (proposed, expected) in cases {
        let r = rig(&[]).await;
        let ctx = r.app.ctx(Actor::system());
        taxonomy::create_tag(&ctx, "здоровье", None).await.unwrap();
        taxonomy::create_person(
            &ctx,
            NewPerson {
                name: "Ivan Testov".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        r.mock.route(
            "classification",
            classify_reply(json!({"tags": [], "people": [], "new_tags": proposed})),
        );
        r.mock.route(
            "field_extraction",
            fields_reply(&r.app, "contract", &[]).await,
        );
        let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

        process_all(&r.app, id).await;

        let doc = view(&r.app, id).await;
        assert!(doc.attention.is_empty(), "{:?}", doc.attention);
        let mut on_document: Vec<_> = doc.tags.iter().map(|t| t.name.as_str()).collect();
        on_document.sort_unstable();
        assert_eq!(on_document, expected, "{proposed:?}");
        let all: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&r.app.db.read)
            .await
            .unwrap();
        // The one tag that existed plus the new ones; never a second
        // "здоровье".
        assert_eq!(
            all,
            1 + i64::from(expected.contains(&"работа")),
            "{proposed:?}"
        );
    }
}

#[tokio::test]
async fn a_passport_holder_the_model_read_becomes_a_person() {
    let r = rig(&[]).await;
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[("surname", json!("TESTOV")), ("given_names", json!("IVAN"))],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;

    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.people.len(), 1, "{:?}", doc.people);
    assert_eq!(doc.people[0].name, "Ivan Testov");
}

#[tokio::test]
async fn a_verified_zone_in_the_transcription_replaces_a_garbled_local_zone() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "passport_td3.png", false).await;
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            llm: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // Simulate local OCR verifying the check digits on line 2 while garbling
    // the names line (`8` -> `B`, stray `K` from filler) into `mrz` provenance.
    sqlx::query(
        "UPDATE field_values SET value = ? WHERE document_id = ? AND field_key = 'surname'",
    )
    .bind(json!("БТЕСТОВАК").to_string())
    .bind(id)
    .execute(&r.app.db.write)
    .await
    .unwrap();

    r.mock.route(
        "transcription",
        transcription_reply(
            "# ПАСПОРТ\n\nPNRUS8TESTOVA<<ANNA<PETROVNA<<<<<<<<<<<<<<\n\
             4501234568RUS9001158M<<<<<<<9100201770001<42",
        ),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "travel_passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[
                ("surname", json!("ЯТЕСТОВА")),
                ("given_names", json!("АННА ПЕТРОВНА")),
            ],
        )
        .await,
    );
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            local: false,
            llm: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let doc = view(&r.app, id).await;
    assert_eq!(doc.type_key.as_deref(), Some("passport"));
    assert_eq!(doc.type_provenance, Some(Provenance::Mrz));
    let surname = field(&doc, "surname").unwrap();
    assert_eq!(surname.value, FieldValue::Text("ЯТЕСТОВА".into()));
    assert_eq!(surname.provenance, Provenance::Mrz);
    let given = field(&doc, "given_names").unwrap();
    assert_eq!(given.value, FieldValue::Text("АННА ПЕТРОВНА".into()));
    assert_eq!(given.provenance, Provenance::Mrz);
    assert_eq!(
        field(&doc, "number").unwrap().value,
        FieldValue::Identifier("123456".into())
    );
    assert!(field(&doc, "expiry_date").is_none());
    assert!(field(&doc, "personal_number").is_none());
    assert!(
        !doc.attention.contains(&"mrz_mismatch".to_string()),
        "{:?}",
        doc.attention
    );
    assert_eq!(doc.people.len(), 1, "{:?}", doc.people);
}

#[tokio::test]
async fn an_unverified_transcription_zone_keeps_the_local_zone_and_manual_fields() {
    if !have_tesseract(&["eng", "rus", "ron", "osd"]) {
        return;
    }
    let r = rig(&[]).await;
    let id = upload_fixture(&r.app, "passport_td3.png", false).await;
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            llm: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A zone with a bad check digit in the transcription must not touch the
    // verified local MRZ reading.
    r.mock.route(
        "transcription",
        transcription_reply(
            "# PASSPORT\n\nP<UTOWRONG<<PERSON<<<<<<<<<<<<<<<<<<<<<<<<<<\n\
             TS12345679UTO8503125M32090142001234567890<20",
        ),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "travel_passport"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "travel_passport",
            &[("surname", json!("TESTOV")), ("given_names", json!("IVAN"))],
        )
        .await,
    );
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            local: false,
            llm: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let doc = view(&r.app, id).await;
    assert_eq!(doc.type_key.as_deref(), Some("travel_passport"));
    let surname = field(&doc, "surname").unwrap();
    assert_eq!(surname.value, FieldValue::Text("TESTOV".into()));
    assert_eq!(surname.provenance, Provenance::Mrz);

    // A manual edit still survives when a valid transcription zone arrives.
    documents::update_document(
        &r.app.ctx(Actor::system()),
        id,
        DocumentPatch {
            fields: [(
                "surname".to_string(),
                Some(FieldValue::Text("CUSTOM".into())),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    r.mock.route(
        "transcription",
        transcription_reply(
            "# ПАСПОРТ\n\nPNRUS8TESTOVA<<ANNA<PETROVNA<<<<<<<<<<<<<<\n\
             4501234568RUS9001158M<<<<<<<9100201770001<42",
        ),
    );
    pipeline::process_document(
        &r.app,
        id,
        ProcessOptions {
            local: false,
            llm: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let doc = view(&r.app, id).await;
    let surname = field(&doc, "surname").unwrap();
    assert_eq!(surname.value, FieldValue::Text("CUSTOM".into()));
    assert_eq!(surname.provenance, Provenance::Manual);
    assert_eq!(
        field(&doc, "number").unwrap().value,
        FieldValue::Identifier("123456".into())
    );
}

#[tokio::test]
async fn openrouter_skips_reasoning_on_transcribe_and_fields_by_default() {
    let r = rig(&[("PAPERJAM_LLM_PROVIDER", "openrouter")]).await;
    r.mock.route(
        "transcription",
        transcription_reply("# Invoice\n\nTotal 100.00 EUR"),
    );
    r.mock.route(
        "classification",
        classify_reply(json!({"type_key": "invoice", "title": "Acme · 100 EUR"})),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(&r.app, "invoice", &[("number", json!("INV-1"))]).await,
    );
    let id = upload_fixture(&r.app, "invoice_scan_en.pdf", false).await;
    process_all(&r.app, id).await;

    let requests = r.mock.requests();
    let step_effort = |schema_name: &str| -> String {
        requests
            .iter()
            .find(|req| {
                req.body["response_format"]["json_schema"]["name"].as_str() == Some(schema_name)
            })
            .and_then(|req| req.body["reasoning"]["effort"].as_str())
            .expect(schema_name)
            .to_string()
    };
    assert_eq!(step_effort("transcription"), "none");
    assert_eq!(step_effort("classification"), "low");
    assert_eq!(step_effort("field_extraction"), "none");

    // An explicit config override applies to every step.
    let overridden = rig(&[
        ("PAPERJAM_LLM_PROVIDER", "openrouter"),
        ("PAPERJAM_LLM_REASONING_EFFORT", "medium"),
    ])
    .await;
    overridden.mock.route(
        "classification",
        classify_reply(json!({"type_key": "contract", "title": "Lease"})),
    );
    overridden.mock.route(
        "field_extraction",
        fields_reply(&overridden.app, "contract", &[]).await,
    );
    let id2 = upload_fixture(&overridden.app, "contract_ru_digital.pdf", false).await;
    process_all(&overridden.app, id2).await;
    for req in overridden.mock.requests() {
        assert_eq!(req.body["reasoning"]["effort"], "medium");
    }
}

#[tokio::test]
async fn mismatched_llm_person_and_title_are_reconciled_with_extracted_holder() {
    let r = rig(&[]).await;
    let relative = taxonomy::create_person(
        &r.app.ctx(Actor::system()),
        NewPerson {
            name: "Тестов Борис Петрович".into(),
            aliases: vec![],
            notes: String::new(),
        },
    )
    .await
    .unwrap();
    r.mock.route(
        "classification",
        classify_reply(json!({
            "type_key": "passport",
            "title": "Тестов Борис Петрович, 2030",
            "people": [relative.id.to_string()],
            "organizations": [],
        })),
    );
    r.mock.route(
        "field_extraction",
        fields_reply(
            &r.app,
            "passport",
            &[
                ("surname", json!("ТЕСТОВ")),
                ("given_names", json!("ПЁТР НИКОЛАЕВИЧ")),
                ("issue_date", json!("2020-05-01")),
            ],
        )
        .await,
    );
    let id = upload_fixture(&r.app, "contract_ru_digital.pdf", false).await;
    process_all(&r.app, id).await;

    let doc = view(&r.app, id).await;
    assert_eq!(doc.people.len(), 1, "{:?}", doc.people);
    assert_ne!(doc.people[0].id, relative.id);
    assert!(
        doc.people[0].name.contains("Пётр"),
        "{}",
        doc.people[0].name
    );
    assert_eq!(doc.title.as_deref(), Some("Тестов Пётр Николаевич, 2020"));
}
