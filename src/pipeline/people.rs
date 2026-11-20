//! Linking people to documents by name, with no LLM.
//!
//! The same matching runs in three places: while a document is processed
//! (one document), in the relink job after the people changed (every
//! document already in the archive), and in that job's startup run, which
//! also heals links lost to an older version or a restored backup. All of
//! them leave alone what a user decided by hand. The text matched against
//! is the pages' own text plus the values earlier runs already extracted
//! into fields and facts, so a name survives a mangled OCR line when a
//! field holds it.
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
pub(crate) fn is_holder_type(type_key: &str) -> bool {
    HOLDER_TYPES.contains(&type_key)
}

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

fn is_patronymic_word(word: &str) -> bool {
    is_patronymic_ending(word) || {
        let lower = word.to_lowercase();
        lower.ends_with("ovich")
            || lower.ends_with("evich")
            || lower.ends_with("ovna")
            || lower.ends_with("evna")
            || lower.ends_with("ichna")
            || lower.ends_with("inichna")
    }
}

/// Rough consonant skeleton of an already-normalized token for cross-script
/// spellings that differ by more than one edit (e.g. `Alexander`/`Александр`,
/// `Dmitry`/`Дмитрий`). Keeps a leading `+` when the name begins with a vowel.
fn consonant_skeleton(norm: &str) -> String {
    let mut out = String::with_capacity(norm.len());
    let mut chars = norm.chars().peekable();
    if let Some(&first) = chars.peek()
        && matches!(first, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
    {
        out.push('+');
    }
    let mut push_dedup = |c: char| {
        if !out.ends_with(c) {
            out.push(c);
        }
    };
    while let Some(c) = chars.next() {
        match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'h' => {}
            'p' if chars.peek() == Some(&'h') => {
                chars.next();
                push_dedup('f');
            }
            'x' => {
                push_dedup('k');
                push_dedup('s');
            }
            'c' | 'q' => push_dedup('k'),
            'w' => push_dedup('v'),
            other => push_dedup(other),
        }
    }
    out
}

