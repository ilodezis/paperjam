//! People, correspondents, tags and document types (with seeding of the
//! built-in types).

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{Result, ServiceError};
use crate::ids::{CorrespondentId, PersonId, TagId, TypeId};
use crate::jobs::queue::JobKind;
use crate::model::fields::{FieldDef, FieldKind, Labels};
use crate::model::taxonomy::{
    CorrespondentPatch, CorrespondentView, NewCorrespondent, NewPerson, NewType, PersonPatch,
    PersonView, TagView, Taxonomy, TypePatch, TypeView,
};
use crate::search::sync::{self, Related};
use crate::service::{App, Ctx, events};
use crate::time;

const SEED_TYPES: &str = include_str!("../seed/types.json");
/// Names accepted in `FieldDef::validator` (see `crate::validators`).
const KNOWN_VALIDATORS: [&str; 15] = [
    "idnp",
    "cnp",
    "inn",
    "snils",
    "iban",
    "pesel",
    "bsn",
    "nir",
    "steuer_id",
    "dni_nie",
    "cpf",
    "aadhaar",
    "nhs",
    "mrz",
    "personal_number",
];
const MAX_NAME_CHARS: usize = 200;
pub(crate) const MAX_TAG_CHARS: usize = 100;

macro_rules! person_select {
    ($rest:literal) => {
        person_select!(
            @cols
            "(SELECT COUNT(*) FROM document_people dp JOIN documents d ON d.id = dp.document_id \
              WHERE dp.person_id = p.id AND d.trashed_at IS NULL)",
            $rest
        )
    };
    (@bare $rest:literal) => {
        person_select!(@cols "0", $rest)
    };
    (@cols $count:literal, $rest:literal) => {
        concat!(
            "SELECT p.id, p.name, p.aliases, p.notes, p.created_at, ",
            $count,
            " AS document_count FROM people p",
            $rest
        )
    };
}

macro_rules! correspondent_select {
    ($rest:literal) => {
        correspondent_select!(
            @cols
            "(SELECT COUNT(DISTINCT d.id) FROM document_organizations do_org JOIN documents d ON d.id = do_org.document_id \
              WHERE do_org.correspondent_id = c.id AND d.trashed_at IS NULL)",
            $rest
        )
    };
    (@bare $rest:literal) => {
        correspondent_select!(@cols "0", $rest)
    };
    (@cols $count:literal, $rest:literal) => {
        concat!(
            "SELECT c.id, c.name, c.aliases, c.created_at, ",
            $count,
            " AS document_count FROM correspondents c",
            $rest
        )
    };
}

macro_rules! tag_select {
    ($rest:literal) => {
        tag_select!(
            @cols
            "(SELECT COUNT(*) FROM document_tags dt JOIN documents d ON d.id = dt.document_id \
              WHERE dt.tag_id = t.id AND d.trashed_at IS NULL)",
            $rest
        )
    };
    (@bare $rest:literal) => {
        tag_select!(@cols "0", $rest)
    };
    (@cols $count:literal, $rest:literal) => {
        concat!(
            "SELECT t.id, t.name, t.color, ",
            $count,
            " AS document_count FROM tags t",
            $rest
        )
    };
}

macro_rules! type_select {
    ($rest:literal) => {
        type_select!(
            @cols
            "(SELECT COUNT(*) FROM documents d \
              WHERE d.type_id = t.id AND d.trashed_at IS NULL)",
            $rest
        )
    };
    (@bare $rest:literal) => {
        type_select!(@cols "0", $rest)
    };
    (@cols $count:literal, $rest:literal) => {
        concat!(
            "SELECT t.id, t.key, t.labels, t.fields, t.summary_fields, t.builtin, t.created_at, ",
            $count,
            " AS document_count FROM document_types t",
            $rest
        )
    };
}

#[derive(sqlx::FromRow)]
struct PersonRow {
    id: PersonId,
    name: String,
    aliases: String,
    notes: String,
    created_at: String,
    document_count: i64,
}

