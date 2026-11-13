//! Search acceptance tests: an in-process index built from literal
//! `IndexDoc`s (spec §7.8, §8).

use std::collections::BTreeMap;

use chrono::{NaiveDate, TimeZone, Utc};
use paperjam::ids::{CorrespondentId, DocumentId, PersonId};
use paperjam::model::fields::FieldValue;
use paperjam::search::{Filters, IndexDoc, SearchIndex, SearchRequest, Sort};
use rust_decimal::Decimal;

fn doc(title: &str, text: &str) -> IndexDoc {
    IndexDoc {
        id: DocumentId::new(),
        title: title.to_string(),
        texts: vec![text.to_string()],
        other_texts: Vec::new(),
        summary: None,
        type_key: None,
        correspondent_ids: Vec::new(),
        correspondent: Vec::new(),
        organization_names: vec![],
        people: Vec::new(),
        tags: Vec::new(),
        fields: BTreeMap::new(),
        field_order: Vec::new(),
        summary_fields: Vec::new(),
        type_names: Vec::new(),
        document_date: None,
        added_at: Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
        local_only: false,
        attention: Vec::new(),
        trashed: false,
        has_pages: true,
    }
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn index_of(docs: &[&IndexDoc]) -> SearchIndex {
    let index = SearchIndex::open_in_ram();
    for d in docs {
        index.index_document(d).unwrap();
    }
    index.commit().unwrap();
    index
}

fn ids(index: &SearchIndex, query: &str) -> Vec<DocumentId> {
    let page = index
        .search(&SearchRequest {
            query: Some(query.to_string()),
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("query {query:?}: {e}"));
    page.hits.iter().map(|h| h.id).collect()
}

#[test]
fn russian_inflection() {
    let d = doc("Аренда", "Условия договора аренды квартиры на 12 месяцев");
    let index = index_of(&[&d]);
    assert_eq!(ids(&index, "договор"), vec![d.id]);
    assert_eq!(ids(&index, "договорами"), vec![d.id]);
}

#[test]
fn english_plural() {
    let d = doc("Deals", "Both contracts were signed in May");
    let index = index_of(&[&d]);
    assert_eq!(ids(&index, "contract"), vec![d.id]);
}

#[test]
fn romanian_article() {
    let d = doc("Chirie", "Contractul de închiriere a fost semnat");
    let index = index_of(&[&d]);
    assert_eq!(ids(&index, "contract"), vec![d.id]);
    assert_eq!(ids(&index, "inchiriere"), vec![d.id]);
}

#[test]
fn identifier_exact() {
    let mut passport = doc("Passport", "REPUBLIC OF TESTLAND");
    passport.type_key = Some("passport".into());
    passport
        .fields
        .insert("number".into(), FieldValue::Identifier("AB1234567".into()));
    passport.field_order = vec!["number".into()];
    let mut other = doc("Other passport", "REPUBLIC OF TESTLAND");
    other
        .fields
        .insert("number".into(), FieldValue::Identifier("AB7654321".into()));
    let index = index_of(&[&passport, &other]);

    assert_eq!(ids(&index, "AB1234567"), vec![passport.id]);
    assert_eq!(ids(&index, "ab1234567"), vec![passport.id]);
    assert_eq!(ids(&index, "field.number:AB1234567"), vec![passport.id]);
    // an identifier is not matched by its prefix
    assert!(ids(&index, "AB123").is_empty());
}

#[test]
fn person_alias_filter() {
    let person = PersonId::new();
    let mut d = doc("Insurance", "Policy number 55");
    d.people = vec![(
        person,
        vec!["Марик Тестов".into(), "Marik".into(), "Maric".into()],
    )];
    let unrelated = doc("Receipt", "Bread and milk");
    let index = index_of(&[&d, &unrelated]);

    assert_eq!(ids(&index, &format!("person:{person}")), vec![d.id]);
    assert_eq!(ids(&index, "Maric"), vec![d.id]);
    assert_eq!(ids(&index, "person:марик"), vec![d.id]);
    assert_eq!(ids(&index, "person:maric"), vec![d.id]);

    let by_filter = index
        .search(&SearchRequest {
            filters: Filters {
                people: vec![person],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_filter.total, 1);
    assert_eq!(by_filter.hits[0].people, vec!["Марик Тестов".to_string()]);
}

#[test]
fn field_date_filter() {
    let mut soon = doc("Passport A", "text");
    soon.fields
        .insert("expiry_date".into(), FieldValue::Date(date(2026, 5, 1)));
    let mut later = doc("Passport B", "text");
    later
        .fields
        .insert("expiry_date".into(), FieldValue::Date(date(2028, 1, 1)));
    let index = index_of(&[&soon, &later]);

    assert_eq!(ids(&index, "field.expiry_date<2027-01-01"), vec![soon.id]);
    assert_eq!(ids(&index, "field.expiry_date>=2027-01"), vec![later.id]);
    assert_eq!(ids(&index, "field.expiry_date:2028-01"), vec![later.id]);
    assert_eq!(ids(&index, "has:field.expiry_date").len(), 2);
}

#[test]
fn field_number_and_money_filters() {
    let mut big = doc("Big", "invoice");
    big.fields.insert(
        "total".into(),
        FieldValue::Money {
            amount: Decimal::new(750000, 2),
            currency: "EUR".into(),
        },
    );
    let mut small = doc("Small", "invoice");
    small.fields.insert(
        "total".into(),
        FieldValue::Money {
            amount: Decimal::new(4999, 2),
            currency: "EUR".into(),
        },
    );
    let index = index_of(&[&big, &small]);
    assert_eq!(ids(&index, "field.total>5000"), vec![big.id]);
    assert_eq!(ids(&index, "field.total<=49.99"), vec![small.id]);
}

#[test]
fn digit_identifiers_match_as_text() {
    let mut passport = doc("Passport", "text");
    passport
        .fields
        .insert("number".into(), FieldValue::Identifier("123456".into()));
    passport
        .fields
        .insert("series".into(), FieldValue::Identifier("45 21".into()));
    let other = doc("Other", "text");
    let index = index_of(&[&passport, &other]);

    assert_eq!(ids(&index, "field.number=123456"), vec![passport.id]);
    assert_eq!(ids(&index, "field.number:123456"), vec![passport.id]);
    assert_eq!(ids(&index, "field.series=4521"), vec![passport.id]);
    assert_eq!(ids(&index, "field.series=\"45 21\""), vec![passport.id]);
    assert!(ids(&index, "field.number=123457").is_empty());
}

#[test]
fn trashed_hidden_by_default() {
    let mut gone = doc("Old lease", "lease text");
    gone.trashed = true;
    let kept = doc("New lease", "lease text");
    let index = index_of(&[&gone, &kept]);

    assert_eq!(ids(&index, "lease"), vec![kept.id]);
    let with_trash = index
        .search(&SearchRequest {
            query: Some("lease".into()),
            filters: Filters {
                include_trashed: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    assert_eq!(with_trash.total, 2);
}

#[test]
fn boolean_operators_and_phrases() {
    let a = doc("A", "red apple pie");
    let b = doc("B", "green apple tart");
    let c = doc("C", "red cherry pie");
    let index = index_of(&[&a, &b, &c]);

    let sorted = |mut v: Vec<DocumentId>| {
        v.sort();
        v
    };
    assert_eq!(sorted(ids(&index, "apple -green")), vec![a.id]);
    assert_eq!(
        sorted(ids(&index, "apple OR cherry")),
        sorted(vec![a.id, b.id, c.id])
    );
    assert_eq!(ids(&index, "\"apple pie\""), vec![a.id]);
    assert_eq!(
        sorted(ids(&index, "(red OR green) apple")),
        sorted(vec![a.id, b.id])
    );
    assert_eq!(sorted(ids(&index, "-apple")), vec![c.id]);
}

#[test]
fn metadata_filters() {
    let corr = CorrespondentId::new();
    let mut d = doc("Statement", "account activity");
    d.type_key = Some("bank_statement".into());
    d.correspondent_ids = vec![corr];
    d.correspondent = vec!["Moldova Agroindbank".into(), "MAIB".into()];
    d.organization_names = vec!["Moldova Agroindbank".into()];
    d.tags = vec!["Bank".into(), "2026".into()];
    d.attention = vec!["checksum_failed".into()];
    d.document_date = Some(date(2026, 3, 15));
    d.local_only = true;
    let plain = doc("Note", "account note");
    let index = index_of(&[&d, &plain]);

    for query in [
        "type:bank_statement",
        "from:maib",
        "from:\"moldova agroindbank\"",
        "tag:bank",
        "has:attention",
        "has:date",
        "date:2026-03",
        "date:>=2026-03-15",
        "date:<=2026",
        "local:yes",
        "has:type",
        "has:tags",
        "has:from",
    ] {
        assert_eq!(ids(&index, query), vec![d.id], "{query}");
    }
    for query in [
        "type:invoice",
        "tag:other",
        "date:<2026-03",
        "date:>2026-03-15",
        "local:no account",
    ] {
        let hits = ids(&index, query);
        assert!(!hits.contains(&d.id), "{query}");
    }
    assert_eq!(ids(&index, "-has:attention account"), vec![plain.id]);
    assert_eq!(ids(&index, "-has:date account"), vec![plain.id]);

    let filtered = index
        .search(&SearchRequest {
            filters: Filters {
                correspondents: vec![corr],
                has_attention: Some(true),
                local_only: Some(true),
                types: vec!["bank_statement".into()],
                tags: vec!["bank".into(), "2026".into()],
                date_from: Some(date(2026, 3, 1)),
                date_to: Some(date(2026, 3, 31)),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        filtered.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
        vec![d.id]
    );
    assert_eq!(
        filtered.hits[0].organizations,
        vec!["Moldova Agroindbank".to_string()]
    );
    assert_eq!(filtered.hits[0].type_key.as_deref(), Some("bank_statement"));
}

#[test]
fn multiple_organizations_searchable_and_filterable() {
    let org1 = CorrespondentId::new();
    let org2 = CorrespondentId::new();
    let mut d = doc("Contract", "service agreement between parties");
    d.correspondent_ids = vec![org1, org2];
    d.correspondent = vec![
        "Alpha Corp".into(),
        "AC".into(),
        "Beta Ltd".into(),
        "BL".into(),
    ];
    d.organization_names = vec!["Alpha Corp".into(), "Beta Ltd".into()];
    let index = index_of(&[&d]);

    // Both findable in search text (names and aliases)
    for query in [
        "alpha",
        "beta",
        "ac",
        "bl",
        "from:alpha",
        "from:beta",
        "from:ac",
        "from:bl",
    ] {
        assert_eq!(ids(&index, query), vec![d.id], "{query}");
    }

    // Both findable individually via correspondents filter
    for corr in [org1, org2] {
        let filtered = index
            .search(&SearchRequest {
                filters: Filters {
                    correspondents: vec![corr],
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered.hits.len(), 1);
        assert_eq!(filtered.hits[0].id, d.id);
        assert_eq!(
            filtered.hits[0].organizations,
            vec!["Alpha Corp".to_string(), "Beta Ltd".to_string()]
        );
    }
}

#[test]
fn delete_removes_document() {
    let d = doc("Temp", "ephemeral text");
    let index = index_of(&[&d]);
    assert_eq!(ids(&index, "ephemeral").len(), 1);
    index.delete(d.id).unwrap();
    index.commit().unwrap();
    assert!(ids(&index, "ephemeral").is_empty());
}

#[test]
fn reindexing_replaces_the_document() {
    let mut d = doc("Draft", "first version");
    let index = index_of(&[&d]);
    d.texts = vec!["second version".into()];
    index.index_document(&d).unwrap();
    index.commit().unwrap();
    assert!(ids(&index, "first").is_empty());
    assert_eq!(ids(&index, "second"), vec![d.id]);
}

#[test]
fn snippet_highlights_matches_and_escapes_html() {
    let d = doc(
        "Doc",
        "<script>alert(1)</script> The lease expires in June & renews",
    );
    let index = index_of(&[&d]);
    let page = index
        .search(&SearchRequest {
            query: Some("lease".into()),
            ..Default::default()
        })
        .unwrap();
    let snippet = &page.hits[0].snippet;
    assert!(snippet.contains("<mark>lease</mark>"), "{snippet}");
    assert!(!snippet.contains("<script>"), "{snippet}");
    assert!(
        snippet.contains("&amp;") || !snippet.contains(" & "),
        "{snippet}"
    );
}

#[test]
fn hit_without_text_query_gets_plain_snippet_and_key_fields() {
    let mut d = doc("Passport", "REPUBLIC OF TESTLAND passport of Ivan Testov");
    d.fields
        .insert("series".into(), FieldValue::Text("AB".into()));
    d.fields
        .insert("number".into(), FieldValue::Identifier("1234567".into()));
    for (i, key) in ["c", "d", "e", "f"].iter().enumerate() {
        d.fields
            .insert((*key).into(), FieldValue::Text(format!("v{i}")));
    }
    d.field_order = ["series", "number", "c", "d", "e", "f"]
        .map(String::from)
        .to_vec();
    let index = index_of(&[&d]);
    let page = index.search(&SearchRequest::default()).unwrap();
    let hit = &page.hits[0];
    assert!(
        hit.snippet.starts_with("REPUBLIC OF TESTLAND"),
        "{}",
        hit.snippet
    );
    // Without summary fields: the first four fields of the type, in the
    // type's order.
    let keys: Vec<&str> = hit.key_fields.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["series", "number", "c", "d"]);
}

#[test]
fn summary_fields_set_which_fields_a_hit_shows_and_in_what_order() {
    let mut d = doc("Passport", "REPUBLIC OF TESTLAND");
    d.fields
        .insert("number".into(), FieldValue::Identifier("AP1234567".into()));
    d.fields
        .insert("expiry_date".into(), FieldValue::Date(date(2031, 5, 1)));
    d.fields
        .insert("surname".into(), FieldValue::Text("Testov".into()));
    d.field_order = ["series", "number", "issue_date", "expiry_date", "surname"]
        .map(String::from)
        .to_vec();
    // `series` has no value and is skipped; `surname` is not a summary field.
    d.summary_fields = ["series", "expiry_date", "number"]
        .map(String::from)
        .to_vec();
    let index = index_of(&[&d]);

    let page = index.search(&SearchRequest::default()).unwrap();
    let keys: Vec<&str> = page.hits[0].key_fields.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["expiry_date", "number"]);

    // The order survives serialization, which is what the clients read.
    let json = serde_json::to_string(&page.hits[0]).unwrap();
    assert!(
        json.find("\"expiry_date\"").unwrap() < json.find("\"number\"").unwrap(),
        "{json}"
    );
}

#[test]
fn identifier_is_found_in_any_spelling() {
    let spaced = doc("Travel passport", "Passport No\nAP 1234567\nSurname TESTOV");
    let glued = doc("Passport", "Number AP7654321 issued in Testland");
    let series = doc(
        "Internal passport",
        "Series and number 45 21 123456, issued 2020",
    );
    let mut field = doc("Scan", "unrelated words");
    field
        .fields
        .insert("number".into(), FieldValue::Identifier("MD 9988776".into()));
    let index = index_of(&[&spaced, &glued, &series, &field]);

    for query in [
        "AP 1234567",
        "AP1234567",
        "ap-1234567",
        "\"AP 1234567\"",
        "1234567",
    ] {
        assert_eq!(ids(&index, query), vec![spaced.id], "{query}");
    }
    for query in ["AP 7654321", "AP7654321", "ap-7654321", "ap.7654321"] {
        assert_eq!(ids(&index, query), vec![glued.id], "{query}");
    }
    for query in [
        "4521123456",
        "45 21 123456",
        "4521 123456",
        "45 21123456",
        "45-21-123456",
        "\"4521 123456\"",
        "series 4521 123456",
    ] {
        assert_eq!(ids(&index, query), vec![series.id], "{query}");
    }
    // The identifier typed field, stored glued or spaced, is found both ways.
    for query in [
        "MD9988776",
        "MD 9988776",
        "field.number:MD9988776",
        "field.number:\"MD 9988776\"",
        "field.number:md-9988776",
    ] {
        assert_eq!(ids(&index, query), vec![field.id], "{query}");
    }
    // Only whole identifiers match: no prefixes, no other digits.
    for query in [
        "AP 123456",
        "4521123457",
        "4521 12345",
        "AP 12345678",
        "MD99887",
    ] {
        assert!(ids(&index, query).is_empty(), "{query}");
    }
    // Other words in the query still have to match.
    assert!(ids(&index, "lease 4521 123456").is_empty());
}

#[test]
fn identifier_spelling_does_not_break_negation() {
    let a = doc("A", "Number AP 1234567 and some words");
    let b = doc("B", "Number AP 7654321 and some words");
    let index = index_of(&[&a, &b]);
    assert_eq!(ids(&index, "words -AP1234567"), vec![b.id]);
    assert_eq!(ids(&index, "words -\"AP 7654321\""), vec![a.id]);
}

#[test]
fn type_names_in_every_language_find_the_document() {
    let mut internal = doc("Scan 1", "text of a scan");
    internal.type_key = Some("passport".into());
    internal.type_names = vec!["Passport".into(), "Паспорт".into()];
    let mut travel = doc("Scan 2", "text of a scan");
    travel.type_key = Some("travel_passport".into());
    travel.type_names = [
        "Travel passport",
        "Загранпаспорт",
        "паспорт",
        "passport",
        "заграничный паспорт",
    ]
    .map(String::from)
    .to_vec();
    let mut other = doc("Scan 3", "text of a scan");
    other.type_key = Some("receipt".into());
    other.type_names = vec!["Receipt".into(), "Чек".into()];
    let index = index_of(&[&internal, &travel, &other]);

    let sorted = |mut v: Vec<DocumentId>| {
        v.sort();
        v
    };
    let both = sorted(vec![internal.id, travel.id]);
    for query in ["паспорт", "паспорта", "passport", "Passports"] {
        assert_eq!(sorted(ids(&index, query)), both, "{query}");
    }
    for query in [
        "загранпаспорт",
        "загранпаспорта",
        "заграничный паспорт",
        "travel passport",
    ] {
        assert_eq!(ids(&index, query), vec![travel.id], "{query}");
    }
    assert_eq!(ids(&index, "чек"), vec![other.id]);
    assert_eq!(ids(&index, "receipt"), vec![other.id]);
    // the structured filter is unaffected
    assert_eq!(ids(&index, "type:travel_passport"), vec![travel.id]);
}

#[test]
fn snippets_and_highlights_carry_no_markdown() {
    let text = "# PASSPORT\n\n**Surname:** TESTOV\n\n---\n\n| Field | Value |\n|---|---|\n\
                | Number | AP 1234567 |\n\n- an *emphasised* word\n\
                See [the lease](http://example.test/x) and ![stamp](img.png) <br> end `code` ~~old~~";
    let d = doc("Doc", text);
    let index = index_of(&[&d]);

    let page = index
        .search(&SearchRequest {
            query: Some("testov".into()),
            ..Default::default()
        })
        .unwrap();
    let snippet = &page.hits[0].snippet;
    assert!(snippet.contains("<mark>TESTOV</mark>"), "{snippet}");
    for markup in [
        "**", "#", "---", "|", "](", "http", "*", "`", "~~", "&lt;br",
    ] {
        assert!(!snippet.contains(markup), "{markup:?} in {snippet}");
    }

    // Later in the text, past the first screen of markup.
    let later = index
        .search(&SearchRequest {
            query: Some("emphasised".into()),
            ..Default::default()
        })
        .unwrap();
    let snippet = &later.hits[0].snippet;
    assert!(snippet.contains("<mark>emphasised</mark>"), "{snippet}");
    assert!(!snippet.contains('*'), "{snippet}");

    // No query: the plain start of the text.
    let plain = index.search(&SearchRequest::default()).unwrap();
    let snippet = &plain.hits[0].snippet;
    assert!(snippet.starts_with("PASSPORT Surname: TESTOV"), "{snippet}");
    for markup in ["**", "#", "---", "|"] {
        assert!(!snippet.contains(markup), "{markup:?} in {snippet}");
    }
}

#[test]
fn diacritics_and_hyphenated_numbers_still_match() {
    let d = doc("Birth", "Born in Chișinău, tax id 112-233-445 95");
    let r = doc("Reverse", "Born in Chisinau");
    let index = index_of(&[&d, &r]);
    let mut found = ids(&index, "Chisinau");
    found.sort();
    let mut expected = vec![d.id, r.id];
    expected.sort();
    assert_eq!(found, expected);
    assert_eq!(ids(&index, "Chișinău").len(), 2);
    assert_eq!(ids(&index, "112-233-445"), vec![d.id]);
    assert_eq!(ids(&index, "11223344595"), vec![d.id]);
}

#[test]
fn sorting_by_date_puts_undated_last_and_pages_with_cursor() {
    let mut docs = Vec::new();
    for (title, d) in [
        ("d2024", Some(date(2024, 1, 1))),
        ("d2026", Some(date(2026, 1, 1))),
        ("none", None),
        ("d2025", Some(date(2025, 1, 1))),
    ] {
        let mut x = doc(title, "shared words");
        x.document_date = d;
        docs.push(x);
    }
    let refs: Vec<&IndexDoc> = docs.iter().collect();
    let index = index_of(&refs);

    let titles = |sort: Sort, limit: u32, cursor: Option<String>| {
        let page = index
            .search(&SearchRequest {
                query: Some("shared".into()),
                sort,
                limit: Some(limit),
                cursor,
                ..Default::default()
            })
            .unwrap();
        (
            page.hits
                .iter()
                .map(|h| h.title.clone())
                .collect::<Vec<_>>(),
            page.next_cursor,
            page.total,
        )
    };
    assert_eq!(
        titles(Sort::DateDesc, 10, None).0,
        vec!["d2026", "d2025", "d2024", "none"]
    );
    assert_eq!(
        titles(Sort::DateAsc, 10, None).0,
        vec!["d2024", "d2025", "d2026", "none"]
    );

    let (first, cursor, total) = titles(Sort::DateDesc, 3, None);
    assert_eq!(first, vec!["d2026", "d2025", "d2024"]);
    assert_eq!(total, 4);
    let (second, cursor, _) = titles(Sort::DateDesc, 3, cursor);
    assert_eq!(second, vec!["none"]);
    assert!(cursor.is_none());
}

#[test]
fn limit_is_clamped() {
    let docs: Vec<IndexDoc> = (0..60).map(|i| doc(&format!("n{i}"), "common")).collect();
    let refs: Vec<&IndexDoc> = docs.iter().collect();
    let index = index_of(&refs);
    let page = index
        .search(&SearchRequest {
            query: Some("common".into()),
            limit: Some(500),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.hits.len(), 50);
    assert_eq!(page.total, 60);
    let default = index
        .search(&SearchRequest {
            query: Some("common".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(default.hits.len(), 20);
}

#[test]
fn query_syntax_error_is_invalid() {
    let index = index_of(&[]);
    let err = index
        .search(&SearchRequest {
            query: Some("a (b".into()),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code, "query_syntax");
}

#[test]
fn empty_index_returns_empty_page() {
    let index = SearchIndex::open_in_ram();
    let page = index.search(&SearchRequest::default()).unwrap();
    assert_eq!(page.total, 0);
    assert!(page.hits.is_empty());
}

#[test]
fn configured_languages_get_their_own_stemmer() {
    let d = doc("Elternabend", "Einladung fuer die Kindern");
    let german = SearchIndex::open_in_ram_with("eng+deu");
    german.index_document(&d).unwrap();
    german.commit().unwrap();
    assert_eq!(ids(&german, "Kinder"), vec![d.id]);

    // Without German in the list the inflected form is not found.
    let english = SearchIndex::open_in_ram_with("eng");
    english.index_document(&d).unwrap();
    english.commit().unwrap();
    assert!(ids(&english, "Kinder").is_empty());
}

#[test]
fn index_persists_on_disk_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let d = doc("Persisted", "durable words");
    {
        let index = SearchIndex::open(dir.path(), "eng+rus+ron").unwrap();
        index.index_document(&d).unwrap();
        index.commit().unwrap();
    }
    let reopened = SearchIndex::open(dir.path(), "eng+rus+ron").unwrap();
    assert_eq!(ids(&reopened, "durable"), vec![d.id]);
    // A different language set is a different schema: the index starts empty
    // and the startup reindex refills it.
    let other = SearchIndex::open(dir.path(), "eng+deu").unwrap();
    assert_eq!(other.num_docs(), 0);
}
