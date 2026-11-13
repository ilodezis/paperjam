//! The tantivy schema. Changing it invalidates existing indexes; `open`
//! then recreates the directory and the startup reindex refills it.

use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, JsonObjectOptions, STORED, STRING, Schema,
    TextFieldIndexing, TextOptions,
};

use super::analyzers;

/// One full-text field analyzed with one language's stemmer.
#[derive(Debug, Clone, Copy)]
pub struct StemField {
    pub field: Field,
    pub analyzer: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Fields {
    pub id: Field,
    /// Display text of all pages; stored only, used for snippets.
    pub body: Field,
    /// Serialized `StoredMeta` for building hits without touching SQLite.
    pub meta: Field,
    /// One field per stemming language (`text_en`, `text_ru`, ...), filled
    /// from the front. Read through [`Fields::stems`].
    stem: [Option<StemField>; analyzers::MAX_STEMMED],
    pub text_exact: Field,
    /// Adjacent identifier-like words glued together (`AP 123` → `AP123`),
    /// so that every spelling of an identifier finds it.
    pub text_ident: Field,
    /// Folded names and aliases of correspondents / people.
    pub from_text: Field,
    pub person_text: Field,
    pub type_key: Field,
    pub person_id: Field,
    pub correspondent_id: Field,
    pub tag: Field,
    /// Presence markers: `attention`, `date`, `tags`, `people`, `summary`,
    /// `type`, `from`, `due`, and `field.<key>` for every stored field value.
    pub has: Field,
    pub document_date: Field,
    pub date_sort_desc: Field,
    pub date_sort_asc: Field,
    pub added_day: Field,
    /// Day the document is due to be renewed or replaced, when it has one
    /// (see `model::due`).
    pub due_day: Field,
    pub added_ts: Field,
    pub local_only: Field,
    pub trashed: Field,
    pub fields: Field,
}

fn text(analyzer: &str, option: IndexRecordOption) -> TextOptions {
    TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(analyzer)
            .set_index_option(option),
    )
}

impl Fields {
    /// The stemmed full-text fields, one per configured language.
    pub fn stems(&self) -> impl Iterator<Item = StemField> + '_ {
        self.stem.iter().flatten().copied()
    }
}

/// Builds the schema with one stemmed text field per tag of
/// [`analyzers::stem_tags`].
pub fn build(tags: &[&'static str]) -> (Schema, Fields) {
    let mut b = Schema::builder();
    let positions = IndexRecordOption::WithFreqsAndPositions;
    let id = b.add_text_field("id", STRING | STORED);
    let body = b.add_text_field("body", TextOptions::default().set_stored());
    let meta = b.add_text_field("meta", TextOptions::default().set_stored());
    let mut stem = [None; analyzers::MAX_STEMMED];
    for (slot, &tag) in stem.iter_mut().zip(tags) {
        *slot = Some(StemField {
            field: b.add_text_field(&format!("text_{tag}"), text(tag, positions)),
            analyzer: tag,
        });
    }
    let fields = Fields {
        id,
        body,
        meta,
        stem,
        text_exact: b.add_text_field("text_exact", text(analyzers::EXACT, positions)),
        text_ident: b.add_text_field(
            "text_ident",
            text(analyzers::EXACT, IndexRecordOption::Basic),
        ),
        from_text: b.add_text_field("from_text", text(analyzers::NORM, positions)),
        person_text: b.add_text_field("person_text", text(analyzers::NORM, positions)),
        type_key: b.add_text_field("type_key", STRING),
        person_id: b.add_text_field("person_id", STRING),
        correspondent_id: b.add_text_field("correspondent_id", STRING),
        tag: b.add_text_field("tag", STRING),
        has: b.add_text_field("has", STRING),
        document_date: b.add_i64_field("document_date", INDEXED | FAST),
        date_sort_desc: b.add_i64_field("date_sort_desc", FAST),
        date_sort_asc: b.add_i64_field("date_sort_asc", FAST),
        added_day: b.add_i64_field("added_day", INDEXED | FAST),
        due_day: b.add_i64_field("due_day", INDEXED | FAST),
        added_ts: b.add_i64_field("added_ts", FAST),
        local_only: b.add_u64_field("local_only", INDEXED),
        trashed: b.add_u64_field("trashed", INDEXED),
        fields: b.add_json_field(
            "fields",
            JsonObjectOptions::default()
                .set_indexing_options(
                    TextFieldIndexing::default()
                        .set_tokenizer(analyzers::EXACT)
                        .set_index_option(IndexRecordOption::Basic),
                )
                .set_fast("raw"),
        ),
    };
    (b.build(), fields)
}