impl From<PersonRow> for PersonView {
    fn from(r: PersonRow) -> Self {
        PersonView {
            id: r.id,
            name: r.name,
            aliases: parse_aliases(&r.aliases),
            notes: r.notes,
            created_at: r.created_at,
            document_count: r.document_count,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CorrespondentRow {
    id: CorrespondentId,
    name: String,
    aliases: String,
    created_at: String,
    document_count: i64,
}

impl From<CorrespondentRow> for CorrespondentView {
    fn from(r: CorrespondentRow) -> Self {
        CorrespondentView {
            id: r.id,
            name: r.name,
            aliases: parse_aliases(&r.aliases),
            created_at: r.created_at,
            document_count: r.document_count,
        }
    }
}

#[derive(sqlx::FromRow)]
struct TagRow {
    id: TagId,
    name: String,
    color: Option<String>,
    document_count: i64,
}

impl From<TagRow> for TagView {
    fn from(r: TagRow) -> Self {
        TagView {
            id: r.id,
            name: r.name,
            color: r.color,
            document_count: r.document_count,
        }
    }
}

#[derive(sqlx::FromRow)]
struct TypeRow {
    id: TypeId,
    key: String,
    labels: String,
    fields: String,
    summary_fields: Option<String>,
    builtin: bool,
    created_at: String,
    document_count: i64,
}

impl From<TypeRow> for TypeView {
    fn from(r: TypeRow) -> Self {
        TypeView {
            id: r.id,
            key: r.key,
            labels: serde_json::from_str(&r.labels).unwrap_or_default(),
            fields: serde_json::from_str(&r.fields).unwrap_or_default(),
            summary_fields: r
                .summary_fields
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            builtin: r.builtin,
            created_at: r.created_at,
            document_count: r.document_count,
        }
    }
}

fn parse_aliases(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Trims, drops empties and removes case-insensitive duplicates, keeping the
/// first spelling.
fn clean_aliases(input: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    input
        .into_iter()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty() && seen.insert(a.to_lowercase()))
        .collect()
}

fn clean_name(name: &str, max: usize) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > max {
        return Err(ServiceError::invalid(
            "invalid_name",
            format!("names are 1-{max} characters"),
        ));
    }
    Ok(name.to_string())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

/// `[a-z][a-z0-9_]{0,63}`
fn is_snake_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && key.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn validate_labels(labels: &Labels, what: &str) -> Result<()> {
    if labels.is_empty() || labels.values().any(|l| l.trim().is_empty()) {
        return Err(ServiceError::invalid(
            "invalid_labels",
            format!("{what} needs a non-empty label for every language it lists"),
        ));
    }
    Ok(())
}

fn validate_fields(fields: &[FieldDef]) -> Result<()> {
    let mut seen = HashSet::new();
    for f in fields {
        if !is_snake_key(&f.key) {
            return Err(ServiceError::invalid(
                "invalid_field_key",
                format!(
                    "{:?} is not a valid field key (lowercase letters, digits, _)",
                    f.key
                ),
            ));
        }
        if !seen.insert(f.key.as_str()) {
            return Err(ServiceError::invalid(
                "duplicate_field_key",
                format!("field {} appears twice", f.key),
            ));
        }
        validate_labels(&f.labels, &format!("field {}", f.key)).map_err(|mut e| {
            e.code = "invalid_field";
            e
        })?;
        if f.issuer && f.kind != FieldKind::Text {
            return Err(ServiceError::invalid(
                "invalid_field",
                format!("field {}: an issuer field must be a text field", f.key),
            ));
        }
        if let Some(v) = &f.validator
            && (f.kind != FieldKind::Identifier || !KNOWN_VALIDATORS.contains(&v.as_str()))
        {
            return Err(ServiceError::invalid(
                "invalid_field",
                format!(
                    "field {}: validator {v:?} needs an identifier field and a known name",
                    f.key
                ),
            ));
        }
    }
    Ok(())
}

// --- listing ------------------------------------------------------------

async fn fetch_taxonomy(
    pool: &SqlitePool,
    types_sql: &'static str,
    people_sql: &'static str,
    correspondents_sql: &'static str,
    tags_sql: &'static str,
) -> Result<Taxonomy> {
    let types: Vec<TypeRow> = sqlx::query_as(types_sql).fetch_all(pool).await?;
    let people: Vec<PersonRow> = sqlx::query_as(people_sql).fetch_all(pool).await?;
    let correspondents: Vec<CorrespondentRow> =
        sqlx::query_as(correspondents_sql).fetch_all(pool).await?;
    let tags: Vec<TagRow> = sqlx::query_as(tags_sql).fetch_all(pool).await?;
    Ok(Taxonomy {
        types: types.into_iter().map(Into::into).collect(),
        people: people.into_iter().map(Into::into).collect(),
        correspondents: correspondents.into_iter().map(Into::into).collect(),
        tags: tags.into_iter().map(Into::into).collect(),
    })
}

pub async fn list_taxonomy(ctx: &Ctx<'_>) -> Result<Taxonomy> {
    fetch_taxonomy(
        &ctx.app.db.read,
        type_select!(" ORDER BY t.id"),
        person_select!(" ORDER BY p.name COLLATE NOCASE, p.id"),
        correspondent_select!(" ORDER BY c.name COLLATE NOCASE, c.id"),
        tag_select!(" ORDER BY t.name COLLATE NOCASE, t.id"),
    )
    .await
}

/// Same taxonomy vocabulary as [`list_taxonomy`] without document count
/// subqueries, for LLM prompts that ignore counts.
pub async fn list_taxonomy_for_llm(app: &App) -> Result<Taxonomy> {
    fetch_taxonomy(
        &app.db.read,
        type_select!(@bare " ORDER BY t.id"),
        person_select!(@bare " ORDER BY p.name COLLATE NOCASE, p.id"),
        correspondent_select!(@bare " ORDER BY c.name COLLATE NOCASE, c.id"),
        tag_select!(@bare " ORDER BY t.name COLLATE NOCASE, t.id"),
    )
    .await
}

async fn fetch_person(pool: &SqlitePool, id: PersonId) -> Result<PersonView> {
    sqlx::query_as::<_, PersonRow>(person_select!(" WHERE p.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(Into::into)
        .ok_or_else(person_not_found)
}

async fn fetch_correspondent(pool: &SqlitePool, id: CorrespondentId) -> Result<CorrespondentView> {
    sqlx::query_as::<_, CorrespondentRow>(correspondent_select!(" WHERE c.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(Into::into)
        .ok_or_else(correspondent_not_found)
}

async fn fetch_tag(pool: &SqlitePool, id: TagId) -> Result<TagView> {
    sqlx::query_as::<_, TagRow>(tag_select!(" WHERE t.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(Into::into)
        .ok_or_else(tag_not_found)
}

async fn fetch_type(pool: &SqlitePool, id: TypeId) -> Result<TypeView> {
    sqlx::query_as::<_, TypeRow>(type_select!(" WHERE t.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(Into::into)
        .ok_or_else(type_not_found)
}

fn person_not_found() -> ServiceError {
    ServiceError::not_found("person_not_found", "no such person")
}

fn correspondent_not_found() -> ServiceError {
    ServiceError::not_found("correspondent_not_found", "no such correspondent")
}

fn tag_not_found() -> ServiceError {
    ServiceError::not_found("tag_not_found", "no such tag")
}

fn type_not_found() -> ServiceError {
    ServiceError::not_found("type_not_found", "no such document type")
}

// --- people and correspondents: shared alias handling ------------------------

/// The two entities that carry a display name plus spelling aliases.
#[derive(Clone, Copy)]
enum Entity {
    Person,
    Correspondent,
}

impl Entity {
    /// Static table name; the only text ever spliced into SQL below.
    fn table(self) -> &'static str {
        match self {
            Entity::Person => "people",
            Entity::Correspondent => "correspondents",
        }
    }

    fn not_found(self) -> ServiceError {
        match self {
            Entity::Person => person_not_found(),
            Entity::Correspondent => correspondent_not_found(),
        }
    }
}

async fn read_name_and_aliases(
    conn: &mut SqliteConnection,
    entity: Entity,
    id: &str,
) -> Result<(String, Vec<String>)> {
    let sql = format!("SELECT name, aliases FROM {} WHERE id = ?", entity.table());
    let row: Option<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(conn)
        .await?;
    let (name, aliases) = row.ok_or_else(|| entity.not_found())?;
    Ok((name, parse_aliases(&aliases)))
}

async fn write_aliases(
    conn: &mut SqliteConnection,
    entity: Entity,
    id: &str,
    aliases: &[String],
) -> Result<()> {
    let sql = format!("UPDATE {} SET aliases = ? WHERE id = ?", entity.table());
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(json!(aliases).to_string())
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Re-indexes the documents of an entity after its names changed.
async fn refresh_entity(ctx: &Ctx<'_>, entity: Entity, id: &str) {
    let related = match entity {
        Entity::Person => id.parse().ok().map(Related::Person),
        Entity::Correspondent => id.parse().ok().map(Related::Correspondent),
    };
    if let Some(related) = related {
        sync::refresh_related(ctx.app, related).await;
    }
}

/// Adds (`add == true`) or removes one alias.
async fn change_alias(
    ctx: &Ctx<'_>,
    entity: Entity,
    id: &str,
    alias: &str,
    add: bool,
    action: &str,
) -> Result<()> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err(ServiceError::invalid(
            "invalid_alias",
            "an alias cannot be empty",
        ));
    }
    let mut tx = ctx.app.db.write.begin().await?;
    let (_, mut aliases) = read_name_and_aliases(&mut tx, entity, id).await?;
    if add {
        aliases.push(alias.to_string());
        aliases = clean_aliases(aliases);
    } else {
        let needle = alias.to_lowercase();
        aliases.retain(|a| a.to_lowercase() != needle);
    }
    write_aliases(&mut tx, entity, id, &aliases).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        action,
        None,
        json!({"id": id, "alias": alias}),
    )
    .await?;
    if matches!(entity, Entity::Person) {
        request_relink(ctx.app, &mut tx).await?;
    }
    tx.commit().await?;
    ctx.app.jobs.wake();
    refresh_entity(ctx, entity, id).await;
    Ok(())
}

/// Validates a merge and writes the union of aliases (plus the name of the
/// record that is going away) onto `into`.
async fn merge_aliases(
    tx: &mut SqliteConnection,
    entity: Entity,
    into: &str,
    from: &str,
) -> Result<()> {
    if into == from {
        return Err(ServiceError::invalid(
            "merge_into_self",
            "cannot merge a record into itself",
        ));
    }
    let (_, into_aliases) = read_name_and_aliases(tx, entity, into).await?;
    let (from_name, from_aliases) = read_name_and_aliases(tx, entity, from).await?;
    let union = clean_aliases(
        into_aliases
            .into_iter()
            .chain(from_aliases)
            .chain([from_name]),
    );
    write_aliases(tx, entity, into, &union).await
}

// --- people ----------------------------------------------------------------

/// Queues the matching of the existing documents against the people, to run
/// in the background once `tx` commits (call `wake` after that). A request
/// already waiting in the queue covers this one.
async fn request_relink(app: &App, tx: &mut SqliteConnection) -> Result<()> {
    app.jobs
        .enqueue(tx, JobKind::Relink, None, json!({}))
        .await?;
    Ok(())
}

pub async fn create_person(ctx: &Ctx<'_>, new: NewPerson) -> Result<PersonView> {
    let name = clean_name(&new.name, MAX_NAME_CHARS)?;
    let id = PersonId::new();
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query("INSERT INTO people (id, name, aliases, notes, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(id)
        .bind(&name)
        .bind(json!(clean_aliases(new.aliases)).to_string())
        .bind(new.notes.trim())
        .bind(time::now_ts())
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "person.create",
        None,
        json!({"id": id}),
    )
    .await?;
    request_relink(ctx.app, &mut tx).await?;
    tx.commit().await?;
    ctx.app.jobs.wake();
    fetch_person(&ctx.app.db.read, id).await
}

pub async fn update_person(ctx: &Ctx<'_>, id: PersonId, patch: PersonPatch) -> Result<PersonView> {
    let name = patch
        .name
        .as_deref()
        .map(|n| clean_name(n, MAX_NAME_CHARS))
        .transpose()?;
    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query(
        "UPDATE people SET name = COALESCE(?, name), notes = COALESCE(?, notes) WHERE id = ?",
    )
    .bind(name)
    .bind(patch.notes.as_deref().map(str::trim))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(person_not_found());
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "person.update",
        None,
        json!({"id": id}),
    )
    .await?;
    if patch.name.is_some() {
        request_relink(ctx.app, &mut tx).await?;
    }
    tx.commit().await?;
    ctx.app.jobs.wake();
    sync::refresh_related(ctx.app, Related::Person(id)).await;
    fetch_person(&ctx.app.db.read, id).await
}

pub async fn add_person_alias(ctx: &Ctx<'_>, id: PersonId, alias: &str) -> Result<PersonView> {
    change_alias(
        ctx,
        Entity::Person,
        &id.to_string(),
        alias,
        true,
        "person.add_alias",
    )
    .await?;
    fetch_person(&ctx.app.db.read, id).await
}

pub async fn remove_person_alias(ctx: &Ctx<'_>, id: PersonId, alias: &str) -> Result<PersonView> {
    change_alias(
        ctx,
        Entity::Person,
        &id.to_string(),
        alias,
        false,
        "person.remove_alias",
    )
    .await?;
    fetch_person(&ctx.app.db.read, id).await
}

pub async fn merge_people(ctx: &Ctx<'_>, into: PersonId, from: PersonId) -> Result<PersonView> {
    let mut tx = ctx.app.db.write.begin().await?;
    merge_aliases(
        &mut tx,
        Entity::Person,
        &into.to_string(),
        &from.to_string(),
    )
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) \
         SELECT document_id, ?, provenance FROM document_people WHERE person_id = ?",
    )
    .bind(into)
    .bind(from)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE users SET person_id = ? WHERE person_id = ?")
        .bind(into)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    // Remaining links of `from` go with it (ON DELETE CASCADE).
    sqlx::query("DELETE FROM people WHERE id = ?")
        .bind(from)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "person.merge",
        None,
        json!({"into": into, "from": from}),
    )
    .await?;
    request_relink(ctx.app, &mut tx).await?;
    tx.commit().await?;
    ctx.app.jobs.wake();
    sync::refresh_related(ctx.app, Related::Person(into)).await;
    fetch_person(&ctx.app.db.read, into).await
}

/// Asks for the existing documents to be matched against the people again,
/// without the LLM. Documents whose people were set by hand are left alone.
/// The work runs in the background; the returned view is the person as of now.
pub async fn relink_person(ctx: &Ctx<'_>, id: PersonId) -> Result<PersonView> {
    let mut tx = ctx.app.db.write.begin().await?;
    let found: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM people WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if found.is_none() {
        return Err(person_not_found());
    }
    request_relink(ctx.app, &mut tx).await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "person.relink",
        None,
        json!({"id": id}),
    )
    .await?;
    tx.commit().await?;
    ctx.app.jobs.wake();
    fetch_person(&ctx.app.db.read, id).await
}

pub async fn delete_person(ctx: &Ctx<'_>, id: PersonId) -> Result<()> {
    let affected = sync::documents_of(ctx.app, Related::Person(id)).await?;
    let mut tx = ctx.app.db.write.begin().await?;
    let name: String = sqlx::query_scalar("SELECT name FROM people WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(person_not_found)?;

    sqlx::query("DELETE FROM document_people WHERE person_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET person_id = NULL WHERE person_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM people WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "person.delete",
        None,
        json!({"id": id, "name": name}),
    )
    .await?;
    tx.commit().await?;
    sync::refresh_many(ctx.app, &affected).await;
    Ok(())
}

// --- correspondents --------------------------------------------------------

pub async fn create_correspondent(
    ctx: &Ctx<'_>,
    new: NewCorrespondent,
) -> Result<CorrespondentView> {
    let name = clean_name(&new.name, MAX_NAME_CHARS)?;
    let id = CorrespondentId::new();
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query("INSERT INTO correspondents (id, name, aliases, created_at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(&name)
        .bind(json!(clean_aliases(new.aliases)).to_string())
        .bind(time::now_ts())
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "correspondent.create",
        None,
        json!({"id": id}),
    )
    .await?;
    tx.commit().await?;
    fetch_correspondent(&ctx.app.db.read, id).await
}

pub async fn update_correspondent(
    ctx: &Ctx<'_>,
    id: CorrespondentId,
    patch: CorrespondentPatch,
) -> Result<CorrespondentView> {
    let name = patch
        .name
        .as_deref()
        .map(|n| clean_name(n, MAX_NAME_CHARS))
        .transpose()?;
    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query("UPDATE correspondents SET name = COALESCE(?, name) WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(correspondent_not_found());
    }
    events::record(
        &mut tx,
        &ctx.actor,
        "correspondent.update",
        None,
        json!({"id": id}),
    )
    .await?;
    tx.commit().await?;
    sync::refresh_related(ctx.app, Related::Correspondent(id)).await;
    fetch_correspondent(&ctx.app.db.read, id).await
}

pub async fn add_correspondent_alias(
    ctx: &Ctx<'_>,
    id: CorrespondentId,
    alias: &str,
) -> Result<CorrespondentView> {
    change_alias(
        ctx,
        Entity::Correspondent,
        &id.to_string(),
        alias,
        true,
        "correspondent.add_alias",
    )
    .await?;
    fetch_correspondent(&ctx.app.db.read, id).await
}

pub async fn remove_correspondent_alias(
    ctx: &Ctx<'_>,
    id: CorrespondentId,
    alias: &str,
) -> Result<CorrespondentView> {
    change_alias(
        ctx,
        Entity::Correspondent,
        &id.to_string(),
        alias,
        false,
        "correspondent.remove_alias",
    )
    .await?;
    fetch_correspondent(&ctx.app.db.read, id).await
}

pub async fn merge_correspondents(
    ctx: &Ctx<'_>,
    into: CorrespondentId,
    from: CorrespondentId,
) -> Result<CorrespondentView> {
    let mut tx = ctx.app.db.write.begin().await?;
    merge_aliases(
        &mut tx,
        Entity::Correspondent,
        &into.to_string(),
        &from.to_string(),
    )
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO document_organizations (document_id, correspondent_id, role, provenance) \
         SELECT document_id, ?, role, provenance FROM document_organizations WHERE correspondent_id = ?",
    )
    .bind(into)
    .bind(from)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM document_organizations WHERE correspondent_id = ?")
        .bind(from)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM correspondents WHERE id = ?")
        .bind(from)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "correspondent.merge",
        None,
        json!({"into": into, "from": from}),
    )
    .await?;
    tx.commit().await?;
    sync::refresh_related(ctx.app, Related::Correspondent(into)).await;
    fetch_correspondent(&ctx.app.db.read, into).await
}

pub async fn delete_correspondent(ctx: &Ctx<'_>, id: CorrespondentId) -> Result<()> {
    let affected = sync::documents_of(ctx.app, Related::Correspondent(id)).await?;
    let mut tx = ctx.app.db.write.begin().await?;
    let name: String = sqlx::query_scalar("SELECT name FROM correspondents WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(correspondent_not_found)?;

    sqlx::query("DELETE FROM correspondents WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    events::record(
        &mut tx,
        &ctx.actor,
        "correspondent.delete",
        None,
        json!({"id": id, "name": name}),
    )
    .await?;
    tx.commit().await?;
    sync::refresh_many(ctx.app, &affected).await;
    Ok(())
}

/// Tidies stored correspondent names (whitespace, quote spacing) using
/// `normalize::correspondent_name`, merges any duplicates created by the tidy,
/// merges offices whose names differ only in the designation word at the
/// front ("Отдел ЗАГС ..." / "Управление ЗАГС ..."), and merges bare
/// abbreviations into their full names with parentheses (e.g. "ASP" into
/// "Agenția Servicii Publice (ASP)").
/// Returns (renamed_count, merged_count).
pub async fn tidy_correspondents(ctx: &Ctx<'_>) -> Result<(usize, usize)> {
    let mut renamed = 0;
    let mut merged = 0;

    let rows: Vec<(CorrespondentId, String)> =
        sqlx::query_as("SELECT id, name FROM correspondents ORDER BY id")
            .fetch_all(&ctx.app.db.read)
            .await?;

    for (id, name) in rows {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM correspondents WHERE id = ?)")
                .bind(id)
                .fetch_one(&ctx.app.db.read)
                .await?;
        if !exists {
            continue;
        }

        if let Some(clean) = crate::llm::normalize::correspondent_name(&name).filter(|c| c != &name)
        {
            let duplicate: Option<CorrespondentId> =
                sqlx::query_scalar("SELECT id FROM correspondents WHERE name = ? AND id != ?")
                    .bind(&clean)
                    .bind(id)
                    .fetch_optional(&ctx.app.db.read)
                    .await?;

            if let Some(into) = duplicate {
                merge_correspondents(ctx, into, id).await?;
                merged += 1;
            } else {
                let mut tx = ctx.app.db.write.begin().await?;
                sqlx::query("UPDATE correspondents SET name = ? WHERE id = ?")
                    .bind(&clean)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                events::record(
                    &mut tx,
                    &ctx.actor,
                    "correspondent.update",
                    None,
                    json!({"id": id, "name": clean, "previous_name": name}),
                )
                .await?;
                tx.commit().await?;
                let _ = add_correspondent_alias(ctx, id, &name).await;
                sync::refresh_related(ctx.app, Related::Correspondent(id)).await;
                renamed += 1;
            }
        }
    }

    let current: Vec<(CorrespondentId, String)> =
        sqlx::query_as("SELECT id, name FROM correspondents ORDER BY id")
            .fetch_all(&ctx.app.db.read)
            .await?;

    // Offices whose stored names differ only in the designation word at the
    // front are one correspondent: the earlier record (ids are created in
    // order) keeps its name, the later one merges into it and stays as an
    // alias, so documents that print either spelling link to one office.
    let mut gone: HashSet<CorrespondentId> = HashSet::new();
    for (i, (into, into_name)) in current.iter().enumerate() {
        if gone.contains(into) {
            continue;
        }
        for (from, from_name) in &current[i + 1..] {
            if gone.contains(from) {
                continue;
            }
            if crate::matching::same_org_name(into_name, from_name) {
                merge_correspondents(ctx, *into, *from).await?;
                gone.insert(*from);
                merged += 1;
            }
        }
    }

    for (full_id, full_name) in &current {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM correspondents WHERE id = ?)")
                .bind(full_id)
                .fetch_one(&ctx.app.db.read)
                .await?;
        if !exists {
            continue;
        }
        if let Some(open) = full_name
            .rfind('(')
            .filter(|&open| full_name.ends_with(')') && open > 0)
        {
            let abbr = full_name[open + 1..full_name.len() - 1].trim();
            if !abbr.is_empty() {
                let bare: Option<CorrespondentId> =
                    sqlx::query_scalar("SELECT id FROM correspondents WHERE name = ? AND id != ?")
                        .bind(abbr)
                        .bind(full_id)
                        .fetch_optional(&ctx.app.db.read)
                        .await?;

                if let Some(from_id) = bare {
                    merge_correspondents(ctx, *full_id, from_id).await?;
                    merged += 1;
                }
            }
        }
    }

    Ok((renamed, merged))
}

// --- tags -------------------------------------------------------------------

fn validate_color(color: &Option<String>) -> Result<()> {
    match color {
        Some(c)
            if !(c.len() == 7
                && c.starts_with('#')
                && c[1..].bytes().all(|b| b.is_ascii_hexdigit())) =>
        {
            Err(ServiceError::invalid(
                "invalid_color",
                "colors look like #ff8800",
            ))
        }
        _ => Ok(()),
    }
}

fn tag_exists() -> ServiceError {
    ServiceError::conflict("tag_exists", "a tag with that name already exists")
}

pub async fn create_tag(ctx: &Ctx<'_>, name: &str, color: Option<String>) -> Result<TagView> {
    let name = clean_name(name, MAX_TAG_CHARS)?;
    validate_color(&color)?;
    let id = TagId::new();
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query("INSERT INTO tags (id, name, color) VALUES (?, ?, ?)")
        .bind(id)
        .bind(&name)
        .bind(&color)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                tag_exists()
            } else {
                e.into()
            }
        })?;
    events::record(&mut tx, &ctx.actor, "tag.create", None, json!({"id": id})).await?;
    tx.commit().await?;
    fetch_tag(&ctx.app.db.read, id).await
}

