//! The calendar feed: the due dates of the archive as an `.ics` subscription.
//!
//! Calendar services fetch the feed from their own servers, so it leaves the
//! house: it carries each document's title and a link to its page, never a
//! field value, and documents marked `local_only` are left out altogether.
//! The feed's address holds a secret that opens this feed and nothing else.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::json;

use crate::error::{Result, ServiceError};
use crate::hashing::sha256_hex;
use crate::ics::{self, Event};
use crate::ids::{DocumentId, UserId};
use crate::model::due;
use crate::model::fields::{FieldValue, Labels};
use crate::service::{App, Ctx, events, links};
use crate::{random, time};

const SECRET_PREFIX: &str = "pjc_";
const SECRET_RANDOM_CHARS: usize = 32;
/// `last_used_at` is refreshed at most this often.
const LAST_USED_GRANULARITY: Duration = Duration::minutes(10);
const CALENDAR_NAME: &str = "paperjam";

/// A user's calendar subscription as shown in settings. The secret is not
/// part of it: it exists in clear only in the response that made it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct FeedView {
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// Creates the acting user's feed, or replaces its secret (the old link stops
/// working at once). Returns the view and the secret, which is never
/// retrievable again.
pub async fn create_or_rotate(ctx: &Ctx<'_>) -> Result<(FeedView, String)> {
    let user_id = ctx.actor.require_user()?;
    let secret = format!("{SECRET_PREFIX}{}", random::base62(SECRET_RANDOM_CHARS));
    let view = FeedView {
        created_at: time::now_ts(),
        last_used_at: None,
    };

    let mut tx = ctx.app.db.write.begin().await?;
    sqlx::query(
        "INSERT INTO calendar_feeds (user_id, secret_hash, created_at) VALUES (?, ?, ?) \
         ON CONFLICT (user_id) DO UPDATE SET \
         secret_hash = excluded.secret_hash, created_at = excluded.created_at, last_used_at = NULL",
    )
    .bind(user_id)
    .bind(sha256_hex(secret.as_bytes()))
    .bind(&view.created_at)
    .execute(&mut *tx)
    .await?;
    events::record(&mut tx, &ctx.actor, "calendar_feed.create", None, json!({})).await?;
    tx.commit().await?;
    Ok((view, secret))
}

pub async fn revoke(ctx: &Ctx<'_>) -> Result<()> {
    let user_id = ctx.actor.require_user()?;
    let mut tx = ctx.app.db.write.begin().await?;
    let deleted = sqlx::query("DELETE FROM calendar_feeds WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ServiceError::not_found(
            "calendar_feed_not_found",
            "there is no calendar feed to revoke",
        ));
    }
    events::record(&mut tx, &ctx.actor, "calendar_feed.revoke", None, json!({})).await?;
    tx.commit().await?;
    Ok(())
}