fn words_compatible(a: &str, b: &str) -> bool {
    let na = matching::normalize(a);
    let nb = matching::normalize(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if matching::token_matches(&na, &nb) {
        return true;
    }
    let script_differs = match (matching::cyrillic(a), matching::cyrillic(b)) {
        (Some(ca), Some(cb)) => ca != cb,
        _ => false,
    };
    if !script_differs {
        return false;
    }
    let sa = consonant_skeleton(&na);
    let sb = consonant_skeleton(&nb);
    let consonants = |s: &str| s.chars().filter(|c| *c != '+').count();
    consonants(&sa) >= 3 && sa == sb
}

fn split_name_words(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `person_names` (`[name, aliases...]`) can represent an identity
/// document's extracted `surname` and/or `given_names`. Rejects family members
/// or relatives who share only a surname or patronymic when at least one name
/// field disagrees cleanly.
pub(crate) fn person_matches_holder(
    person_names: &[String],
    surname: Option<&str>,
    given_names: Option<&str>,
) -> bool {
    let surname_clean = surname.map(|s| match given_names {
        Some(g) => without_given_names(s, g),
        None => s.to_string(),
    });
    let surname_words: Vec<&str> = surname_clean
        .as_deref()
        .map(split_name_words)
        .unwrap_or_default();
    let mut given_words: Vec<&str> = given_names.map(split_name_words).unwrap_or_default();
    if given_words.len() >= 2 && is_patronymic_word(given_words[given_words.len() - 1]) {
        given_words.pop();
    }
    if surname_words.is_empty() && given_words.is_empty() {
        return true;
    }
    person_names.iter().any(|alias| {
        let raw = split_name_words(alias);
        if raw.is_empty() {
            return false;
        }
        let mut core = if raw.len() >= 3 {
            let non_pat: Vec<&str> = raw
                .iter()
                .copied()
                .filter(|w| !is_patronymic_word(w))
                .collect();
            if non_pat.len() >= 2 { non_pat } else { raw }
        } else {
            raw
        };
        if !surname_words.is_empty() {
            let matched = core
                .iter()
                .position(|aw| surname_words.iter().any(|sw| words_compatible(sw, aw)));
            if core.len() >= 2 {
                let Some(idx) = matched else {
                    return false;
                };
                core.remove(idx);
            } else if let Some(idx) = matched {
                core.remove(idx);
            }
        }
        if !given_words.is_empty() {
            if core.is_empty() {
                return false;
            }
            if !given_words
                .iter()
                .any(|gw| core.iter().any(|aw| words_compatible(gw, aw)))
            {
                return false;
            }
        }
        true
    })
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
async fn preferred_holder_name(
    conn: &mut SqliteConnection,
    id: DocumentId,
    holder: &Holder,
) -> Result<(String, Option<String>)> {
    let want_cyrillic = added_by_prefers_cyrillic(conn, id).await?;
    Ok(match &holder.other {
        Some(other) if matching::cyrillic(&holder.name) != Some(want_cyrillic) => {
            (other.clone(), Some(holder.name.clone()))
        }
        other => (holder.name.clone(), other.clone()),
    })
}

/// Removes non-manual links on identity documents whose person contradicts the
/// extracted `surname` / `given_names` fields (for instance a relative sharing a
/// surname or patronymic picked during classification). Returns the removed
/// person entries so downstream steps can also reconcile an auto-generated title.
pub(crate) async fn prune_mismatched_holder_people(
    conn: &mut SqliteConnection,
    id: DocumentId,
) -> Result<Vec<(PersonId, Vec<String>)>> {
    let key: Option<String> = sqlx::query_scalar(
        "SELECT t.key FROM documents d JOIN document_types t ON t.id = d.type_id WHERE d.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if !key.is_some_and(|k| is_holder_type(&k)) || decided_by_hand(conn, id).await? {
        return Ok(Vec::new());
    }
    let surname = name_field(conn, id, "surname").await?.map(|(v, _)| v);
    let given = name_field(conn, id, "given_names").await?.map(|(v, _)| v);
    if surname.is_none() && given.is_none() {
        return Ok(Vec::new());
    }
    let rows: Vec<(PersonId, String, String, String)> = sqlx::query_as(
        "SELECT dp.person_id, dp.provenance, p.name, p.aliases \
         FROM document_people dp JOIN people p ON p.id = dp.person_id \
         WHERE dp.document_id = ? AND dp.provenance <> 'manual'",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut removed = Vec::new();
    for (person_id, provenance, name, aliases) in rows {
        let names = names_and_aliases(name, &aliases);
        if !person_matches_holder(&names, surname.as_deref(), given.as_deref()) {
            sqlx::query(
                "DELETE FROM document_people WHERE document_id = ? AND person_id = ? AND provenance = ?",
            )
            .bind(id)
            .bind(person_id)
            .bind(provenance)
            .execute(&mut *conn)
            .await?;
            removed.push((person_id, names));
        }
    }
    Ok(removed)
}

/// If `title_head` (an identity document's title before `, YYYY`) names a
/// mismatching listed person (either pruned above or present in `people`),
/// returns the extracted holder's formatted name to replace it.
pub(crate) async fn reconcile_holder_title(
    conn: &mut SqliteConnection,
    id: DocumentId,
    title_head: &str,
    removed: &[(PersonId, Vec<String>)],
) -> Result<Option<String>> {
    if contested(conn, id).await? {
        return Ok(None);
    }
    let surname = name_field(conn, id, "surname").await?.map(|(v, _)| v);
    let given = name_field(conn, id, "given_names").await?.map(|(v, _)| v);
    if surname.is_none() && given.is_none() {
        return Ok(None);
    }
    if person_matches_holder(
        &[title_head.to_string()],
        surname.as_deref(),
        given.as_deref(),
    ) {
        return Ok(None);
    }
    let wrong: Vec<(PersonId, Vec<String>)> = if !removed.is_empty() {
        removed.to_vec()
    } else {
        candidates(conn)
            .await?
            .into_iter()
            .filter(|(_, names)| {
                !person_matches_holder(names, surname.as_deref(), given.as_deref())
            })
            .collect()
    };
    if wrong.is_empty() || matching::find_person(title_head, &wrong).is_none() {
        return Ok(None);
    }
    let text = stored_text(conn, id).await?;
    if let Some(holder) = holder_of(conn, id, &text).await? {
        let (preferred, _) = preferred_holder_name(conn, id, &holder).await?;
        return Ok(Some(preferred));
    }
    Ok(given.map(|g| matching::readable_name(&g)))
}

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
    if !key.is_some_and(|k| is_holder_type(&k)) || decided_by_hand(conn, id).await? {
        return Ok(HolderLink::None);
    }
    prune_mismatched_holder_people(conn, id).await?;
    let has_people: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM document_people WHERE document_id = ?)")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    if has_people {
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
            let (name, alias) = preferred_holder_name(conn, id, &holder).await?;
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

/// The string form of a field value for name matching: names and
/// identifiers carry one; a date, a number or an amount matches nothing.
fn value_text(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(t) | FieldValue::Identifier(t) => t.clone(),
        FieldValue::List(items) => items.join(" "),
        _ => String::new(),
    }
}

/// `base` with the values already extracted into the document's fields and
/// facts appended, so a name a scan's OCR mangled still matches: the
/// pipeline read it into a field (a birth certificate's `father`, a lease's
/// `permitted_resident`) or a fact (the `spouses` of a marriage), and the
/// stored value names the person even when the printed line does not.
/// Nothing here calls the model; only what earlier runs extracted is read.
async fn with_extracted_values(
    conn: &mut SqliteConnection,
    id: DocumentId,
    mut base: String,
) -> Result<String> {
    let values: Vec<(String,)> =
        sqlx::query_as("SELECT value FROM field_values WHERE document_id = ?")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    for (value,) in values {
        if let Ok(value) = serde_json::from_str::<FieldValue>(&value) {
            let text = value_text(&value);
            if !text.is_empty() {
                base.push('\n');
                base.push_str(&text);
            }
        }
    }
    let facts: Vec<(String,)> = sqlx::query_as("SELECT value FROM facts WHERE document_id = ?")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    for (value,) in facts {
        base.push('\n');
        base.push_str(&value);
    }
    Ok(base)
}

/// Matches the people against the text of a document being processed.
pub(super) async fn match_document(
    conn: &mut SqliteConnection,
    id: DocumentId,
    text: &str,
) -> Result<()> {
    let candidates = candidates(conn).await?;
    let text = with_extracted_values(conn, id, text.to_string()).await?;
    link_by_name(conn, id, &text, &candidates).await?;
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

/// The text a relink matches against: the stored text of the pages, with the
/// extracted field and fact values appended (see [`with_extracted_values`]).
async fn names_text(conn: &mut SqliteConnection, id: DocumentId) -> Result<String> {
    let text = stored_text(conn, id).await?;
    with_extracted_values(conn, id, text).await
}

/// [`link_holder`] over the stored text of the document, for a processing
/// run: after the local steps (`may_create` false while the model's steps
/// still follow) and at its end.
pub(super) async fn link_holder_in(
    conn: &mut SqliteConnection,
    id: DocumentId,
    may_create: bool,
) -> Result<HolderLink> {
    let text = names_text(conn, id).await?;
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
        let text = names_text(&mut tx, id).await?;
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
    use super::{person_matches_holder, without_given_names};

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

    #[test]
    fn holder_matching_distinguishes_relatives_sharing_surname_or_patronymic() {
        let relative = vec![
            "Тестов Петр Иванович".to_string(),
            "Петр Тестов".to_string(),
        ];
        // Father/son or child sharing surname and patronymic root.
        assert!(!person_matches_holder(
            &relative,
            Some("ТЕСТОВ"),
            Some("ИВАН СЕРГЕЕВИЧ"),
        ));
        assert!(!person_matches_holder(
            &relative,
            None,
            Some("ИВАН СЕРГЕЕВИЧ"),
        ));
        assert!(!person_matches_holder(
            &relative,
            Some("СИДОРОВ"),
            Some("ПЕТР ИВАНОВИЧ"),
        ));

        // Actual holder in same or opposite script.
        assert!(person_matches_holder(
            &relative,
            Some("ТЕСТОВ"),
            Some("ПЕТР СЕРГЕЕВИЧ"),
        ));
        assert!(person_matches_holder(
            &["Ivan Testov".to_string()],
            Some("ТЕСТОВ"),
            Some("ИВАН"),
        ));
        // Cross-script transliteration variants stay compatible.
        assert!(person_matches_holder(
            &["Alexander Ivanov".to_string()],
            Some("ИВАНОВ"),
            Some("АЛЕКСАНДР"),
        ));
        assert!(person_matches_holder(
            &["Dmitry Petrov".to_string()],
            Some("ПЕТРОВ"),
            Some("ДМИТРИЙ"),
        ));
    }
}