pub async fn rename_tag(ctx: &Ctx<'_>, id: TagId, name: &str) -> Result<TagView> {
    let name = clean_name(name, MAX_TAG_CHARS)?;
    let mut tx = ctx.app.db.write.begin().await?;
    let updated = sqlx::query("UPDATE tags SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                tag_exists()
            } else {
                e.into()
            }
        })?;
    if updated.rows_affected() == 0 {
        return Err(tag_not_found());
    }
    events::record(&mut tx, &ctx.actor, "tag.rename", None, json!({"id": id})).await?;
    tx.commit().await?;
    sync::refresh_related(ctx.app, Related::Tag(id)).await;
    fetch_tag(&ctx.app.db.read, id).await
}

/// What `delete_tag` removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedTag {
    pub id: TagId,
    pub name: String,
    /// Documents the tag was taken off (trashed ones included).
    pub documents_detached: u64,
}

pub async fn delete_tag(ctx: &Ctx<'_>, id: TagId) -> Result<DeletedTag> {
    // Found before the delete cascades away the links.
    let affected = sync::documents_of(ctx.app, Related::Tag(id)).await?;
    let mut tx = ctx.app.db.write.begin().await?;
    let name: String = sqlx::query_scalar("SELECT name FROM tags WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(tag_not_found)?;
    let (documents,): (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT document_id) FROM document_tags WHERE tag_id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    // document_tags rows go with it (ON DELETE CASCADE).
    sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    events::record(&mut tx, &ctx.actor, "tag.delete", None, json!({"id": id})).await?;
    tx.commit().await?;
    sync::refresh_many(ctx.app, &affected).await;
    Ok(DeletedTag {
        id,
        name,
        documents_detached: u64::try_from(documents).unwrap_or_default(),
    })
}

// --- types ------------------------------------------------------------------

pub async fn create_type(ctx: &Ctx<'_>, new: NewType) -> Result<TypeView> {
    if !is_snake_key(&new.key) {
        return Err(ServiceError::invalid(
            "invalid_type_key",
            "type keys are lowercase letters, digits and underscores, starting with a letter",
        ));
    }
    validate_labels(&new.labels, "a type")?;
    validate_fields(&new.fields)?;
    let id = TypeId::new();
    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query(
        "INSERT INTO document_types (id, key, labels, fields, builtin, created_at) \
         VALUES (?, ?, ?, ?, 0, ?)",
    )
    .bind(id)
    .bind(&new.key)
    .bind(json!(new.labels).to_string())
    .bind(json!(new.fields).to_string())
    .bind(time::now_ts())
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ServiceError::conflict("type_key_taken", "a document type with that key exists")
        } else {
            e.into()
        }
    })?;
    events::record(
        &mut tx,
        &ctx.actor,
        "type.create",
        None,
        json!({"id": id, "key": new.key}),
    )
    .await?;
    tx.commit().await?;
    fetch_type(&ctx.app.db.read, id).await
}

