//! Linking people to documents by name, with no LLM.
//!
//! The same matching runs in two places: while a document is processed (one
//! document), and in the relink job after the people changed (every document
//! already in the archive). Both leave alone what a user decided by hand.
//!
//! An identity document also names its holder in fields, and a holder nobody
//! has added yet becomes a person ([`link_holder`]): a household archive is
//! about people, and the model can only pick people that already exist.

use serde_json::json;
use sqlx::SqliteConnection;

use crate::error::Result;
use crate::ids::{DocumentId, PersonId};
use crate::matching;
use crate::model::fields::FieldValue;
use crate::pipeline::checks;
use crate::search::sync;
use crate::service::documents::{Layer, best_layer};
use crate::service::{Actor, App, events};
use crate::time;

/// A name followed by its aliases, as the matcher wants them.
pub(super) fn names_and_aliases(name: String, aliases: &str) -> Vec<String> {
    let mut all = vec![name];
    all.extend(serde_json::from_str::<Vec<String>>(aliases).unwrap_or_default());
    all
}

/// Every person with the names they can be recognised by.
async fn candidates(conn: &mut SqliteConnection) -> Result<Vec<(PersonId, Vec<String>)>> {
    let rows: Vec<(PersonId, String, String)> =
        sqlx::query_as("SELECT id, name, aliases FROM people")
            .fetch_all(conn)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, aliases)| (id, names_and_aliases(name, &aliases)))
        .collect())
}

/// Whether a user (or an agent) set the people of this document: a stored
/// choice, which may well be the empty list, or any link made by hand.
async fn decided_by_hand(conn: &mut SqliteConnection, id: DocumentId) -> Result<bool> {
    let decided: Option<bool> = sqlx::query_scalar(
        "SELECT people_provenance = 'manual' OR EXISTS ( \
             SELECT 1 FROM document_people \
             WHERE document_id = documents.id AND provenance = 'manual') \
         FROM documents WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(decided.unwrap_or(false))
}

/// Replaces the links found by name (`local`, `mrz`) with the people named in
/// `text`. Links from the LLM and links made by hand stay. Returns whether
/// the set of people found by name changed. Does nothing for a document whose
/// people were set by hand.
async fn link_by_name(
    conn: &mut SqliteConnection,
    id: DocumentId,
    text: &str,
    candidates: &[(PersonId, Vec<String>)],
) -> Result<bool> {
    if decided_by_hand(conn, id).await? {
        return Ok(false);
    }
    let linked: Vec<(PersonId, String)> =
        sqlx::query_as("SELECT person_id, provenance FROM document_people WHERE document_id = ?")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    let by_name = |p: &str| matches!(p, "local" | "mrz");
    let mut current: Vec<PersonId> = linked
        .iter()
        .filter(|(_, p)| by_name(p))
        .map(|(person, _)| *person)
        .collect();
    let mut wanted: Vec<PersonId> = matching::find_people(text, candidates)
        .into_iter()
        .filter(|found| {
            !linked
                .iter()
                .any(|(person, p)| person == found && !by_name(p))
        })
        .collect();
    current.sort();
    wanted.sort();
    if current == wanted {
        return Ok(false);
    }

    sqlx::query(
        "DELETE FROM document_people WHERE document_id = ? AND provenance IN ('local', 'mrz')",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    for person in wanted {
        sqlx::query(
            "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) VALUES (?, ?, 'local')",
        )
        .bind(id)
        .bind(person)
        .execute(&mut *conn)
        .await?;
    }
    Ok(true)
}

/// Types whose holder is the person the document is about. Other documents
/// that carry names (a birth certificate's parents, a contract's signatories,
/// a diploma's holder on a form) never create people.
const HOLDER_TYPES: [&str; 5] = [
    "passport",
    "id_card",
    "travel_passport",
    "driver_license",
    "residence_permit",
];

/// Interface languages written in Cyrillic: a person created from a document
/// uploaded by one of their users is named in Cyrillic when the document
/// prints both spellings.
fn prefers_cyrillic(locale: &str) -> bool {
    matches!(
        locale.split(['-', '_']).next().unwrap_or(""),
        "ru" | "uk" | "be"
    )
}

/// A name field of the document and whether it was read from the
/// machine-readable zone. `None` for a value that is not plainly a name.
async fn name_field(
    conn: &mut SqliteConnection,
    id: DocumentId,
    key: &str,
) -> Result<Option<(String, bool)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT value, provenance FROM field_values WHERE document_id = ? AND field_key = ?",
    )
    .bind(id)
    .bind(key)
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|(value, provenance)| {
        let FieldValue::Text(text) = serde_json::from_str(&value).ok()? else {
            return None;
        };
        // A bilingual document prints both spellings ("TESTOVA / ТЕСТОВА"):
        // the first one names the holder, the text gives the other.
        let text = text.split('/').next().unwrap_or("").trim().to_string();
        let plausible = text.chars().filter(|c| c.is_alphabetic()).count() >= 2
            && text.chars().count() <= 80
            && text
                .chars()
                .all(|c| c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '.'));
        plausible.then_some((text, provenance == "mrz"))
    }))
}

