//! `/calendar/<secret>.ics`: the due-date feed that calendar apps subscribe to.
//! The secret in the address is the only credential; every failure is a bare 404.

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;

use crate::http::HttpState;
use crate::service::calendar;

pub async fn feed(State(state): State<HttpState>, Path(file): Path<String>) -> Response {
    let Some(secret) = file.strip_suffix(".ics") else {
        return failure_status(StatusCode::NOT_FOUND);
    };
    let user = match calendar::open(&state.app, secret).await {
        Ok(user) => user,
        Err(e) => return failure(&e),
    };
    match calendar::render(&state.app, user, Utc::now()).await {
        Ok(body) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/calendar; charset=utf-8"),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-cache"),
                ),
                (
                    header::REFERRER_POLICY,
                    HeaderValue::from_static("no-referrer"),
                ),
            ],
            body,
        )
            .into_response(),
        Err(e) => failure(&e),
    }
}

fn failure(e: &crate::error::ServiceError) -> Response {
    if e.kind == crate::error::ErrorKind::NotFound {
        return failure_status(StatusCode::NOT_FOUND);
    }
    tracing::warn!(code = e.code, "the calendar feed could not be built");
    failure_status(StatusCode::INTERNAL_SERVER_ERROR)
}

fn failure_status(status: StatusCode) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
        ],
    )
        .into_response()
}
