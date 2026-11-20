//! Full-text search over tantivy. The index is derived data: it can be
//! rebuilt from SQLite at any time.

pub mod analyzers;
mod exec;
mod ident;
mod markup;
pub mod query;
pub mod schema;
pub mod sync;
pub mod type_names;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use indexmap::IndexMap;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tantivy::directory::MmapDirectory;
use tantivy::indexer::IndexWriterOptions;
use tantivy::schema::Schema;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{Result, ServiceError};
use crate::ids::{CorrespondentId, DocumentId, PersonId};
use crate::matching;
use crate::model::due;
use crate::model::fields::FieldValue;
use query::Op;
use schema::Fields;

/// Everything the index needs to know about one document.
#[derive(Debug, Clone)]
pub struct IndexDoc {
    pub id: DocumentId,
    pub title: String,
    /// Display text per page.
    pub texts: Vec<String>,
    /// Text layers that are searched but not displayed (the losing OCR or
    /// embedded layer of a page).
    pub other_texts: Vec<String>,
    pub summary: Option<String>,
    pub type_key: Option<String>,
    pub correspondent_ids: Vec<CorrespondentId>,
    /// Name first, then aliases.
    pub correspondent: Vec<String>,
    /// Organization names for the hit, in role order.
    pub organization_names: Vec<String>,
    /// Each person with name first, then aliases.
    pub people: Vec<(PersonId, Vec<String>)>,
    pub tags: Vec<String>,
    pub fields: BTreeMap<String, FieldValue>,
    /// Keys of the document type's fields in the type's order, so hits can
    /// show the first few.
    pub field_order: Vec<String>,
    /// Field keys the type wants shown on a hit, in display order. Empty:
    /// the first fields of `field_order` that have a value.
    pub summary_fields: Vec<String>,
    /// Every name the document's type goes by (labels in all languages and
    /// synonyms), searchable as ordinary words.
    pub type_names: Vec<String>,
    pub document_date: Option<NaiveDate>,
    pub added_at: DateTime<Utc>,
    pub local_only: bool,
    pub attention: Vec<String>,
    pub trashed: bool,
    /// Rendered pages and a thumbnail exist: the document did not fail and
    /// has at least one page image (a plain-text file has none).
    pub has_pages: bool,
}

/// Order of the hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Relevance,
    DateDesc,
    DateAsc,
}

/// `field.<key>` filter. `value` is read as a date (`YYYY-MM`,
/// `YYYY-MM-DD`; a bare year in a field that holds dates), a number, or text,
/// in that order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FieldFilter {
    /// The field's key, e.g. `expiry_date`.
    pub key: String,
    pub op: Op,
    /// A date (`YYYY-MM-DD`, `YYYY-MM`, or a bare year on a field that holds
    /// dates), a number or text.
    pub value: String,
}

/// Structured filters (MCP and REST callers should not have to compose
/// query strings). Types, people and correspondents match any of the given
/// values; tags must all be present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct Filters {
    /// Document type keys (any of them).
    pub types: Vec<String>,
    /// Person ids (any of them).
    #[schema(value_type = Vec<String>)]
    pub people: Vec<PersonId>,
    /// Organization ids (any of them).
    #[schema(value_type = Vec<String>)]
    pub correspondents: Vec<CorrespondentId>,
    /// Tag names (all of them).
    pub tags: Vec<String>,
    /// Earliest document date, `YYYY-MM-DD`.
    #[schema(value_type = Option<String>, format = Date)]
    pub date_from: Option<NaiveDate>,
    /// Latest document date, `YYYY-MM-DD`.
    #[schema(value_type = Option<String>, format = Date)]
    pub date_to: Option<NaiveDate>,
    /// Only documents that need (or do not need) attention.
    pub has_attention: Option<bool>,
    /// Only documents kept local-only (or not).
    pub local_only: Option<bool>,
    /// Comparisons on typed fields, all of which must hold.
    pub fields: Vec<FieldFilter>,
    /// Include documents in the trash.
    pub include_trashed: bool,
}