/// The acting user's feed, or `None` when they have not made one.
pub async fn get(ctx: &Ctx<'_>) -> Result<Option<FeedView>> {
    let user_id = ctx.actor.require_user()?;
    Ok(
        sqlx::query_as("SELECT created_at, last_used_at FROM calendar_feeds WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&ctx.app.db.read)
            .await?,
    )
}

/// The address of the feed: `/calendar/<secret>.ics` under the public URL
/// when one is configured, else just the path.
pub fn feed_url(app: &App, secret: &str) -> (String, Option<String>) {
    let path = format!("/calendar/{secret}.ics");
    let url = app
        .config
        .public_url
        .is_some()
        .then(|| format!("{}{path}", links::base_url(app, None)));
    (path, url)
}

fn not_found() -> ServiceError {
    ServiceError::not_found("calendar_feed_not_found", "no such calendar feed")
}

/// The owner of the feed whose secret this is. A secret that is unknown,
/// revoked, of a disabled user or not shaped like one is just "not found".
pub async fn open(app: &App, secret: &str) -> Result<UserId> {
    let shaped = secret.strip_prefix(SECRET_PREFIX).is_some_and(|rest| {
        rest.len() == SECRET_RANDOM_CHARS && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    });
    if !shaped {
        return Err(not_found());
    }
    let hash = sha256_hex(secret.as_bytes());
    let row: Option<(UserId, Option<String>)> = sqlx::query_as(
        "SELECT f.user_id, f.last_used_at \
         FROM calendar_feeds f JOIN users u ON u.id = f.user_id \
         WHERE f.secret_hash = ? AND u.disabled_at IS NULL",
    )
    .bind(&hash)
    .fetch_optional(&app.db.read)
    .await?;
    let (user_id, last_used_at) = row.ok_or_else(not_found)?;

    let now = Utc::now();
    let stale = time::ts(now - LAST_USED_GRANULARITY);
    if last_used_at.is_none_or(|last| last < stale) {
        sqlx::query("UPDATE calendar_feeds SET last_used_at = ? WHERE user_id = ?")
            .bind(time::ts(now))
            .bind(user_id)
            .execute(&app.db.write)
            .await?;
    }
    Ok(user_id)
}

/// What the event titles say, in the feed owner's language.
struct Words {
    untitled: &'static str,
    expires: &'static str,
    due: &'static str,
    renew: &'static str,
}

const EN: Words = Words {
    untitled: "Document",
    expires: "{title} expires",
    due: "{title} is due",
    renew: "Renew {title}",
};

const RU: Words = Words {
    untitled: "Документ",
    expires: "{title}: срок действия истекает",
    due: "{title}: пора заменить",
    renew: "Продлить: {title}",
};

fn words(locale: &str) -> &'static Words {
    if locale == "ru" { &RU } else { &EN }
}

/// Id, title, type key and type labels (JSON) of one document in the feed.
type FeedDocument = (DocumentId, Option<String>, Option<String>, Option<String>);