/// The holder of an identity document, named in the form people write it.
struct Holder {
    /// Given names, then surname, or Surname Given Patronymic for Cyrillic.
    name: String,
    /// The same name in the other script, when the text prints it.
    other: Option<String>,
    /// Additional aliases for this holder (e.g. short form without patronymic).
    aliases: Vec<String>,
    /// Both names come from the machine-readable zone.
    from_mrz: bool,
}

fn is_patronymic_ending(word: &str) -> bool {
    let lower = word.to_lowercase();
    lower.ends_with("ович")
        || lower.ends_with("евич")
        || lower.ends_with("овна")
        || lower.ends_with("евна")
        || lower.ends_with("ична")
        || lower.ends_with("инична")
        || lower.ends_with("ич")
        || lower.ends_with("ыч")
}

/// The surname without the given names a misread zone ran into it ("TESTOVA
/// ANNA PETROVNA" next to the given names "ANNA PETROVNA"), so the person is
/// not named with them twice.
fn without_given_names(surname: &str, given: &str) -> String {
    let words: Vec<&str> = surname.split_whitespace().collect();
    let given: Vec<String> = given.split_whitespace().map(matching::normalize).collect();
    if given.is_empty() || words.len() <= given.len() {
        return surname.to_string();
    }
    let tail: Vec<String> = words[words.len() - given.len()..]
        .iter()
        .map(|w| matching::normalize(w))
        .collect();
    if tail == given {
        words[..words.len() - given.len()].join(" ")
    } else {
        surname.to_string()
    }
}

/// Reads the holder off the name fields, or `None` without both of them.
async fn holder_of(
    conn: &mut SqliteConnection,
    id: DocumentId,
    text: &str,
) -> Result<Option<Holder>> {
    let (Some((surname, s_mrz)), Some((given, g_mrz))) = (
        name_field(conn, id, "surname").await?,
        name_field(conn, id, "given_names").await?,
    ) else {
        return Ok(None);
    };
    let surname = without_given_names(&surname, &given);
    let surname_clean = matching::readable_name(&surname);
    let given_words: Vec<String> = given
        .split_whitespace()
        .map(matching::readable_name)
        .collect();
    let is_cyrillic = matching::cyrillic(&format!("{surname} {given}")) == Some(true);

    let (name, aliases, words) = if is_cyrillic
        && given_words.len() >= 2
        && is_patronymic_ending(given_words.last().unwrap())
    {
        let full_name = format!("{surname_clean} {}", given_words.join(" "));
        let short_alias = format!("{} {surname_clean}", given_words[0]);
        let natural_alias = format!("{} {surname_clean}", given_words.join(" "));
        let mut lookup_words = vec![surname_clean];
        lookup_words.extend(given_words);
        (full_name, vec![short_alias, natural_alias], lookup_words)
    } else {
        let mut all_words = given_words;
        all_words.push(surname_clean);
        let full_name = all_words.join(" ");
        (full_name, Vec::new(), all_words)
    };

    let others: Vec<Option<String>> = words
        .iter()
        .map(|w| matching::other_script_spelling(w, text).map(|o| matching::readable_name(&o)))
        .collect();
    let other = others
        .iter()
        .all(Option::is_some)
        .then(|| others.into_iter().flatten().collect::<Vec<_>>().join(" "));
    Ok(Some(Holder {
        name,
        other,
        aliases,
        from_mrz: s_mrz && g_mrz,
    }))
}