pub const DEFAULT_LIMIT: u32 = 20;
pub const MAX_LIMIT: u32 = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct SearchRequest {
    /// The query language: free text, `"exact phrase"`, `-negation`, `OR`,
    /// parentheses, `type:`, `person:`, `from:`, `tag:`, `has:`, `date:`,
    /// `added:`, `due:`, `local:` and `field.<key><op><value>`. `has:expiring`
    /// finds documents due within 270 days or already past due. Empty or missing: no
    /// text condition.
    pub query: Option<String>,
    /// Structured filters, combined with the query by AND.
    pub filters: Filters,
    /// Order of the hits; `relevance` when omitted.
    pub sort: Sort,
    /// Hits per page: at most 50, 20 when omitted.
    pub limit: Option<u32>,
    /// Opaque; pass back `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Hit {
    #[schema(value_type = String)]
    pub id: DocumentId,
    pub title: String,
    pub type_key: Option<String>,
    #[schema(value_type = Option<String>, format = Date)]
    pub document_date: Option<NaiveDate>,
    /// When the document has to be renewed or replaced: its expiry date, or
    /// for a Russian internal passport the birthday it is replaced at
    /// (20 or 45). Absent for documents without one.
    #[schema(value_type = Option<String>, format = Date)]
    pub due_date: Option<NaiveDate>,
    /// Names of the linked organizations, in role order.
    pub organizations: Vec<String>,
    pub people: Vec<String>,
    /// The type's summary fields that have a value, in the type's order
    /// (without summary fields: the first few fields of the type). A JSON
    /// object whose key order is the display order.
    #[schema(value_type = Object)]
    pub key_fields: IndexMap<String, FieldValue>,
    /// Plain text around the match with `<mark>…</mark>` highlights; every
    /// other `<`, `>` and `&` is escaped.
    pub snippet: String,
    /// The document is in the trash (only reachable with `include_trashed`).
    pub trashed: bool,
    /// The checks that failed on the document, e.g. `mrz_mismatch`; empty
    /// when it needs no attention.
    pub attention: Vec<String>,
    /// The document has page images and a thumbnail; false for a failed
    /// document and for a plain-text file, whose image routes answer 404.
    pub has_pages: bool,
}

/// One page of hits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// Pass back as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<String>,
    /// Number of matching documents.
    pub total: u64,
    /// True when no document matched every word of the query, and these are
    /// the ones that match some of them (the words were joined by OR).
    #[serde(default)]
    #[schema(required = true)]
    pub partial: bool,
}

/// Memory the writer may use before flushing a segment (tantivy's minimum).
const WRITER_HEAP_BYTES: usize = 15_000_000;
/// The committer wakes this often.
const COMMIT_INTERVAL: Duration = Duration::from_secs(2);
/// The writer (and its heap) is released after this many wake-ups without
/// any use of it.
const IDLE_TICKS_BEFORE_RELEASE: u32 = 30;
/// Merge threads of the writer. A household archive merges a few small
/// segments now and then; tantivy's default of four threads would sit idle.
const WRITER_MERGE_THREADS: usize = 1;
/// Fields shown on a hit.
const KEY_FIELD_COUNT: usize = 4;

pub(crate) fn internal(e: impl Display) -> ServiceError {
    tracing::error!(error = %e, "search index error");
    ServiceError::internal("search_failed", "the search index failed")
}

/// Days since 0001-01-01, the unit of every date in the index.
pub(crate) fn days(date: NaiveDate) -> i64 {
    i64::from(chrono::Datelike::num_days_from_ce(&date))
}

