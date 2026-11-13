//! Spend accounting: every call is recorded in `llm_usage`; the optional
//! monthly budget (`PAPERJAM_LLM_MONTHLY_BUDGET_USD`) is checked before each
//! LLM step.

use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use crate::error::Result;
use crate::ids::DocumentId;
use crate::llm::Usage;
use crate::service::{Actor, App, stats};
use crate::time;

/// What may still be spent this UTC month; `None` when no budget is set.
/// Never negative: an exhausted budget is `Some(0)`.
pub async fn remaining(app: &App) -> Result<Option<Decimal>> {
    let Some(budget) = app.config.llm_monthly_budget_usd else {
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
