//! Numbers for the dashboard and `get_stats`.

use std::collections::BTreeMap;

use chrono::{Datelike, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::error::Result;
use crate::jobs::queue::QueueCounts;
use crate::service::Ctx;
use crate::time;

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    /// Documents that are not in the trash.
    pub documents: i64,
    pub by_status: BTreeMap<String, i64>,
    /// Documents with at least one attention flag.
    pub attention: i64,
    pub queue: QueueCounts,
    /// LLM spend in the current UTC month, as a decimal string.
    pub llm_month_usd: String,
}

pub async fn get_stats(ctx: &Ctx<'_>) -> Result<Stats> {
    let pool = &ctx.app.db.read;
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM documents WHERE trashed_at IS NULL GROUP BY status",
    )
    .fetch_all(pool)
    .await?;
    let documents = rows.iter().map(|r| r.1).sum();
    let attention: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM documents WHERE trashed_at IS NULL AND attention != '[]'",
    )
    .fetch_one(pool)
    .await?;
    Ok(Stats {
        documents,
        by_status: rows.into_iter().collect(),
        attention,
        queue: ctx.app.jobs.counts().await?,
        llm_month_usd: llm_month_usd(ctx).await?.to_string(),
    })
}

/// Sum of `llm_usage.cost_usd` since the start of the current UTC month.
pub async fn llm_month_usd(ctx: &Ctx<'_>) -> Result<Decimal> {
    let now = Utc::now();
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now);
    let costs: Vec<String> =
        sqlx::query_scalar("SELECT cost_usd FROM llm_usage WHERE created_at >= ?")
            .bind(time::ts(start))
            .fetch_all(&ctx.app.db.read)
            .await?;
    Ok(costs.iter().filter_map(|c| c.parse::<Decimal>().ok()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Actor;
    use crate::service::testing::{insert_document, test_app};

    #[tokio::test]
    async fn counts_documents_by_status_and_attention() {
        let (_dir, app) = test_app().await;
        let a = insert_document(&app).await;
        insert_document(&app).await;
        sqlx::query(
            "UPDATE documents SET status = 'ready', attention = '[\"llm_failed\"]' WHERE id = ?",
        )
        .bind(a)
        .execute(&app.db.write)
        .await
        .unwrap();
        let trashed = insert_document(&app).await;
        sqlx::query("UPDATE documents SET trashed_at = '2026-01-01T00:00:00Z' WHERE id = ?")
            .bind(trashed)
            .execute(&app.db.write)
            .await
            .unwrap();

        let stats = get_stats(&app.ctx(Actor::system())).await.unwrap();
        assert_eq!(stats.documents, 2);
        assert_eq!(stats.by_status["ready"], 1);
        assert_eq!(stats.by_status["queued"], 1);
        assert_eq!(stats.attention, 1);
        assert_eq!(stats.llm_month_usd, "0");
    }

    #[tokio::test]
    async fn month_spend_sums_this_months_usage_only() {
        let (_dir, app) = test_app().await;
        let now = time::now_ts();
        for (cost, at) in [
            ("0.10", now.as_str()),
            ("0.25", now.as_str()),
            ("9.00", "2020-01-01T00:00:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO llm_usage (model, step, input_tokens, output_tokens, cost_usd, created_at) \
                 VALUES ('m', 's', 1, 1, ?, ?)",
            )
            .bind(cost)
            .bind(at)
            .execute(&app.db.write)
            .await
            .unwrap();
        }
        let spent = llm_month_usd(&app.ctx(Actor::system())).await.unwrap();
        assert_eq!(spent.to_string(), "0.35");
    }
}