/// What a hit shows, kept in the index so hits never touch SQLite.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoredMeta {
    pub title: String,
    pub type_key: Option<String>,
    pub document_date: Option<NaiveDate>,
    /// When the document is due to be renewed or replaced. Missing in
    /// indexes written before it existed.
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
    #[serde(default)]
    pub organizations: Vec<String>,
    pub people: Vec<String>,
    pub key_fields: IndexMap<String, FieldValue>,
    /// Missing in indexes written before the flag existed: read as live.
    #[serde(default)]
    pub trashed: bool,
    /// The short description; shown as the snippet when nothing is searched
    /// for. Missing in indexes written before it was stored.
    #[serde(default)]
    pub summary: Option<String>,
    /// The checks that failed (`attention` codes). Missing in indexes
    /// written before it was stored.
    #[serde(default)]
    pub attention: Vec<String>,
    /// Missing in indexes written before it was stored: read as "has pages",
    /// which is what every hit assumed until then.
    #[serde(default)]
    pub has_pages: Option<bool>,
}

pub(crate) struct Inner {
    pub index: Index,
    pub schema: Schema,
    pub fields: Fields,
    pub reader: IndexReader,
    writer: Mutex<Option<IndexWriter>>,
    /// Whether `writer` holds a writer; readable without taking the lock
    /// (a commit holds it for a while).
    writer_alive: AtomicBool,
    /// Set by every use of the writer, cleared by the committer: how it
    /// tells that the writer was touched since its last tick, also by a
    /// caller that committed on its own (a search, a rebuild).
    used: AtomicBool,
    dirty: AtomicBool,
}

/// Cheap to clone; all clones share one index.
#[derive(Clone)]
pub struct SearchIndex {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for SearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchIndex")
    }
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::open_in_ram()
    }
}

fn open_directory(dir: &Path, schema: &Schema) -> tantivy::Result<Index> {
    Index::open_or_create(MmapDirectory::open(dir)?, schema.clone())
}