/// The feed of `user_id`: one all-day event on each due date, and one
/// `lead_days` before it unless that is 0 or that day has passed. Trashed and `local_only` documents
/// are not in it.
pub async fn render(app: &App, user_id: UserId, now: DateTime<Utc>) -> Result<String> {
    let (locale,): (String,) = sqlx::query_as("SELECT locale FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&app.db.read)
        .await?;
    let words = words(&locale);

    let documents: Vec<FeedDocument> = sqlx::query_as(
        "SELECT d.id, d.title, t.key, t.labels FROM documents d \
         LEFT JOIN document_types t ON t.id = d.type_id \
         WHERE d.trashed_at IS NULL AND d.local_only = 0 ORDER BY d.id",
    )
    .fetch_all(&app.db.read)
    .await?;
    let marks = vec!["?"; due::DUE_FIELD_KEYS.len()].join(", ");
    let sql = format!(
        "SELECT v.document_id, v.field_key, v.value FROM field_values v \
         JOIN documents d ON d.id = v.document_id \
         WHERE d.trashed_at IS NULL AND d.local_only = 0 AND v.field_key IN ({marks})"
    );
    let mut query = sqlx::query_as::<_, (DocumentId, String, String)>(sqlx::AssertSqlSafe(sql));
    for key in due::DUE_FIELD_KEYS {
        query = query.bind(key);
    }
    let rows = query.fetch_all(&app.db.read).await?;
    let mut fields: HashMap<DocumentId, HashMap<String, FieldValue>> = HashMap::new();
    for (id, key, value) in rows {
        if let Ok(value) = serde_json::from_str(&value) {
            fields.entry(id).or_default().insert(key, value);
        }
    }

    let lead = Duration::days(i64::from(app.runtime_settings().calendar.lead_days));
    let today = now.date_naive();
    let mut events = Vec::new();
    for (id, title, type_key, labels) in documents {
        let Some(own) = fields.get(&id) else { continue };
        let Some(date) = due::due_date(type_key.as_deref(), |key| own.get(key)) else {
            continue;
        };
        let label = labels
            .and_then(|l| serde_json::from_str::<Labels>(&l).ok())
            .and_then(|l| l.get(&locale).or_else(|| l.get("en")).cloned())
            .filter(|l| !l.trim().is_empty());
        let title = event_title(title.as_deref(), label.as_deref(), words.untitled);
        // A day printed on the document is the day it runs out — an expiry,
        // the end of a lease or a warranty. A payment day (an invoice's
        // `due_date`) and a passport replaced by age are simply "due".
        let runs_out = ["expiry_date", "end_date", "warranty_end_date"]
            .iter()
            .any(|k| matches!(own.get(*k), Some(FieldValue::Date(_))));
        let url = app
            .config
            .public_url
            .is_some()
            .then(|| format!("{}/documents/{id}", links::base_url(app, None)));
        let mut push = |kind: &str, date: NaiveDate, template: &str| {
            events.push(Event {
                uid: format!("{id}-{kind}@paperjam"),
                date,
                summary: template.replace("{title}", &title),
                url: url.clone(),
            });
        };
        push(
            "due",
            date,
            if runs_out { words.expires } else { words.due },
        );
        if !lead.is_zero()
            && let Some(renew) = date.checked_sub_signed(lead)
            && renew >= today
        {
            push("renew", renew, words.renew);
        }
    }
    events.sort_by(|a, b| (a.date, &a.uid).cmp(&(b.date, &b.uid)));
    Ok(ics::calendar(CALENDAR_NAME, &events, now))
}

/// What an event calls its document: the type and the title, since titles
/// leave the type out ("Загранпаспорт — Ivan Testov, 2026"), so two documents
/// of one holder stay apart. A title that already names the type stands alone.
fn event_title(title: Option<&str>, label: Option<&str>, untitled: &str) -> String {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    match (title, label) {
        (Some(title), Some(label)) if !title.to_lowercase().contains(&label.to_lowercase()) => {
            format!("{label} — {title}")
        }
        (Some(title), _) => title.to_string(),
        (None, Some(label)) => label.to_string(),
        (None, None) => untitled.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn an_event_names_the_type_next_to_the_title() {
        assert_eq!(
            event_title(Some("Ivan Testov, 2026"), Some("Загранпаспорт"), "Документ"),
            "Загранпаспорт — Ivan Testov, 2026"
        );
        assert_eq!(
            event_title(Some("Travel passport"), Some("Travel passport"), "Document"),
            "Travel passport"
        );
        assert_eq!(event_title(None, Some("Паспорт"), "Документ"), "Паспорт");
        assert_eq!(event_title(Some("  "), None, "Документ"), "Документ");
    }
    use crate::model::user::{NewUser, User};
    use crate::service::testing::TEST_SECRET;
    use crate::service::users::{create_user, disable_user};
    use crate::service::{Actor, Via, tokens};
    use chrono::TimeZone;
    use secrecy::SecretString;
    use serde_json::{Value, json};

    async fn app_with(vars: &[(&str, &str)]) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().into_owned();
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let config = Config::from_lookup(|name| match name {
            "PAPERJAM_SECRET_KEY" => Some(TEST_SECRET.to_string()),
            "PAPERJAM_DATA_DIR" => Some(data_dir.clone()),
            "PAPERJAM_OCR_LANGS" => Some("eng+rus+ron".to_string()),
            other => vars
                .iter()
                .find(|(k, _)| k == other)
                .map(|(_, v)| v.clone()),
        })
        .unwrap();
        let app = App::open(config).await.unwrap();
        crate::service::taxonomy::seed_builtin_types(&app.ctx(Actor::system()))
            .await
            .unwrap();
        (dir, app)
    }

    async fn user(app: &App, name: &str, locale: &str) -> User {
        create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                username: name.into(),
                display_name: name.into(),
                password: SecretString::from("correct horse".to_string()),
                locale: locale.into(),
                is_admin: false,
            },
        )
        .await
        .unwrap()
    }

    fn web(u: &User) -> Actor {
        Actor::user(u.id, Via::Web)
    }

    fn date(s: &str) -> Value {
        json!({"kind": "date", "value": s})
    }

    fn identifier(s: &str) -> Value {
        json!({"kind": "identifier", "value": s})
    }

    fn text(s: &str) -> Value {
        json!({"kind": "text", "value": s})
    }

    struct Doc<'a> {
        title: Option<&'a str>,
        type_key: Option<&'a str>,
        fields: Vec<(&'a str, Value)>,
        local_only: bool,
        trashed: bool,
    }

    fn doc<'a>(title: &'a str, fields: Vec<(&'a str, Value)>) -> Doc<'a> {
        Doc {
            title: Some(title),
            type_key: None,
            fields,
            local_only: false,
            trashed: false,
        }
    }

    async fn add(app: &App, d: Doc<'_>) -> DocumentId {
        let id = DocumentId::new();
        let now = time::now_ts();
        sqlx::query(
            "INSERT INTO documents (id, title, type_id, local_only, trashed_at, created_at, updated_at) \
             VALUES (?, ?, (SELECT id FROM document_types WHERE key = ?), ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(d.title)
        .bind(d.type_key)
        .bind(d.local_only)
        .bind(d.trashed.then(|| now.clone()))
        .bind(&now)
        .bind(&now)
        .execute(&app.db.write)
        .await
        .unwrap();
        for (key, value) in d.fields {
            set_field(app, id, key, value).await;
        }
        id
    }

    async fn set_field(app: &App, id: DocumentId, key: &str, value: Value) {
        sqlx::query(
            "INSERT OR REPLACE INTO field_values (document_id, field_key, value, provenance, updated_at) \
             VALUES (?, ?, ?, 'manual', ?)",
        )
        .bind(id)
        .bind(key)
        .bind(value.to_string())
        .bind(time::now_ts())
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 8, 30, 0).unwrap()
    }

    async fn feed(app: &App, u: &User) -> String {
        render(app, u.id, now()).await.unwrap()
    }

    #[tokio::test]
    async fn a_due_document_gets_a_due_event_and_a_renew_event() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let id = add(
            &app,
            doc("Travel passport", vec![("expiry_date", date("2031-02-03"))]),
        )
        .await;

        let text = feed(&app, &alice).await;
        assert!(text.contains(&format!("UID:{id}-due@paperjam\r\n")));
        assert!(text.contains("DTSTART;VALUE=DATE:20310203\r\n"));
        assert!(text.contains("SUMMARY:Travel passport expires\r\n"));
        assert!(text.contains(&format!("UID:{id}-renew@paperjam\r\n")));
        assert!(text.contains("DTSTART;VALUE=DATE:20301205\r\n"));
        assert!(text.contains("SUMMARY:Renew Travel passport\r\n"));
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 2);
    }

    async fn test_app_default() -> (tempfile::TempDir, App) {
        app_with(&[]).await
    }

    #[tokio::test]
    async fn zero_lead_days_turns_the_renew_event_off() {
        let (_dir, app) = app_with(&[("PAPERJAM_CALENDAR_LEAD_DAYS", "0")]).await;
        let alice = user(&app, "alice", "en").await;
        add(
            &app,
            doc("Lease", vec![("expiry_date", date("2031-02-03"))]),
        )
        .await;
        let text = feed(&app, &alice).await;
        assert_eq!(text.matches("BEGIN:VEVENT").count(), 1);
        assert!(!text.contains("-renew@paperjam"));
    }

    #[tokio::test]
    async fn a_renew_event_is_only_for_today_or_later() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        // Today is 2026-11-09 and the lead is 60 days.
        let past = add(&app, doc("Past", vec![("expiry_date", date("2026-12-01"))])).await;
        let today = add(
            &app,
            doc("Today", vec![("expiry_date", date("2027-01-08"))]),
        )
        .await;
        let lapsed = add(
            &app,
            doc("Lapsed", vec![("expiry_date", date("2026-10-01"))]),
        )
        .await;

        let text = feed(&app, &alice).await;
        for id in [past, today, lapsed] {
            assert!(text.contains(&format!("UID:{id}-due@paperjam\r\n")));
        }
        assert!(!text.contains(&format!("UID:{past}-renew@paperjam")));
        assert!(!text.contains(&format!("UID:{lapsed}-renew@paperjam")));
        assert!(text.contains(&format!("UID:{today}-renew@paperjam\r\n")));
        assert!(text.contains("DTSTART;VALUE=DATE:20261109\r\n"));
    }

    #[tokio::test]
    async fn a_contract_running_to_its_end_date_expires() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let mut d = doc("Lease", vec![("end_date", date("2031-02-03"))]);
        d.type_key = Some("rental_agreement");
        add(&app, d).await;
        let text = feed(&app, &alice).await;
        assert!(text.contains("SUMMARY:Rental Agreement — Lease expires\r\n"));
    }

    #[tokio::test]
    async fn an_invoice_due_day_is_not_an_expiry() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let mut d = doc("Invoice", vec![("due_date", date("2031-02-03"))]);
        d.type_key = Some("invoice");
        add(&app, d).await;
        let text = feed(&app, &alice).await;
        assert!(text.contains("SUMMARY:Invoice is due\r\n"));
    }

    #[tokio::test]
    async fn fields_the_due_date_does_not_read_do_not_matter() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let mut d = doc(
            "Lease",
            vec![
                ("expiry_date", date("2031-02-03")),
                ("notes", text("not a due field")),
            ],
        );
        d.type_key = Some("passport");
        add(&app, d).await;
        assert_eq!(feed(&app, &alice).await.matches("BEGIN:VEVENT").count(), 2);
    }

    #[tokio::test]
    async fn trashed_local_only_and_undated_documents_are_left_out() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let expiry = vec![("expiry_date", date("2031-02-03"))];
        add(&app, doc("Kept", expiry.clone())).await;
        add(
            &app,
            Doc {
                trashed: true,
                ..doc("Trashed one", expiry.clone())
            },
        )
        .await;
        add(
            &app,
            Doc {
                local_only: true,
                ..doc("Local one", expiry)
            },
        )
        .await;
        add(
            &app,
            doc("No date", vec![("document_number", identifier("N1"))]),
        )
        .await;

        let text = feed(&app, &alice).await;
        assert!(text.contains("Kept"));
        for absent in ["Trashed one", "Local one", "No date"] {
            assert!(!text.contains(absent), "{absent} is in the feed");
        }
    }

    #[tokio::test]
    async fn no_field_value_reaches_the_feed() {
        let (_dir, app) = app_with(&[("PAPERJAM_PUBLIC_URL", "https://docs.example.org")]).await;
        let alice = user(&app, "alice", "en").await;
        let id = add(
            &app,
            Doc {
                type_key: Some("travel_passport"),
                ..doc(
                    "Mine, again; v2",
                    vec![
                        ("expiry_date", date("2031-02-03")),
                        ("document_number", identifier("ZZ9876543")),
                        ("personal_number", identifier("PN-4455667788")),
                        ("surname", text("Placeholder")),
                    ],
                )
            },
        )
        .await;

        let text = feed(&app, &alice).await;
        for secret in ["ZZ9876543", "PN-4455667788", "Placeholder"] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        // The type goes in front of a title that does not name it.
        let summary = text
            .lines()
            .find(|l| l.starts_with("SUMMARY:") && l.contains("expires"))
            .unwrap();
        assert!(
            summary.ends_with(" — Mine\\, again\\; v2 expires"),
            "{summary}"
        );
        assert!(
            summary.to_lowercase().starts_with("summary:passport"),
            "{summary}"
        );
        assert!(text.contains(&format!("URL:https://docs.example.org/documents/{id}\r\n")));
    }

    #[tokio::test]
    async fn there_is_no_url_without_a_public_url() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        add(
            &app,
            doc("Lease", vec![("expiry_date", date("2031-02-03"))]),
        )
        .await;
        assert!(!feed(&app, &alice).await.contains("URL:"));
    }

    #[tokio::test]
    async fn uids_are_stable_and_a_changed_date_moves_the_event() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let id = add(
            &app,
            doc("Lease", vec![("expiry_date", date("2031-02-03"))]),
        )
        .await;
        let before = feed(&app, &alice).await;
        assert_eq!(before, feed(&app, &alice).await);

        set_field(&app, id, "expiry_date", date("2032-06-30")).await;
        let after = feed(&app, &alice).await;
        assert!(after.contains(&format!("UID:{id}-due@paperjam\r\n")));
        assert!(after.contains("DTSTART;VALUE=DATE:20320630\r\n"));
        assert!(!after.contains("20310203"));

        sqlx::query("UPDATE documents SET trashed_at = ? WHERE id = ?")
            .bind(time::now_ts())
            .bind(id)
            .execute(&app.db.write)
            .await
            .unwrap();
        assert!(!feed(&app, &alice).await.contains(&id.to_string()));
    }

    #[tokio::test]
    async fn the_text_follows_the_owners_language_and_a_replaced_passport_is_due() {
        let (_dir, app) = test_app_default().await;
        let olga = user(&app, "olga", "ru").await;
        add(
            &app,
            Doc {
                type_key: Some("passport"),
                ..doc(
                    "Паспорт",
                    vec![
                        ("birth_date", date("1990-05-17")),
                        ("issue_date", date("2010-05-17")),
                        ("nationality", text("RUS")),
                    ],
                )
            },
        )
        .await;
        add(&app, doc("", vec![("expiry_date", date("2031-02-03"))])).await;

        let text = feed(&app, &olga).await;
        assert!(
            text.contains("SUMMARY:Паспорт: пора заменить\r\n"),
            "{text}"
        );
        assert!(text.contains("DTSTART;VALUE=DATE:20350517\r\n"));
        assert!(text.contains("SUMMARY:Продлить: Паспорт\r\n"));
        assert!(text.contains("SUMMARY:Документ: срок действия истекает\r\n"));
        // Every physical line, folded Cyrillic included, fits 75 octets.
        assert!(text.split("\r\n").all(|l| l.len() <= 75));
    }

    #[tokio::test]
    async fn the_secret_is_shown_once_and_stored_hashed() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let ctx = app.ctx(web(&alice));
        assert!(get(&ctx).await.unwrap().is_none());

        let (view, secret) = create_or_rotate(&ctx).await.unwrap();
        assert!(secret.starts_with("pjc_"));
        assert_eq!(secret.len(), 4 + 32);
        let (hash,): (String,) = sqlx::query_as("SELECT secret_hash FROM calendar_feeds")
            .fetch_one(&app.db.read)
            .await
            .unwrap();
        assert_eq!(hash, sha256_hex(secret.as_bytes()));
        assert_ne!(hash, secret);
        assert_eq!(
            get(&ctx).await.unwrap().unwrap().created_at,
            view.created_at
        );
        assert!(!serde_json::to_string(&view).unwrap().contains(&secret));
        assert_eq!(open(&app, &secret).await.unwrap(), alice.id);
    }

    #[tokio::test]
    async fn rotating_revoking_and_disabling_close_the_link() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let bob = user(&app, "bob", "en").await;
        let ctx = app.ctx(web(&alice));

        let (_, first) = create_or_rotate(&ctx).await.unwrap();
        let (_, second) = create_or_rotate(&ctx).await.unwrap();
        assert_ne!(first, second);
        assert!(open(&app, &first).await.is_err());
        assert_eq!(open(&app, &second).await.unwrap(), alice.id);

        // Another user's revoke does not touch alice's feed.
        assert!(revoke(&app.ctx(web(&bob))).await.is_err());
        assert_eq!(open(&app, &second).await.unwrap(), alice.id);

        revoke(&ctx).await.unwrap();
        assert!(open(&app, &second).await.is_err());
        assert!(revoke(&ctx).await.is_err());

        let (_, third) = create_or_rotate(&ctx).await.unwrap();
        disable_user(&app.ctx(Actor::cli()), alice.id)
            .await
            .unwrap();
        assert!(open(&app, &third).await.is_err());
    }

    #[tokio::test]
    async fn an_api_token_is_not_a_calendar_secret_and_the_reverse() {
        let (_dir, app) = test_app_default().await;
        let alice = user(&app, "alice", "en").await;
        let ctx = app.ctx(web(&alice));
        let (_, token) = tokens::create_token(&ctx, "laptop").await.unwrap();
        let (_, secret) = create_or_rotate(&ctx).await.unwrap();

        assert!(open(&app, &token).await.is_err());
        assert!(tokens::resolve_token(&app, &secret).await.is_err());
        assert!(open(&app, "").await.is_err());
        assert!(open(&app, "pjc_short").await.is_err());
    }
}