/// What [`link_holder`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HolderLink {
    /// The document is not an identity document, has people already, was
    /// decided by hand, or does not name its holder.
    None,
    /// The holder is an existing person.
    Linked,
    /// The holder was new: a person was created and linked.
    Created,
}

/// Links the holder of an identity document who has no person on the
/// document yet: the existing person with that name (in either script, see
/// `matching`), else a new person named in the form people write it, with the
/// other spelling as an alias so the next document of the same holder finds
/// them instead of adding a duplicate. `text` is the document's text.
///
/// A new person is created only with `may_create` (the run is over: the
/// model has had its say about the names) and never while the document
/// carries `mrz_mismatch`: a holder name the zone and the printed page
/// disagree about is for the user to confirm, not to become a person.
pub(super) async fn link_holder(
    conn: &mut SqliteConnection,
    id: DocumentId,
    text: &str,
    may_create: bool,
) -> Result<HolderLink> {
    let key: Option<String> = sqlx::query_scalar(
        "SELECT t.key FROM documents d JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if !key.is_some_and(|k| HOLDER_TYPES.contains(&k.as_str())) {
        return Ok(HolderLink::None);
    }
    let has_people: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM document_people WHERE document_id = ?)")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    if has_people || decided_by_hand(conn, id).await? {
        return Ok(HolderLink::None);
    }
    let Some(holder) = holder_of(conn, id, text).await? else {
        return Ok(HolderLink::None);
    };
    let provenance = if holder.from_mrz { "mrz" } else { "local" };

    let candidates = candidates(conn).await?;
    let (person, outcome) = match matching::find_person(&holder.name, &candidates).or_else(|| {
        holder
            .aliases
            .iter()
            .find_map(|a| matching::find_person(a, &candidates))
    }) {
        Some(person) => (person, HolderLink::Linked),
        None if !may_create || contested(conn, id).await? => return Ok(HolderLink::None),
        None => {
            let locale: Option<String> = sqlx::query_scalar(
                "SELECT u.locale FROM documents d JOIN users u ON u.id = d.added_by WHERE d.id = ?",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            let want_cyrillic = prefers_cyrillic(locale.as_deref().unwrap_or("en"));
            let (name, alias) = match holder.other {
                Some(other) if matching::cyrillic(&holder.name) != Some(want_cyrillic) => {
                    (other, Some(holder.name))
                }
                other => (holder.name, other),
            };
            let mut aliases = holder.aliases;
            if let Some(a) = alias.filter(|a| !aliases.contains(a) && a != &name) {
                aliases.push(a);
            }
            let person = PersonId::new();
            sqlx::query(
                "INSERT INTO people (id, name, aliases, notes, created_at) VALUES (?, ?, ?, '', ?)",
            )
            .bind(person)
            .bind(&name)
            .bind(json!(aliases).to_string())
            .bind(time::now_ts())
            .execute(&mut *conn)
            .await?;
            events::record(
                conn,
                &Actor::system(),
                "person.create",
                Some(id),
                json!({"id": person, "name": name, "from": "holder"}),
            )
            .await?;
            (person, HolderLink::Created)
        }
    };
    sqlx::query(
        "INSERT OR IGNORE INTO document_people (document_id, person_id, provenance) VALUES (?, ?, ?)",
    )
    .bind(id)
    .bind(person)
    .bind(provenance)
    .execute(conn)
    .await?;
    Ok(outcome)
}