impl SearchIndex {
    /// Opens (or creates) the index in `dir`. An index written with a
    /// different schema is discarded: it is derived data, and the caller
    /// refills it by reindexing (compare [`SearchIndex::num_docs`] with the
    /// database).
    ///
    /// `ocr_langs` is the Tesseract language list (`eng+deu`); it picks the
    /// stemmed text fields, so changing it recreates the index too.
    pub fn open(dir: &Path, ocr_langs: &str) -> Result<Self> {
        // 8: `due_date` now also reads `end_date`, `warranty_end_date` and
        // `due_date` fields, so every stored hit's due date is recomputed.
        const SCHEMA_VERSION: u32 = 8;
        std::fs::create_dir_all(dir).map_err(internal)?;
        let tags = analyzers::stem_tags(ocr_langs);
        let version = format!("{SCHEMA_VERSION}:{}", tags.join(","));
        let version_path = dir.join("schema_version");
        let version_ok = std::fs::read_to_string(&version_path)
            .ok()
            .is_some_and(|s| s.trim() == version);
        let (schema, fields) = schema::build(&tags);
        let wipe = |dir: &Path| -> Result<()> {
            for entry in std::fs::read_dir(dir).map_err(internal)?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(path);
                } else {
                    let _ = std::fs::remove_file(path);
                }
            }
            Ok(())
        };
        let index = if !version_ok {
            tracing::warn!("search index version mismatch, recreating it");
            wipe(dir)?;
            let index = open_directory(dir, &schema).map_err(internal)?;
            let _ = std::fs::write(&version_path, &version);
            index
        } else {
            match open_directory(dir, &schema) {
                Ok(index) => index,
                Err(e) => {
                    tracing::warn!(error = %e, "search index unusable, recreating it");
                    wipe(dir)?;
                    let index = open_directory(dir, &schema).map_err(internal)?;
                    let _ = std::fs::write(&version_path, &version);
                    index
                }
            }
        };
        let _ = std::fs::write(&version_path, &version);
        Self::from_index(index, schema, fields, &tags)
    }

    /// An in-memory index with the stemmers of the default test languages
    /// (English, Russian, Romanian).
    pub fn open_in_ram() -> Self {
        Self::open_in_ram_with("eng+rus+ron")
    }

    pub fn open_in_ram_with(ocr_langs: &str) -> Self {
        let tags = analyzers::stem_tags(ocr_langs);
        let (schema, fields) = schema::build(&tags);
        let index = Index::create_in_ram(schema.clone());
        Self::from_index(index, schema, fields, &tags).expect("in-memory index")
    }

    fn from_index(
        index: Index,
        schema: Schema,
        fields: Fields,
        tags: &[&'static str],
    ) -> Result<Self> {
        analyzers::register(&index, tags);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(internal)?;
        Ok(Self {
            inner: Arc::new(Inner {
                index,
                schema,
                fields,
                reader,
                writer: Mutex::new(None),
                writer_alive: AtomicBool::new(false),
                used: AtomicBool::new(false),
                dirty: AtomicBool::new(false),
            }),
        })
    }

    pub fn num_docs(&self) -> u64 {
        self.inner.reader.searcher().num_docs()
    }

    fn with_writer<T>(&self, f: impl FnOnce(&mut IndexWriter) -> Result<T>) -> Result<T> {
        let mut guard: MutexGuard<'_, Option<IndexWriter>> = self
            .inner
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.inner.used.store(true, Ordering::Release);
        if guard.is_none() {
            let options = IndexWriterOptions::builder()
                .num_worker_threads(1)
                .memory_budget_per_thread(WRITER_HEAP_BYTES)
                .num_merge_threads(WRITER_MERGE_THREADS)
                .build();
            *guard = Some(
                self.inner
                    .index
                    .writer_with_options(options)
                    .map_err(internal)?,
            );
            self.inner.writer_alive.store(true, Ordering::Release);
        }
        let out = f(guard.as_mut().expect("writer was just created"))?;
        // Still under the lock: a release can never see the writer holding
        // changes that are not marked as pending.
        self.inner.dirty.store(true, Ordering::Release);
        Ok(out)
    }

    /// Whether the index currently holds a writer (and its threads and heap).
    pub(crate) fn has_writer(&self) -> bool {
        self.inner.writer_alive.load(Ordering::Acquire)
    }

    /// Adds or replaces the document. Searchable after the next commit.
    pub fn index_document(&self, doc: &IndexDoc) -> Result<()> {
        let json = to_json(doc, &self.inner.fields);
        let parsed =
            TantivyDocument::parse_json(&self.inner.schema, &json.to_string()).map_err(internal)?;
        let id_term = Term::from_field_text(self.inner.fields.id, &doc.id.to_string());
        self.with_writer(|w| {
            w.delete_term(id_term);
            w.add_document(parsed).map_err(internal)?;
            Ok(())
        })
    }

    pub fn delete(&self, id: DocumentId) -> Result<()> {
        let term = Term::from_field_text(self.inner.fields.id, &id.to_string());
        self.with_writer(|w| {
            w.delete_term(term);
            Ok(())
        })
    }

    /// Removes every document (used before a full rebuild).
    pub fn clear(&self) -> Result<()> {
        self.with_writer(|w| {
            w.delete_all_documents().map_err(internal)?;
            Ok(())
        })
    }

    /// Makes pending changes searchable, if there are any.
    pub fn commit_if_dirty(&self) -> Result<()> {
        if self.inner.dirty.load(Ordering::Acquire) {
            self.commit()
        } else {
            Ok(())
        }
    }

    /// Makes pending changes searchable.
    pub fn commit(&self) -> Result<()> {
        let mut guard = self
            .inner
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(writer) = guard.as_mut() {
            writer.commit().map_err(internal)?;
            self.inner.dirty.store(false, Ordering::Release);
        }
        drop(guard);
        self.inner.reader.reload().map_err(internal)
    }

    /// Drops the writer and its memory once everything is committed.
    pub fn release_writer(&self) {
        let mut guard = self
            .inner
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !self.inner.dirty.load(Ordering::Acquire)
            && let Some(writer) = guard.take()
        {
            self.inner.writer_alive.store(false, Ordering::Release);
            let _ = writer.wait_merging_threads();
        }
    }

    /// Commits changes at most every couple of seconds and frees the writer
    /// after a long idle spell. Stops (after a final commit) on `cancel`.
    pub fn spawn_committer(&self, cancel: CancellationToken) -> JoinHandle<()> {
        let index = self.clone();
        tokio::spawn(async move {
            let mut idle = 0;
            let mut tick = tokio::time::interval(COMMIT_INTERVAL);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tick.tick() => {}
                }
                let step = committer_step(
                    &mut idle,
                    index.inner.dirty.load(Ordering::Acquire),
                    index.inner.used.swap(false, Ordering::AcqRel),
                    index.has_writer(),
                );
                // Nothing to commit and no release due: stay off the blocking
                // pool, whose threads would otherwise live on between ticks.
                let Step::Run { commit, release } = step else {
                    continue;
                };
                let worker = index.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if commit && let Err(e) = worker.commit() {
                        tracing::error!(error = %e, "scheduled index commit failed");
                    }
                    if release {
                        worker.release_writer();
                    }
                })
                .await;
            }
            let last = index.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let _ = last.commit();
                last.release_writer();
            })
            .await;
        })
    }

    pub fn search(&self, request: &SearchRequest) -> Result<SearchPage> {
        self.search_with(request, &BTreeSet::new())
    }

    /// [`Self::search`], told which `field.<key>` fields hold dates: the index
    /// stores a date as a number of days, so only the caller (who has the
    /// document types) knows that `field.expiry_date<2037` means a year.
    pub fn search_with(
        &self,
        request: &SearchRequest,
        date_fields: &BTreeSet<String>,
    ) -> Result<SearchPage> {
        exec::search(&self.inner, request, date_fields)
    }
}

