//! Append-only audit log: who changed what, through which door.

use serde::Serialize;
use serde_json::Value;
use sqlx::SqliteConnection;

use crate::error::{Result, ServiceError};
use crate::ids::DocumentId;
use crate::service::{Actor, Ctx};
use crate::time;

/// One audit log entry for a document.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DocumentEvent {
    pub id: i64,
    pub action: String,
    pub via: String,
    pub at: String,
    pub actor_name: Option<String>,
    #[schema(value_type = Object)]
    pub diff: Value,
}

/// Records one event inside the caller's transaction. Never put document
/// text, passwords or tokens in `diff`.
pub async fn record(
    tx: &mut SqliteConnection,
    actor: &Actor,
    action: &str,
    document_id: Option<DocumentId>,
    diff: Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO events (actor_user_id, via, action, document_id, at, diff) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(actor.user_id)
    .bind(actor.via.as_str())
    .bind(action)
    .bind(document_id)
    .bind(time::now_ts())
    .bind(diff.to_string())
    .execute(tx)
    .await?;
    Ok(())
}

/// Recent audit events of a document, newest first.
pub async fn list_for_document(
    ctx: &Ctx<'_>,
    document_id: DocumentId,
    limit: i64,
) -> Result<Vec<DocumentEvent>> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM documents WHERE id = ?")
        .bind(document_id)
        .fetch_optional(&ctx.app.db.read)
        .await?;
    if exists.is_none() {
        return Err(ServiceError::not_found(
            "document_not_found",
            "no such document",
        ));
    }

    let limit = limit.clamp(1, 100);
    let rows: Vec<(i64, String, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT e.id, e.action, e.via, e.at, u.display_name, e.diff \
         FROM events e \
         LEFT JOIN users u ON u.id = e.actor_user_id \
         WHERE e.document_id = ? \
         ORDER BY e.id DESC \
         LIMIT ?",
    )
    .bind(document_id)
    .bind(limit)
    .fetch_all(&ctx.app.db.read)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, action, via, at, actor_name, diff)| DocumentEvent {
            id,
            action,
            via,
            at,
            actor_name,
            diff: serde_json::from_str::<Value>(&diff).unwrap_or_else(|_| serde_json::json!({})),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::user::NewUser;
    use crate::service::Via;
    use crate::service::testing::{insert_document, test_app};
    use crate::service::users::create_user;
    use secrecy::SecretString;
    use serde_json::json;

    #[tokio::test]
    async fn record_writes_a_row() {
        let (_dir, app) = test_app().await;
        let doc = DocumentId::new();
        let mut tx = app.db.write.begin().await.unwrap();
        record(
            &mut tx,
            &Actor::cli(),
            "document.update",
            Some(doc),
            json!({"title": "x"}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let (via, action, document_id, diff): (String, String, Option<String>, String) =
            sqlx::query_as("SELECT via, action, document_id, diff FROM events")
                .fetch_one(&app.db.read)
                .await
                .unwrap();
        assert_eq!(via, Via::Cli.as_str());
        assert_eq!(action, "document.update");
        assert_eq!(document_id, Some(doc.to_string()));
        assert_eq!(
            serde_json::from_str::<Value>(&diff).unwrap(),
            json!({"title": "x"})
        );
    }

    #[tokio::test]
    async fn list_for_document_returns_newest_first_with_actor_name() {
        let (_dir, app) = test_app().await;
        let user = create_user(
            &app.ctx(Actor::cli()),
            NewUser {
                username: "alice".into(),
                display_name: "Alice Smith".into(),
                password: SecretString::from("correct horse".to_string()),
                locale: "en".into(),
                is_admin: false,
            },
        )
        .await
        .unwrap();
        let doc = insert_document(&app).await;
        let other = insert_document(&app).await;

        let mut tx = app.db.write.begin().await.unwrap();
        record(
            &mut tx,
            &Actor::user(user.id, Via::Web),
            "document.upload",
            Some(doc),
            json!({"filename": "a.pdf"}),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            &Actor::system(),
            "document.upload",
            Some(other),
            json!({}),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            &Actor::system(),
            "document.process",
            Some(doc),
            json!({"pages": 1}),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            &Actor::user(user.id, Via::Token),
            "document.update",
            Some(doc),
            json!({"title": "Lease"}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let events = list_for_document(&app.ctx(Actor::cli()), doc, 50)
            .await
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(events[0].id > events[1].id && events[1].id > events[2].id);

        assert_eq!(events[0].action, "document.update");
        assert_eq!(events[0].via, "token");
        assert_eq!(events[0].actor_name.as_deref(), Some("Alice Smith"));
        assert_eq!(events[0].diff, json!({"title": "Lease"}));

        assert_eq!(events[1].action, "document.process");
        assert_eq!(events[1].via, "system");
        assert_eq!(events[1].actor_name, None);
        assert_eq!(events[1].diff, json!({"pages": 1}));

        assert_eq!(events[2].action, "document.upload");
        assert_eq!(events[2].via, "web");
        assert_eq!(events[2].actor_name.as_deref(), Some("Alice Smith"));
        assert_eq!(events[2].diff, json!({"filename": "a.pdf"}));
    }

    #[tokio::test]
    async fn list_for_document_missing_document_returns_not_found() {
        let (_dir, app) = test_app().await;
        let err = list_for_document(&app.ctx(Actor::cli()), DocumentId::new(), 50)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
        assert_eq!(err.code, "document_not_found");
    }

    #[tokio::test]
    async fn list_for_document_clamps_limit() {
        let (_dir, app) = test_app().await;
        let doc = insert_document(&app).await;

        let mut tx = app.db.write.begin().await.unwrap();
        for i in 0..105 {
            record(
                &mut tx,
                &Actor::system(),
                "document.update",
                Some(doc),
                json!({"n": i}),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let ctx = app.ctx(Actor::cli());
        let min_zero = list_for_document(&ctx, doc, 0).await.unwrap();
        assert_eq!(min_zero.len(), 1);
        assert_eq!(min_zero[0].diff, json!({"n": 104}));

        let min_negative = list_for_document(&ctx, doc, -5).await.unwrap();
        assert_eq!(min_negative.len(), 1);
        assert_eq!(min_negative[0].diff, json!({"n": 104}));

        let max_clamped = list_for_document(&ctx, doc, 200).await.unwrap();
        assert_eq!(max_clamped.len(), 100);
        assert_eq!(max_clamped[0].diff, json!({"n": 104}));
        assert_eq!(max_clamped[99].diff, json!({"n": 5}));
    }
}
