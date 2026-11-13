//! Append-only audit log: who changed what, through which door.

use serde_json::Value;
use sqlx::SqliteConnection;

use crate::error::Result;
use crate::ids::DocumentId;
use crate::service::Actor;
use crate::time;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Via;
    use crate::service::testing::test_app;
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
}