/// What the committer does on one tick.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Idle,
    Run { commit: bool, release: bool },
}

/// Decides one committer tick. `idle` counts ticks since the writer was last
/// used by anyone: a change waiting for a commit (`dirty`) or any use since
/// the previous tick (`used`, which also covers a search or rebuild that
/// committed by itself before the committer looked) resets it.
fn committer_step(idle: &mut u32, dirty: bool, used: bool, has_writer: bool) -> Step {
    *idle = if dirty || used {
        0
    } else {
        idle.saturating_add(1)
    };
    let release = has_writer && *idle >= IDLE_TICKS_BEFORE_RELEASE;
    if dirty || release {
        Step::Run {
            commit: dirty,
            release,
        }
    } else {
        Step::Idle
    }
}

fn field_json(value: &FieldValue) -> Option<Value> {
    Some(match value {
        FieldValue::Text(s) => json!(s),
        // Any spelling of an identifier finds it: stored without separators.
        FieldValue::Identifier(s) => json!(ident::squash(s)),
        FieldValue::Date(d) => json!(days(*d) as f64),
        FieldValue::Money { amount, .. } => json!(amount.to_f64()?),
        FieldValue::Number(n) => json!(n.to_f64()?),
        FieldValue::List(items) => json!(items),
    })
}

/// Plain text of a field value, searchable as ordinary words.
fn field_text(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(s) | FieldValue::Identifier(s) => s.clone(),
        FieldValue::Date(d) => d.to_string(),
        FieldValue::Money { amount, currency } => format!("{amount} {currency}"),
        FieldValue::Number(n) => n.to_string(),
        FieldValue::List(items) => items.join("\n"),
    }
}

