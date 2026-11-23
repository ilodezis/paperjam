//! The calendar feed of the signed-in user: make, rotate, revoke.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::http::auth::Principal;
use crate::http::{ApiResult, HttpState};
use crate::service::calendar::{self, FeedView};

/// The feed as settings show it.
#[derive(Serialize, utoipa::ToSchema)]
pub struct CalendarFeed {
    /// Whether a feed exists.
    pub active: bool,
    pub feed: Option<FeedView>,
    /// Days before a due date the feed adds a "renew" event; 0 means it does not.
    pub lead_days: u32,
}

/// A freshly made or rotated feed. The secret and the address are in this response only.
#[derive(Serialize, utoipa::ToSchema)]
pub struct CreatedCalendarFeed {
    pub feed: FeedView,
    pub secret: String,
    /// The path to subscribe to, `/calendar/<secret>.ics`.
    pub path: String,
    /// The full address when `PAPERJAM_PUBLIC_URL` is set; otherwise the client prefixes the origin it reached the server on.
    pub url: Option<String>,
}

/// The calendar feed of the signed-in user, without its secret.
#[utoipa::path(
    get,
    path = "/calendar-feed",
    tag = "calendar",
    summary = "The calendar feed of the signed-in user, without its secret.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Feed state", body = CalendarFeed),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn get(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<CalendarFeed>> {
    let feed = calendar::get(&state.app.ctx(principal.actor)).await?;
    let lead_days = state.app.runtime_settings().calendar.lead_days;
    Ok(Json(CalendarFeed {
        active: feed.is_some(),
        feed,
        lead_days,
    }))
}

/// Makes the calendar feed, or rotates its secret so the old link stops working.
#[utoipa::path(
    post,
    path = "/calendar-feed",
    tag = "calendar",
    summary = "Makes the calendar feed, or rotates its secret so the old link stops working. The secret is in this response only.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 201, description = "Feed made", body = CreatedCalendarFeed),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn create(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<(StatusCode, Json<CreatedCalendarFeed>)> {
    let (feed, secret) = calendar::create_or_rotate(&state.app.ctx(principal.actor)).await?;
    let (path, url) = calendar::feed_url(&state.app, &secret);
    Ok((
        StatusCode::CREATED,
        Json(CreatedCalendarFeed {
            feed,
            secret,
            path,
            url,
        }),
    ))
}

/// Revokes the calendar feed; its link stops working at once.
#[utoipa::path(
    delete,
    path = "/calendar-feed",
    tag = "calendar",
    summary = "Revokes the calendar feed; its link stops working at once.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 204, description = "Feed revoked"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "There is no feed", body = crate::http::ErrorResponse),
    )
)]
pub async fn revoke(State(state): State<HttpState>, principal: Principal) -> ApiResult<StatusCode> {
    calendar::revoke(&state.app.ctx(principal.actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}
