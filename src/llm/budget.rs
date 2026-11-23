//! Spend accounting: every call is recorded in `llm_usage`; the optional
//! monthly budget (`PAPERJAM_LLM_MONTHLY_BUDGET_USD`) is checked before each
//! LLM step, and each call sets an estimate of its cost aside before it is
//! sent ([`reserve`]), so calls running at once (pages transcribed side by
//! side, several documents) cannot all pass the same check and overspend it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{Datelike, TimeZone, Utc};
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use crate::error::Result;
use crate::ids::DocumentId;
use crate::llm::Usage;
use crate::service::{Actor, App, stats};
use crate::time;

/// Set aside for a call when this month has no call of its step and model
/// to go by.
const DEFAULT_ESTIMATE_USD: Decimal = Decimal::from_parts(5, 0, 0, false, 2);

/// Spend set aside for LLM calls that are still running.
#[derive(Default)]
pub struct Reservations {
    held: Mutex<Decimal>,
}

impl Reservations {
    fn held(&self) -> MutexGuard<'_, Decimal> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One call's share of the budget, held until the call's cost is recorded:
/// drop it after [`record`].
pub struct Reservation<'a> {
    pool: Option<&'a Reservations>,
    amount: Decimal,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool {
            *pool.held() -= self.amount;
        }
    }
}

/// Checks the budget and sets an estimate aside for one call of `step` on
/// `model`; `None` when the budget is used up. The estimate is the dearest
/// such call this month, or [`DEFAULT_ESTIMATE_USD`].
///
/// A call may start while this month's spend is under the budget and, when
/// other calls are running, their estimates and its own still fit in it. One
/// call at a time may still go over by its own cost, as before, but calls
/// running at once no longer pile on top of it. The check runs on the single
/// write connection, so it never sees a finished call's reservation gone
/// before its recorded cost is there. Without a budget nothing is held.
pub async fn reserve<'a>(
    app: &App,
    pool: &'a Reservations,
    model: &str,
    step: &str,
) -> Result<Option<Reservation<'a>>> {
    let Some(budget) = app.runtime_settings().llm.monthly_budget_usd else {
        return Ok(Some(Reservation {
            pool: None,
            amount: Decimal::ZERO,
        }));
    };
    let now = Utc::now();
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now);
    let mut conn = app.db.write.acquire().await?;
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT model, step, cost_usd FROM llm_usage WHERE created_at >= ?")
            .bind(time::ts(start))
            .fetch_all(&mut *conn)
            .await?;
    let mut spent = Decimal::ZERO;
    let mut dearest: Option<Decimal> = None;
    for (m, s, cost) in rows {
        let Ok(cost) = cost.parse::<Decimal>() else {
            continue;
        };
        spent += cost;
        if m == model && s == step {
            dearest = Some(dearest.map_or(cost, |d| d.max(cost)));
        }
    }
    let estimate = dearest.unwrap_or(DEFAULT_ESTIMATE_USD);
    let mut held = pool.held();
    let fits = spent < budget && (held.is_zero() || spent + *held + estimate <= budget);
    if !fits {
        return Ok(None);
    }
    *held += estimate;
    drop(held);
    drop(conn);
    Ok(Some(Reservation {
        pool: Some(pool),
        amount: estimate,
    }))
}

/// What may still be spent this UTC month; `None` when no budget is set.
/// Never negative: an exhausted budget is `Some(0)`.
pub async fn remaining(app: &App) -> Result<Option<Decimal>> {
    let Some(budget) = app.runtime_settings().llm.monthly_budget_usd else {
        return Ok(None);
    };
    let spent = stats::llm_month_usd(&app.ctx(Actor::system())).await?;
    Ok(Some((budget - spent).max(Decimal::ZERO)))
}

/// Records one call's usage inside the caller's transaction. `step` is
/// `transcribe`, `classify` or `fields`.
pub async fn record(
    tx: &mut SqliteConnection,
    doc: DocumentId,
    model: &str,
    step: &str,
    usage: &Usage,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO llm_usage \
         (document_id, model, step, input_tokens, output_tokens, cost_usd, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(doc)
    .bind(model)
    .bind(step)
    .bind(usage.input_tokens)
    .bind(usage.output_tokens)
    .bind(usage.cost_usd.to_string())
    .bind(time::now_ts())
    .execute(tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::service::testing::test_app;

    async fn app_with_budget(usd: &str) -> (tempfile::TempDir, App) {
        let (dir, mut app) = test_app().await;
        Arc::get_mut(&mut app.config)
            .expect("the test app's config is not shared yet")
            .llm_monthly_budget_usd = Some(usd.parse().unwrap());
        app.reload_settings().await.unwrap();
        (dir, app)
    }

    async fn spend(app: &App, step: &str, usd: &str) {
        sqlx::query(
            "INSERT INTO llm_usage (model, step, input_tokens, output_tokens, cost_usd, created_at) \
             VALUES ('m', ?, 1, 1, ?, ?)",
        )
        .bind(step)
        .bind(usd)
        .bind(time::now_ts())
        .execute(&app.db.write)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn calls_running_at_once_cannot_overspend_the_budget() {
        let (_dir, app) = app_with_budget("0.10").await;
        // One classify call this month cost 4 cents: that is the estimate.
        spend(&app, "classify", "0.04").await;
        let pool = Reservations::default();
        let first = reserve(&app, &pool, "m", "classify").await.unwrap();
        assert!(first.is_some(), "0.04 spent, nothing running");
        // 0.04 spent + 0.04 running + 0.04 for this one = 0.12 > 0.10.
        assert!(
            reserve(&app, &pool, "m", "classify")
                .await
                .unwrap()
                .is_none()
        );
        // Once the first call is done (its cost recorded), there is room again.
        spend(&app, "classify", "0.01").await;
        drop(first);
        assert!(
            reserve(&app, &pool, "m", "classify")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn one_call_at_a_time_runs_while_any_budget_is_left() {
        let (_dir, app) = app_with_budget("0.001").await;
        let pool = Reservations::default();
        // No history: the default estimate is far over the budget, yet a
        // lone call still runs, as before reservations.
        let lone = reserve(&app, &pool, "m", "fields").await.unwrap();
        assert!(lone.is_some());
        assert!(reserve(&app, &pool, "m", "fields").await.unwrap().is_none());
        drop(lone);
        spend(&app, "fields", "0.001").await;
        assert!(
            reserve(&app, &pool, "m", "fields").await.unwrap().is_none(),
            "used up"
        );
    }

    #[tokio::test]
    async fn without_a_budget_nothing_is_held() {
        let (_dir, app) = test_app().await;
        let pool = Reservations::default();
        let held: Vec<_> = futures_util::future::join_all(
            (0..10).map(|_| reserve(&app, &pool, "m", "transcribe")),
        )
        .await;
        assert!(held.iter().all(|r| matches!(r, Ok(Some(_)))));
        assert!(pool.held().is_zero());
    }
}