fn to_json(doc: &IndexDoc, fields: &Fields) -> Value {
    let mut all = String::new();
    let mut push = |s: &str| {
        all.push_str(s);
        all.push('\n');
    };
    push(&doc.title);
    if let Some(summary) = &doc.summary {
        push(summary);
    }
    for text in doc.texts.iter().chain(&doc.other_texts) {
        push(text);
    }
    for name in &doc.correspondent {
        push(name);
    }
    for (_, aliases) in &doc.people {
        for alias in aliases {
            push(alias);
        }
    }
    for tag in &doc.tags {
        push(tag);
    }
    for name in &doc.type_names {
        push(name);
    }
    for value in doc.fields.values() {
        push(&field_text(value));
    }

    let from_text: Vec<String> = doc
        .correspondent
        .iter()
        .map(|n| matching::normalize(n))
        .collect();
    let person_text: Vec<String> = doc
        .people
        .iter()
        .flat_map(|(_, aliases)| aliases)
        .map(|n| matching::normalize(n))
        .collect();

    let in_type_order: Vec<&String> = if doc.field_order.is_empty() {
        doc.fields.keys().collect()
    } else {
        doc.field_order.iter().collect()
    };
    let key_fields = |order: Vec<&String>| -> IndexMap<String, FieldValue> {
        order
            .into_iter()
            .filter_map(|k| doc.fields.get(k).map(|v| (k.clone(), v.clone())))
            .take(KEY_FIELD_COUNT)
            .collect()
    };
    let mut summary = key_fields(doc.summary_fields.iter().collect());
    if summary.is_empty() {
        summary = key_fields(in_type_order);
    }
    let due_date = due::due_date(doc.type_key.as_deref(), |k| doc.fields.get(k));
    let meta = StoredMeta {
        title: doc.title.clone(),
        type_key: doc.type_key.clone(),
        document_date: doc.document_date,
        due_date,
        organizations: doc.organization_names.clone(),
        people: doc
            .people
            .iter()
            .filter_map(|(_, aliases)| aliases.first().cloned())
            .collect(),
        key_fields: summary,
        trashed: doc.trashed,
        attention: doc.attention.clone(),
        has_pages: Some(doc.has_pages),
        summary: doc.summary.clone(),
    };

    let mut has: Vec<String> = Vec::new();
    let mut mark = |condition: bool, name: &str| {
        if condition {
            has.push(name.to_string());
        }
    };
    mark(!doc.attention.is_empty(), "attention");
    mark(doc.document_date.is_some(), "date");
    mark(due_date.is_some(), "due");
    mark(!doc.tags.is_empty(), "tags");
    mark(!doc.people.is_empty(), "people");
    mark(
        doc.summary.as_deref().is_some_and(|s| !s.is_empty()),
        "summary",
    );
    mark(doc.type_key.is_some(), "type");
    mark(
        !doc.correspondent_ids.is_empty() || !doc.correspondent.is_empty(),
        "from",
    );
    has.extend(doc.fields.keys().map(|k| format!("field.{k}")));

    let typed: serde_json::Map<String, Value> = doc
        .fields
        .iter()
        .filter_map(|(k, v)| field_json(v).map(|j| (k.clone(), j)))
        .collect();

    let mut out = json!({
        "id": doc.id.to_string(),
        "body": markup::plain(&doc.texts.join("\n\n")),
        "meta": serde_json::to_string(&meta).unwrap_or_default(),
        "text_ident": ident::glued_spellings(&all),
        "text_exact": all,
        "from_text": from_text,
        "person_text": person_text,
        "person_id": doc.people.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>(),
        "correspondent_id": doc.correspondent_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "tag": doc.tags.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>(),
        "has": has,
        "date_sort_desc": doc.document_date.map_or(i64::MIN, days),
        "date_sort_asc": doc.document_date.map_or(i64::MAX, days),
        "added_day": days(doc.added_at.date_naive()),
        "added_ts": doc.added_at.timestamp(),
        "local_only": u64::from(doc.local_only),
        "trashed": u64::from(doc.trashed),
        "fields": typed,
    });
    let object = out.as_object_mut().expect("object literal");
    for stem in fields.stems() {
        object.insert(format!("text_{}", stem.analyzer), json!(all));
    }
    if let Some(type_key) = &doc.type_key {
        object.insert("type_key".into(), json!(type_key.to_lowercase()));
    }
    if let Some(date) = doc.document_date {
        object.insert("document_date".into(), json!(days(date)));
    }
    if let Some(date) = due_date {
        object.insert("due_day".into(), json!(days(date)));
    }
    out
}

#[cfg(test)]
mod committer_tests;
#[cfg(test)]
mod sync_tests;
