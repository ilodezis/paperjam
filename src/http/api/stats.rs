//! Dashboard numbers.

use axum::Json;
use axum::extract::State;

use crate::http::auth::Principal;
use crate::http::{ApiResult, HttpState};
use crate::service::stats::{self, Stats};

/// Document counts, attention, queue depth and this month's LLM spend.
#[utoipa::path(
    get,
    path = "/stats",
    tag = "stats",
    summary = "Document counts, attention, queue depth and this month's LLM spend.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Dashboard numbers", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn get(State(state): State<HttpState>, principal: Principal) -> ApiResult<Json<Stats>> {
    Ok(Json(
        stats::get_stats(&state.app.ctx(principal.actor)).await?,
    ))
}