pub async fn update_type(ctx: &Ctx<'_>, id: TypeId, patch: TypePatch) -> Result<TypeView> {
    if let Some(labels) = &patch.labels {
        validate_labels(labels, "a type")?;
    }
    if let Some(fields) = &patch.fields {
        validate_fields(fields)?;
    }
    let mut tx = ctx.app.db.write.begin().await?;
    // Stored field values are never touched: dropping a field from the list
    // only hides it.
    let updated = sqlx::query(
        "UPDATE document_types SET labels = COALESCE(?, labels), fields = COALESCE(?, fields) \
         WHERE id = ?",
    )
    .bind(patch.labels.as_ref().map(|l| json!(l).to_string()))
    .bind(patch.fields.as_ref().map(|f| json!(f).to_string()))
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(type_not_found());
    }
    events::record(&mut tx, &ctx.actor, "type.update", None, json!({"id": id})).await?;
    tx.commit().await?;
    sync::refresh_related(ctx.app, Related::Type(id)).await;
    fetch_type(&ctx.app.db.read, id).await
}

/// A built-in type as the seed file lists it.
#[derive(Deserialize)]
struct SeedType {
    #[serde(flatten)]
    new: NewType,
    #[serde(default)]
    summary_fields: Vec<String>,
}

/// Inserts built-in types that are missing; never touches existing rows, so
/// edits made by users survive. The one exception: a built-in type that has
/// no summary fields yet (it predates them) gets the seed's. Returns how many
/// types were inserted.
pub async fn seed_builtin_types(ctx: &Ctx<'_>) -> Result<usize> {
    let seed: Vec<SeedType> = serde_json::from_str(SEED_TYPES)
        .map_err(|_| ServiceError::internal("seed_invalid", "the built-in type list is invalid"))?;
    let mut tx = ctx.app.db.write.begin().await?;
    let mut inserted = 0;
    for SeedType {
        new: t,
        summary_fields,
    } in seed
    {
        let summary = (!summary_fields.is_empty()).then(|| json!(summary_fields).to_string());
        let result = sqlx::query(
            "INSERT INTO document_types (id, key, labels, fields, summary_fields, builtin, created_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?) ON CONFLICT(key) DO NOTHING",
        )
        .bind(TypeId::new())
        .bind(&t.key)
        .bind(json!(t.labels).to_string())
        .bind(json!(t.fields).to_string())
        .bind(&summary)
        .bind(time::now_ts())
        .execute(&mut *tx)
        .await?;
        inserted += result.rows_affected() as usize;
        if result.rows_affected() == 0 && summary.is_some() {
            sqlx::query(
                "UPDATE document_types SET summary_fields = ? \
                 WHERE key = ? AND builtin = 1 AND summary_fields IS NULL",
            )
            .bind(&summary)
            .bind(&t.key)
            .execute(&mut *tx)
            .await?;
        }
    }
    if inserted > 0 {
        events::record(
            &mut tx,
            &ctx.actor,
            "types.seed",
            None,
            json!({"inserted": inserted}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::ids::DocumentId;
    use crate::model::fields::{FieldDef, FieldKind, Labels};
    use crate::service::testing::test_app;
    use crate::service::{Actor, App};

    const SPEC_TYPES: [&str; 22] = [
        "passport",
        "travel_passport",
        "id_card",
        "residence_permit",
        "driver_license",
        "birth_certificate",
        "marriage_certificate",
        "diploma",
        "contract",
        "rental_agreement",
        "power_of_attorney",
        "insurance_policy",
        "bank_statement",
        "payment_confirmation",
        "invoice",
        "receipt",
        "warranty",
        "medical_document",
        "tax_document",
        "certificate_other",
        "letter",
        "other",
    ];

    async fn setup() -> (tempfile::TempDir, App) {
        test_app().await
    }

    fn labels(en: &str) -> Labels {
        Labels::from([("en".to_string(), en.to_string())])
    }

    fn field(key: &str, kind: FieldKind) -> FieldDef {
        FieldDef {
            key: key.to_string(),
            labels: labels(key),
            kind,
            validator: None,
            required: false,
            issuer: false,
        }
    }

    async fn insert_doc(app: &App, correspondent: Option<CorrespondentId>) -> DocumentId {
        let id = DocumentId::new();
        sqlx::query("INSERT INTO documents (id, created_at, updated_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind("2026-01-01T00:00:00.000Z")
            .bind("2026-01-01T00:00:00.000Z")
            .execute(&app.db.write)
            .await
            .unwrap();
        if let Some(cid) = correspondent {
            sqlx::query(
                "INSERT INTO document_organizations (document_id, correspondent_id, role, provenance) VALUES (?, ?, 'unspecified', 'manual')",
            )
            .bind(id)
            .bind(cid)
            .execute(&app.db.write)
            .await
            .unwrap();
        }
        id
    }

    async fn link_person_to_doc(app: &App, doc: DocumentId, person: PersonId) {
        sqlx::query(
            "INSERT INTO document_people (document_id, person_id, provenance) VALUES (?, ?, 'manual')",
        )
        .bind(doc)
        .bind(person)
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    fn person(name: &str, aliases: &[&str]) -> NewPerson {
        NewPerson {
            name: name.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            notes: String::new(),
        }
    }

    // --- seed -----------------------------------------------------------

    #[tokio::test]
    async fn seed_is_idempotent() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        let first = seed_builtin_types(&ctx).await.unwrap();
        assert_eq!(first, SPEC_TYPES.len());
        assert_eq!(seed_builtin_types(&ctx).await.unwrap(), 0);
        let tax = list_taxonomy(&ctx).await.unwrap();
        assert_eq!(tax.types.len(), SPEC_TYPES.len());
        assert!(tax.types.iter().all(|t| t.builtin));
    }

    #[tokio::test]
    async fn seed_does_not_overwrite_edited_labels() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        let passport = list_taxonomy(&ctx)
            .await
            .unwrap()
            .types
            .into_iter()
            .find(|t| t.key == "passport")
            .unwrap();
        let mut edited = passport.labels.clone();
        edited.insert("en".into(), "Travel passport".into());
        update_type(
            &ctx,
            passport.id,
            TypePatch {
                labels: Some(edited),
                fields: Some(vec![field("number", FieldKind::Identifier)]),
            },
        )
        .await
        .unwrap();

        seed_builtin_types(&ctx).await.unwrap();

        let after = list_taxonomy(&ctx)
            .await
            .unwrap()
            .types
            .into_iter()
            .find(|t| t.key == "passport")
            .unwrap();
        assert_eq!(after.labels["en"], "Travel passport");
        assert_eq!(after.fields.len(), 1);
    }

    #[tokio::test]
    async fn seed_restores_a_deleted_builtin_type() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        sqlx::query("DELETE FROM document_types WHERE key = 'receipt'")
            .execute(&app.db.write)
            .await
            .unwrap();
        assert_eq!(seed_builtin_types(&ctx).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn builtin_types_have_summary_fields_that_exist() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();
        for t in &tax.types {
            let keys: Vec<&str> = t.fields.iter().map(|f| f.key.as_str()).collect();
            for key in &t.summary_fields {
                assert!(keys.contains(&key.as_str()), "{}: no field {key}", t.key);
            }
            assert_eq!(
                t.summary_fields.is_empty(),
                t.fields.is_empty(),
                "{}: a type with fields shows some of them",
                t.key
            );
        }
        let summary = |key: &str| {
            tax.types
                .iter()
                .find(|t| t.key == key)
                .unwrap()
                .summary_fields
                .clone()
        };
        // The number first, then how long the document is good for.
        assert_eq!(summary("travel_passport"), ["number", "expiry_date"]);
        assert_eq!(summary("passport"), ["series", "number", "expiry_date"]);
    }

    #[tokio::test]
    async fn the_travel_passport_is_a_passport_without_a_series() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();
        let find = |key: &str| tax.types.iter().find(|t| t.key == key).unwrap();
        let keys =
            |key: &str| -> Vec<String> { find(key).fields.iter().map(|f| f.key.clone()).collect() };
        assert_eq!(find("travel_passport").labels["ru"], "Загранпаспорт");
        // What most people mean by "passport" in English is the travel one;
        // the series-bearing document of some countries is the internal one.
        assert_eq!(find("travel_passport").labels["en"], "Passport");
        assert_eq!(find("passport").labels["en"], "Internal Passport");
        let mut internal = keys("passport");
        internal.retain(|k| k != "series");
        assert_eq!(keys("travel_passport"), internal);
    }

    #[tokio::test]
    async fn seed_adds_summary_fields_to_builtin_types_that_predate_them() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        sqlx::query("UPDATE document_types SET summary_fields = NULL")
            .execute(&app.db.write)
            .await
            .unwrap();
        // Not an insert: the types exist, only their summary fields are filled.
        assert_eq!(seed_builtin_types(&ctx).await.unwrap(), 0);
        let tax = list_taxonomy(&ctx).await.unwrap();
        let passport = tax.types.iter().find(|t| t.key == "passport").unwrap();
        assert_eq!(passport.summary_fields, ["series", "number", "expiry_date"]);

        // An edit survives the next start.
        sqlx::query(
            "UPDATE document_types SET summary_fields = '[\"number\"]' WHERE key = 'passport'",
        )
        .execute(&app.db.write)
        .await
        .unwrap();
        seed_builtin_types(&ctx).await.unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();
        let passport = tax.types.iter().find(|t| t.key == "passport").unwrap();
        assert_eq!(passport.summary_fields, ["number"]);
    }

    #[tokio::test]
    async fn seed_covers_all_spec_types() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();

        let keys: Vec<&str> = tax.types.iter().map(|t| t.key.as_str()).collect();
        for expected in SPEC_TYPES {
            assert!(keys.contains(&expected), "missing type {expected}");
        }
        assert_eq!(keys.len(), SPEC_TYPES.len(), "unexpected extra types");

        for t in &tax.types {
            for lang in ["en", "ru"] {
                assert!(
                    t.labels.get(lang).is_some_and(|l| !l.trim().is_empty()),
                    "type {} lacks a {lang} label",
                    t.key
                );
            }
            let mut seen = std::collections::HashSet::new();
            for f in &t.fields {
                assert!(
                    seen.insert(f.key.clone()),
                    "{}: duplicate field {}",
                    t.key,
                    f.key
                );
                for lang in ["en", "ru"] {
                    assert!(
                        f.labels.get(lang).is_some_and(|l| !l.trim().is_empty()),
                        "{}.{} lacks a {lang} label",
                        t.key,
                        f.key
                    );
                }
                if f.validator.is_some() {
                    assert_eq!(f.kind, FieldKind::Identifier, "{}.{}", t.key, f.key);
                }
            }
        }
    }

    #[tokio::test]
    async fn seed_has_the_field_lists_the_spec_names() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::system());
        seed_builtin_types(&ctx).await.unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();
        let fields_of = |key: &str| -> Vec<(String, FieldKind)> {
            tax.types
                .iter()
                .find(|t| t.key == key)
                .unwrap_or_else(|| panic!("no type {key}"))
                .fields
                .iter()
                .map(|f| (f.key.clone(), f.kind))
                .collect()
        };
        let has = |key: &str, field: &str, kind: FieldKind| {
            assert!(
                fields_of(key).contains(&(field.to_string(), kind)),
                "{key} should have {field} as {kind:?}; has {:?}",
                fields_of(key)
            );
        };

        for (f, k) in [
            ("series", FieldKind::Identifier),
            ("number", FieldKind::Identifier),
            ("issuing_authority", FieldKind::Text),
            ("issue_date", FieldKind::Date),
            ("expiry_date", FieldKind::Date),
            ("personal_number", FieldKind::Identifier),
            ("surname", FieldKind::Text),
            ("given_names", FieldKind::Text),
            ("birth_date", FieldKind::Date),
            ("birth_place", FieldKind::Text),
            ("nationality", FieldKind::Text),
        ] {
            has("passport", f, k);
        }
        for ty in ["passport", "travel_passport", "id_card", "residence_permit"] {
            let pn = tax
                .types
                .iter()
                .find(|t| t.key == ty)
                .unwrap()
                .fields
                .iter()
                .find(|f| f.key == "personal_number")
                .unwrap_or_else(|| panic!("{ty} lacks personal_number"));
            assert_eq!(pn.validator.as_deref(), Some("personal_number"), "{ty}");
        }
        has("receipt", "merchant", FieldKind::Text);
        has("receipt", "total", FieldKind::Money);
        has("receipt", "purchase_date", FieldKind::Date);
        has("receipt", "items", FieldKind::List);
        has("receipt", "payment_method", FieldKind::Text);
        for ty in ["contract", "rental_agreement"] {
            has(ty, "parties", FieldKind::List);
            has(ty, "subject", FieldKind::Text);
            has(ty, "signed_date", FieldKind::Date);
            has(ty, "start_date", FieldKind::Date);
            has(ty, "end_date", FieldKind::Date);
            has(ty, "amount", FieldKind::Money);
        }
        let medical: Vec<String> = fields_of("medical_document")
            .into_iter()
            .map(|f| f.0)
            .collect();
        assert_eq!(
            medical,
            ["clinic", "doctor", "visit_date", "diagnosis_or_summary"]
        );
    }

    // --- people and correspondents -------------------------------------

    #[tokio::test]
    async fn people_crud_and_aliases() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let p = create_person(
            &ctx,
            person("Marik Testov", &["Марик Тестов", "marik testov"]),
        )
        .await
        .unwrap();
        // Case-insensitive duplicates collapse.
        assert_eq!(p.aliases, ["Марик Тестов", "marik testov"]);

        let p = add_person_alias(&ctx, p.id, "  Maric Testov ")
            .await
            .unwrap();
        assert_eq!(p.aliases.last().unwrap(), "Maric Testov");
        let p = add_person_alias(&ctx, p.id, "MARIC TESTOV").await.unwrap();
        assert_eq!(p.aliases.len(), 3);

        let p = remove_person_alias(&ctx, p.id, "maric testov")
            .await
            .unwrap();
        assert_eq!(p.aliases.len(), 2);

        let p = update_person(
            &ctx,
            p.id,
            PersonPatch {
                name: Some("Marik T.".into()),
                notes: Some("family".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!((p.name.as_str(), p.notes.as_str()), ("Marik T.", "family"));

        let err = create_person(&ctx, person("  ", &[])).await.unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Invalid, "invalid_name"));
        let err = update_person(&ctx, PersonId::new(), PersonPatch::default())
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn correspondents_crud_and_aliases() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let c = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Example Bank".into(),
                aliases: vec!["EXB".into()],
            },
        )
        .await
        .unwrap();
        let c = add_correspondent_alias(&ctx, c.id, "Example Bank Ltd")
            .await
            .unwrap();
        assert_eq!(c.aliases, ["EXB", "Example Bank Ltd"]);
        let c = remove_correspondent_alias(&ctx, c.id, "exb").await.unwrap();
        assert_eq!(c.aliases, ["Example Bank Ltd"]);
        let c = update_correspondent(
            &ctx,
            c.id,
            CorrespondentPatch {
                name: Some("Example Bank SA".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(c.name, "Example Bank SA");
    }

    #[tokio::test]
    async fn merge_correspondents_moves_documents_and_unions_aliases() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let keep = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Example Bank".into(),
                aliases: vec!["EXB".into()],
            },
        )
        .await
        .unwrap();
        let dup = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Example Bank Ltd".into(),
                aliases: vec!["exb".into(), "E-Bank".into()],
            },
        )
        .await
        .unwrap();
        let d1 = insert_doc(&app, Some(dup.id)).await;
        let d2 = insert_doc(&app, Some(keep.id)).await;

        let merged = merge_correspondents(&ctx, keep.id, dup.id).await.unwrap();

        assert_eq!(merged.id, keep.id);
        assert_eq!(merged.document_count, 2);
        // Union of aliases, deduplicated case-insensitively, plus the merged-away name.
        let mut aliases = merged.aliases.clone();
        aliases.sort();
        assert_eq!(aliases, ["E-Bank", "EXB", "Example Bank Ltd"]);

        let owner = |doc: DocumentId| {
            let pool = app.db.read.clone();
            async move {
                let rows: Vec<(CorrespondentId,)> = sqlx::query_as(
                    "SELECT correspondent_id FROM document_organizations WHERE document_id = ?",
                )
                .bind(doc)
                .fetch_all(&pool)
                .await
                .unwrap();
                rows.into_iter().next().map(|r| r.0)
            }
        };
        assert_eq!(owner(d1).await, Some(keep.id));
        assert_eq!(owner(d2).await, Some(keep.id));
        // `from` is gone and the merge is audited.
        let tax = list_taxonomy(&ctx).await.unwrap();
        assert_eq!(tax.correspondents.len(), 1);
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM events WHERE action = 'correspondent.merge'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn merge_correspondents_collapses_same_document_and_role() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let keep = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Keep Corp".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let dup = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Dup Corp".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();

        let doc = insert_doc(&app, None).await;
        // Link both keep and dup to doc with the same role 'party'.
        for cid in [keep.id, dup.id] {
            sqlx::query(
                "INSERT INTO document_organizations (document_id, correspondent_id, role, provenance) VALUES (?, ?, 'party', 'manual')",
            )
            .bind(doc)
            .bind(cid)
            .execute(&app.db.write)
            .await
            .unwrap();
        }

        let merged = merge_correspondents(&ctx, keep.id, dup.id).await.unwrap();
        assert_eq!(merged.document_count, 1);

        let rows: Vec<(CorrespondentId, String)> = sqlx::query_as(
            "SELECT correspondent_id, role FROM document_organizations WHERE document_id = ?",
        )
        .bind(doc)
        .fetch_all(&app.db.read)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, keep.id);
        assert_eq!(rows[0].1, "party");
    }

    #[tokio::test]
    async fn tidy_correspondents_cleans_quote_spacing_and_merges_duplicates_and_abbreviations() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());

        // 1. A correspondent with dirty quote spacing that has no duplicate
        let dirty = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "ООО \"  Ромашка \"".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();

        // 2. An existing clean correspondent and a dirty duplicate of it
        let clean = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "ООО \"Лютик\"".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let duplicate_dirty = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "ООО \" Лютик \"".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();

        // 3. A full name with abbreviation in parentheses, and a bare abbreviation
        let full = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Agenția Servicii Publice (ASP)".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let bare = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "ASP".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();

        let _doc_dup = insert_doc(&app, Some(duplicate_dirty.id)).await;
        let _doc_bare = insert_doc(&app, Some(bare.id)).await;

        let (renamed, merged) = tidy_correspondents(&ctx).await.unwrap();
        assert_eq!(renamed, 1, "dirty was renamed");
        assert_eq!(merged, 2, "duplicate_dirty and bare were merged");

        // Check dirty was renamed cleanly
        let dirty_view = fetch_correspondent(&app.db.read, dirty.id).await.unwrap();
        assert_eq!(dirty_view.name, "ООО \"Ромашка\"");
        assert!(
            dirty_view
                .aliases
                .contains(&"ООО \"  Ромашка \"".to_string())
        );

        // Check duplicate was merged into clean
        assert!(
            fetch_correspondent(&app.db.read, duplicate_dirty.id)
                .await
                .is_err()
        );
        let clean_view = fetch_correspondent(&app.db.read, clean.id).await.unwrap();
        assert_eq!(clean_view.document_count, 1);

        // Check bare abbreviation was merged into full
        assert!(fetch_correspondent(&app.db.read, bare.id).await.is_err());
        let full_view = fetch_correspondent(&app.db.read, full.id).await.unwrap();
        assert_eq!(full_view.document_count, 1);
        assert!(full_view.aliases.contains(&"ASP".to_string()));

        // Check idempotency: second run does nothing
        let (r2, m2) = tidy_correspondents(&ctx).await.unwrap();
        assert_eq!((r2, m2), (0, 0));
    }

    #[tokio::test]
    async fn tidy_correspondents_merges_offices_differing_in_their_designation_word() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());

        // The two spellings of one ЗАГС, as real documents printed them.
        let otdel = create_correspondent(
            &ctx,
            NewCorrespondent {
                name:
                    "Отдел ЗАГС администрации города Тестовска Тестинского автономного округа - Юга"
                        .into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let upravlenie = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Управление ЗАГС администрации города Тестовска Тестинского автономного округа - Юга".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        // A different ЗАГС is its own office, whatever its designation.
        let other_city = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Управление ЗАГС города Москвы".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let _doc_upravlenie = insert_doc(&app, Some(upravlenie.id)).await;
        let _doc_other = insert_doc(&app, Some(other_city.id)).await;

        let (renamed, merged) = tidy_correspondents(&ctx).await.unwrap();
        assert_eq!(renamed, 0);
        assert_eq!(
            merged, 1,
            "the Управление spelling merged into the earlier record"
        );

        // The earlier record (ids are created in order) keeps its name and
        // gains the other spelling as an alias; the document moved with the
        // merge; the Moscow office is untouched.
        assert!(
            fetch_correspondent(&app.db.read, upravlenie.id)
                .await
                .is_err()
        );
        let kept = fetch_correspondent(&app.db.read, otdel.id).await.unwrap();
        assert_eq!(kept.document_count, 1);
        assert!(
            kept.aliases
                .iter()
                .any(|a| a.starts_with("Управление ЗАГС"))
        );
        let moscow = fetch_correspondent(&app.db.read, other_city.id)
            .await
            .unwrap();
        assert_eq!(moscow.document_count, 1);

        // Idempotent.
        let (r2, m2) = tidy_correspondents(&ctx).await.unwrap();
        assert_eq!((r2, m2), (0, 0));
    }

    #[tokio::test]
    async fn merge_people_moves_links_users_and_unions_aliases() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let keep = create_person(&ctx, person("Marik Testov", &["Marik"]))
            .await
            .unwrap();
        let dup = create_person(&ctx, person("Maric Testov", &["Maric"]))
            .await
            .unwrap();
        let both = insert_doc(&app, None).await;
        let only_dup = insert_doc(&app, None).await;
        link_person_to_doc(&app, both, keep.id).await;
        link_person_to_doc(&app, both, dup.id).await;
        link_person_to_doc(&app, only_dup, dup.id).await;
        sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, person_id, created_at) \
             VALUES ('u1', 'u', 'u', 'x', ?, '2026-01-01T00:00:00.000Z')",
        )
        .bind(dup.id)
        .execute(&app.db.write)
        .await
        .unwrap();

        let merged = merge_people(&ctx, keep.id, dup.id).await.unwrap();

        assert_eq!(merged.document_count, 2);
        let mut aliases = merged.aliases.clone();
        aliases.sort();
        assert_eq!(aliases, ["Maric", "Maric Testov", "Marik"]);
        let (links,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM document_people WHERE person_id = ?")
                .bind(keep.id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(links, 2);
        let (user_person,): (Option<PersonId>,) =
            sqlx::query_as("SELECT person_id FROM users WHERE id = 'u1'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(user_person, Some(keep.id));
        assert_eq!(list_taxonomy(&ctx).await.unwrap().people.len(), 1);
    }

    #[tokio::test]
    async fn merging_into_itself_or_a_missing_record_fails() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let p = create_person(&ctx, person("A", &[])).await.unwrap();
        let err = merge_people(&ctx, p.id, p.id).await.unwrap_err();
        assert_eq!(
            (err.kind, err.code),
            (ErrorKind::Invalid, "merge_into_self")
        );
        let err = merge_people(&ctx, p.id, PersonId::new()).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        let c = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "B".into(),
                aliases: vec![],
            },
        )
        .await
        .unwrap();
        let err = merge_correspondents(&ctx, c.id, c.id).await.unwrap_err();
        assert_eq!(err.code, "merge_into_self");
    }

    #[tokio::test]
    async fn person_delete_unlinks_documents_users_and_records_event() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let p = create_person(&ctx, person("Ion Creangă", &["Ion"]))
            .await
            .unwrap();
        let doc = insert_doc(&app, None).await;
        link_person_to_doc(&app, doc, p.id).await;
        sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, person_id, created_at) \
             VALUES ('u2', 'u2', 'u2', 'x', ?, '2026-01-01T00:00:00.000Z')",
        )
        .bind(p.id)
        .execute(&app.db.write)
        .await
        .unwrap();

        delete_person(&ctx, p.id).await.unwrap();
        assert!(list_taxonomy(&ctx).await.unwrap().people.is_empty());

        let (links,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM document_people WHERE person_id = ?")
                .bind(p.id)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(links, 0, "deleting a person unlinks document_people");

        let (user_person,): (Option<PersonId>,) =
            sqlx::query_as("SELECT person_id FROM users WHERE id = 'u2'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(user_person, None, "deleting a person unlinks user");

        let err = delete_person(&ctx, p.id).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);

        let (event_count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM events WHERE action = 'person.delete'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(event_count, 1);
    }

    #[tokio::test]
    async fn correspondent_delete_unlinks_documents_and_records_event() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let c = create_correspondent(
            &ctx,
            NewCorrespondent {
                name: "Orange Telecom".into(),
                aliases: vec!["Orange".into()],
            },
        )
        .await
        .unwrap();
        let doc = insert_doc(&app, Some(c.id)).await;

        delete_correspondent(&ctx, c.id).await.unwrap();
        assert!(list_taxonomy(&ctx).await.unwrap().correspondents.is_empty());

        let org_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM document_organizations WHERE document_id = ?")
                .bind(doc)
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(
            org_count, 0,
            "deleting a correspondent removes its document_organizations rows"
        );

        let err = delete_correspondent(&ctx, c.id).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);

        let (event_count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM events WHERE action = 'correspondent.delete'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(event_count, 1);
    }

    // --- tags ------------------------------------------------------------

    #[tokio::test]
    async fn tags_create_rename_delete() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let t = create_tag(&ctx, "Warranty", Some("#ff8800".into()))
            .await
            .unwrap();
        let err = create_tag(&ctx, "warranty", None).await.unwrap_err();
        assert_eq!((err.kind, err.code), (ErrorKind::Conflict, "tag_exists"));

        let doc = insert_doc(&app, None).await;
        sqlx::query(
            "INSERT INTO document_tags (document_id, tag_id, provenance) VALUES (?, ?, 'manual')",
        )
        .bind(doc)
        .bind(t.id)
        .execute(&app.db.write)
        .await
        .unwrap();
        let t = rename_tag(&ctx, t.id, "Guarantee").await.unwrap();
        assert_eq!(t.name, "Guarantee");
        assert_eq!(t.document_count, 1);

        let deleted = delete_tag(&ctx, t.id).await.unwrap();
        assert_eq!(
            (
                deleted.id,
                deleted.name.as_str(),
                deleted.documents_detached
            ),
            (t.id, "Guarantee", 1),
            "the result names the tag and counts the documents it was taken off"
        );
        let unused = create_tag(&ctx, "Unused", None).await.unwrap();
        assert_eq!(
            delete_tag(&ctx, unused.id)
                .await
                .unwrap()
                .documents_detached,
            0
        );
        assert!(list_taxonomy(&ctx).await.unwrap().tags.is_empty());
        let (links,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM document_tags")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(links, 0, "deleting a tag unlinks it from documents");
        assert_eq!(
            delete_tag(&ctx, t.id).await.unwrap_err().kind,
            ErrorKind::NotFound
        );
    }

    // --- types -----------------------------------------------------------

    #[tokio::test]
    async fn create_type_validates_key_and_fields() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let t = create_type(
            &ctx,
            NewType {
                key: "pet_passport".into(),
                labels: labels("Pet passport"),
                fields: vec![field("chip_number", FieldKind::Identifier)],
            },
        )
        .await
        .unwrap();
        assert!(!t.builtin);
        assert_eq!(t.fields.len(), 1);

        let dup = create_type(
            &ctx,
            NewType {
                key: "pet_passport".into(),
                labels: labels("x"),
                fields: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            (dup.kind, dup.code),
            (ErrorKind::Conflict, "type_key_taken")
        );

        for bad_key in ["Pet", "1pet", "pet passport", ""] {
            let err = create_type(
                &ctx,
                NewType {
                    key: bad_key.into(),
                    labels: labels("x"),
                    fields: vec![],
                },
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, "invalid_type_key", "{bad_key:?}");
        }
        let err = create_type(
            &ctx,
            NewType {
                key: "twice".into(),
                labels: labels("x"),
                fields: vec![field("a", FieldKind::Text), field("a", FieldKind::Date)],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "duplicate_field_key");

        let mut bad_validator = field("a", FieldKind::Text);
        bad_validator.validator = Some("inn".into());
        let err = create_type(
            &ctx,
            NewType {
                key: "validated".into(),
                labels: labels("x"),
                fields: vec![bad_validator],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "invalid_field",
            "validators only apply to identifiers"
        );
        let mut unknown_validator = field("a", FieldKind::Identifier);
        unknown_validator.validator = Some("nope".into());
        let err = create_type(
            &ctx,
            NewType {
                key: "validated".into(),
                labels: labels("x"),
                fields: vec![unknown_validator],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "invalid_field");
    }

    #[tokio::test]
    async fn the_issuer_flag_belongs_to_text_fields_only() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let mut issuer = field("issued_by_office", FieldKind::Text);
        issuer.issuer = true;
        let ok = create_type(
            &ctx,
            NewType {
                key: "licence".into(),
                labels: labels("Licence"),
                fields: vec![issuer.clone()],
            },
        )
        .await
        .unwrap();
        assert!(ok.fields[0].issuer, "a user type can flag its issuer field");

        issuer.kind = FieldKind::Date;
        let err = create_type(
            &ctx,
            NewType {
                key: "licence_two".into(),
                labels: labels("x"),
                fields: vec![issuer],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "invalid_field");
    }

    #[tokio::test]
    async fn removing_a_field_keeps_stored_values() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let t = create_type(
            &ctx,
            NewType {
                key: "custom".into(),
                labels: labels("Custom"),
                fields: vec![field("a", FieldKind::Text), field("b", FieldKind::Text)],
            },
        )
        .await
        .unwrap();
        let doc = insert_doc(&app, None).await;
        sqlx::query("UPDATE documents SET type_id = ? WHERE id = ?")
            .bind(t.id)
            .bind(doc)
            .execute(&app.db.write)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO field_values (document_id, field_key, value, provenance, updated_at) \
             VALUES (?, 'b', '{\"kind\":\"text\",\"value\":\"kept\"}', 'manual', '2026-01-01T00:00:00.000Z')",
        )
        .bind(doc)
        .execute(&app.db.write)
        .await
        .unwrap();

        let updated = update_type(
            &ctx,
            t.id,
            TypePatch {
                labels: None,
                fields: Some(vec![field("a", FieldKind::Text)]),
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.fields.len(), 1);
        assert_eq!(updated.document_count, 1);

        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM field_values WHERE field_key = 'b'")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(n, 1, "orphaned values stay");
    }

    #[tokio::test]
    async fn counts_exclude_trashed_documents() {
        let (_dir, app) = setup().await;
        let ctx = app.ctx(Actor::cli());
        let p = create_person(&ctx, person("A", &[])).await.unwrap();
        let live = insert_doc(&app, None).await;
        let trashed = insert_doc(&app, None).await;
        link_person_to_doc(&app, live, p.id).await;
        link_person_to_doc(&app, trashed, p.id).await;
        sqlx::query("UPDATE documents SET trashed_at = '2026-01-02T00:00:00.000Z' WHERE id = ?")
            .bind(trashed)
            .execute(&app.db.write)
            .await
            .unwrap();
        let tax = list_taxonomy(&ctx).await.unwrap();
        assert_eq!(tax.people[0].document_count, 1);
    }
}