/// The locale of the user who added the document prefers Cyrillic names.
async fn added_by_prefers_cyrillic(conn: &mut SqliteConnection, id: DocumentId) -> Result<bool> {
    let locale: Option<String> = sqlx::query_scalar(
        "SELECT u.locale FROM documents d JOIN users u ON u.id = d.added_by WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(prefers_cyrillic(locale.as_deref().unwrap_or("en")))
}

/// At the end of a run: the person an identity document names as its holder
/// takes the holder's name in the script the user reads, when paperjam named
/// that person itself from an earlier document printed in the other script
/// ("Ivan Testov" from a Romanian ID, then a Russian passport of the same
/// holder). The old name stays as an alias. A person a user created, renamed
/// or merged keeps their name. Returns the renamed person.
pub(super) async fn prefer_holder_script(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Option<PersonId>> {
    let key: Option<String> = sqlx::query_scalar(
        "SELECT t.key FROM documents d JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if !key.is_some_and(|k| HOLDER_TYPES.contains(&k.as_str())) {
        return Ok(None);
    }
    let text = stored_text(conn, id).await?;
    let Some(holder) = holder_of(conn, id, &text).await? else {
        return Ok(None);
    };
    let want_cyrillic = added_by_prefers_cyrillic(conn, id).await?;
    let in_script = |name: &str| matching::cyrillic(name) == Some(want_cyrillic);
    let preferred = if in_script(&holder.name) {
        holder.name.clone()
    } else {
        match &holder.other {
            Some(other) if in_script(other) => other.clone(),
            _ => return Ok(None),
        }
    };

    let rows: Vec<(PersonId, String, String)> = sqlx::query_as(
        "SELECT p.id, p.name, p.aliases FROM people p          JOIN document_people dp ON dp.person_id = p.id WHERE dp.document_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let list: Vec<(PersonId, Vec<String>)> = rows
        .iter()
        .map(|(p, name, aliases)| (*p, names_and_aliases(name.clone(), aliases)))
        .collect();
    let person = matching::find_person(&preferred, &list)
        .or_else(|| matching::find_person(&holder.name, &list))
        .or_else(|| (rows.len() == 1).then(|| rows[0].0));
    let Some(person) = person else {
        return Ok(None);
    };
    let Some((_, name, aliases)) = rows.iter().find(|(p, ..)| *p == person) else {
        return Ok(None);
    };
    if matching::cyrillic(name) != Some(!want_cyrillic) {
        return Ok(None);
    }
    let named_by_paperjam: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE action = 'person.create'                 AND json_extract(diff, '$.id') = ?1 AND json_extract(diff, '$.from') = 'holder')             AND NOT EXISTS (SELECT 1 FROM events WHERE action IN ('person.update', 'person.merge')                 AND (json_extract(diff, '$.id') = ?1 OR json_extract(diff, '$.into') = ?1))",
    )
    .bind(person.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if !named_by_paperjam {
        return Ok(None);
    }

    let mut all: Vec<String> = serde_json::from_str(aliases).unwrap_or_default();
    all.push(name.clone());
    if in_script(&holder.name) {
        all.extend(holder.aliases.iter().cloned());
    }
    let mut seen = std::collections::HashSet::new();
    all.retain(|a| a != &preferred && seen.insert(matching::normalize(a)));
    sqlx::query("UPDATE people SET name = ?, aliases = ? WHERE id = ?")
        .bind(&preferred)
        .bind(json!(all).to_string())
        .bind(person)
        .execute(&mut *conn)
        .await?;
    events::record(
        conn,
        &Actor::system(),
        "person.rename",
        Some(id),
        json!({"id": person, "name": preferred, "from": "holder"}),
    )
    .await?;
    Ok(Some(person))
}

/// Whether the zone and the model disagree about the document.
async fn contested(conn: &mut SqliteConnection, id: DocumentId) -> Result<bool> {
    let attention: Option<String> =
        sqlx::query_scalar("SELECT attention FROM documents WHERE id = ?")
            .bind(id)
            .fetch_optional(conn)
            .await?;
    Ok(attention
        .and_then(|a| serde_json::from_str::<Vec<String>>(&a).ok())
        .is_some_and(|flags| flags.iter().any(|f| f == checks::MRZ_MISMATCH)))
}

/// Matches the people against the text of a document being processed.
pub(super) async fn match_document(
    conn: &mut SqliteConnection,
    id: DocumentId,
    text: &str,
) -> Result<()> {
    let candidates = candidates(conn).await?;
    link_by_name(conn, id, text, &candidates).await?;
    Ok(())
}

/// The text the pipeline matched on: per page the best of the embedded and
/// OCR layers. The LLM transcription is left out, so the result is the one a
/// reprocess would reach.
async fn stored_text(conn: &mut SqliteConnection, id: DocumentId) -> Result<String> {
    let rows: Vec<(i64, String, String, Option<f64>)> = sqlx::query_as(
        "SELECT page, source, text, quality_score FROM text_layers \
         WHERE document_id = ? AND source <> 'llm' ORDER BY page",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    let mut pages: Vec<String> = Vec::new();
    let mut layers: Vec<Layer> = Vec::new();
    let mut current = None;
    for (page, source, text, score) in rows {
        if current != Some(page) {
            pages.extend(best_layer(&layers).map(|l| l.1.clone()));
            layers.clear();
            current = Some(page);
        }
        layers.push((source, text, score));
    }
    pages.extend(best_layer(&layers).map(|l| l.1.clone()));
    Ok(pages.join("\n"))
}

/// [`link_holder`] over the stored text of the document, for a processing
/// run: after the local steps (`may_create` false while the model's steps
/// still follow) and at its end.
pub(super) async fn link_holder_in(
    conn: &mut SqliteConnection,
    id: DocumentId,
    may_create: bool,
) -> Result<HolderLink> {
    let text = stored_text(conn, id).await?;
    link_holder(conn, id, &text, may_create).await
}

/// What a relink run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Relinked {
    /// Documents whose people were not set by hand and that have text.
    pub examined: u64,
    /// Documents whose links changed.
    pub changed: u64,
    /// People created from the holders of identity documents.
    pub created: u64,
}

/// The people linked to a document, in a stable order.
async fn linked(conn: &mut SqliteConnection, id: DocumentId) -> Result<Vec<(PersonId, String)>> {
    Ok(sqlx::query_as(
        "SELECT person_id, provenance FROM document_people WHERE document_id = ? ORDER BY person_id",
    )
    .bind(id)
    .fetch_all(conn)
    .await?)
}

/// Matches every person against every document that has text and whose
/// people nobody set by hand, adds the holders of identity documents who are
/// not people yet, and reindexes the documents that changed. Each document is
/// its own transaction, so a large archive never holds the database for long.
pub async fn relink_all(app: &App) -> Result<Relinked> {
    let mut candidates = {
        let mut conn = app.db.read.acquire().await?;
        candidates(&mut conn).await?
    };
    let ids: Vec<DocumentId> = sqlx::query_scalar(
        "SELECT d.id FROM documents d          WHERE d.people_provenance IS NOT 'manual'            AND NOT EXISTS (SELECT 1 FROM document_people dp                            WHERE dp.document_id = d.id AND dp.provenance = 'manual')            AND EXISTS (SELECT 1 FROM text_layers t WHERE t.document_id = d.id)          ORDER BY d.id",
    )
    .fetch_all(&app.db.read)
    .await?;

    let mut done = Relinked::default();
    for id in ids {
        let mut tx = app.db.write.begin().await?;
        let text = stored_text(&mut tx, id).await?;
        let before = linked(&mut tx, id).await?;
        link_by_name(&mut tx, id, &text, &candidates).await?;
        let holder = link_holder(&mut tx, id, &text, true).await?;
        let after = linked(&mut tx, id).await?;
        let created = holder == HolderLink::Created;
        if created {
            // The next document of the same holder must find this person.
            candidates = self::candidates(&mut tx).await?;
        }
        tx.commit().await?;
        done.examined += 1;
        done.created += u64::from(created);
        if before != after {
            done.changed += 1;
            sync::refresh(app, id).await;
        }
    }

    if done.changed > 0 {
        let mut tx = app.db.write.begin().await?;
        events::record(
            &mut tx,
            &Actor::system(),
            "people.relink",
            None,
            json!({"examined": done.examined, "changed": done.changed, "created": done.created}),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::without_given_names;

    #[test]
    fn given_names_run_into_the_surname_are_not_repeated() {
        assert_eq!(
            without_given_names("ЯТЕСТОВА АННА ПЕТРОВНА", "Анна Петровна"),
            "ЯТЕСТОВА"
        );
        // A double surname or one that merely shares a word stays whole.
        assert_eq!(
            without_given_names("TESTOV IVANOV", "IVAN"),
            "TESTOV IVANOV"
        );
        assert_eq!(without_given_names("ANNA", "ANNA"), "ANNA");
        assert_eq!(without_given_names("TESTOVA", ""), "TESTOVA");
    }
}
