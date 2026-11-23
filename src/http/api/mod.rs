//! The REST routes under `/api/v1`.

pub mod calendar_feed;
pub mod connections;
pub mod documents;
pub mod email;
pub mod links;
pub mod passkeys;
pub mod search;
pub mod shares;
pub mod stats;
pub mod taxonomy;
pub mod tokens;
pub mod users;

use axum::extract::DefaultBodyLimit;
use tower::ServiceBuilder;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};
use utoipa_axum::routes;

use crate::http::{HttpState, auth, openapi};

/// Every route of the REST API as `(method, path relative to /api/v1)`,
/// read from the same OpenAPI document that is served to clients.
pub fn route_list() -> Vec<(axum::http::Method, String)> {
    use axum::http::Method;
    use utoipa::openapi::HttpMethod;

    let (_, openapi) = router(0).split_for_parts();
    let mut routes = Vec::new();
    for (path, item) in &openapi.paths.paths {
        for (method, present) in [
            (HttpMethod::Get, Method::GET),
            (HttpMethod::Post, Method::POST),
            (HttpMethod::Put, Method::PUT),
            (HttpMethod::Patch, Method::PATCH),
            (HttpMethod::Delete, Method::DELETE),
        ] {
            let operation = match method {
                HttpMethod::Get => &item.get,
                HttpMethod::Post => &item.post,
                HttpMethod::Put => &item.put,
                HttpMethod::Patch => &item.patch,
                _ => &item.delete,
            };
            if operation.is_some() {
                routes.push((present, path.clone()));
            }
        }
    }
    routes
}

/// `upload_limit` is the body ceiling of the three routes that carry a file;
/// every other route keeps the small default set on the whole server.
pub fn router(upload_limit: usize) -> OpenApiRouter<HttpState> {
    OpenApiRouter::with_openapi(openapi::build())
        .routes(routes!(auth::setup_status, auth::setup))
        .routes(routes!(auth::login))
        .routes(routes!(auth::logout))
        .routes(routes!(auth::session, auth::update_session))
        .routes(routes!(auth::change_password))
        .routes(routes!(auth::passkey_challenge))
        .routes(routes!(auth::passkey_login))
        .routes(routes!(documents::create).layer(DefaultBodyLimit::max(upload_limit)))
        .routes(
            routes!(email::ingest).layer(
                ServiceBuilder::new()
                    .layer(DefaultBodyLimit::max(upload_limit))
                    .layer(RequestBodyLimitLayer::new(upload_limit)),
            ),
        )
        .routes(routes!(documents::get, documents::patch, documents::purge))
        .routes(routes!(documents::original))
        .routes(routes!(documents::download))
        .routes(routes!(documents::page_image))
        .routes(routes!(documents::thumbnail))
        .routes(routes!(documents::events))
        .routes(routes!(documents::reprocess))
        .routes(routes!(documents::trash))
        .routes(routes!(documents::restore))
        .routes(routes!(documents::empty_trash))
        .routes(routes!(shares::create, shares::list))
        .routes(routes!(shares::revoke))
        .routes(routes!(crate::http::share::page_image))
        .routes(routes!(crate::http::share::download))
        .routes(routes!(
            calendar_feed::get,
            calendar_feed::create,
            calendar_feed::revoke
        ))
        .routes(routes!(links::create_download_link))
        .routes(routes!(links::create_upload_link))
        .routes(routes!(links::download))
        .routes(
            routes!(links::upload_with_link).layer(
                ServiceBuilder::new()
                    .layer(DefaultBodyLimit::max(upload_limit))
                    .layer(RequestBodyLimitLayer::new(upload_limit)),
            ),
        )
        .routes(routes!(search::search))
        .routes(routes!(taxonomy::list))
        .routes(routes!(taxonomy::create_person))
        .routes(routes!(taxonomy::update_person, taxonomy::delete_person))
        .routes(routes!(
            taxonomy::add_person_alias,
            taxonomy::remove_person_alias
        ))
        .routes(routes!(taxonomy::merge_people))
        .routes(routes!(taxonomy::relink_person))
        .routes(routes!(taxonomy::create_correspondent))
        .routes(routes!(
            taxonomy::update_correspondent,
            taxonomy::delete_correspondent
        ))
        .routes(routes!(
            taxonomy::add_correspondent_alias,
            taxonomy::remove_correspondent_alias
        ))
        .routes(routes!(taxonomy::merge_correspondents))
        .routes(routes!(taxonomy::create_tag))
        .routes(routes!(taxonomy::rename_tag, taxonomy::delete_tag))
        .routes(routes!(taxonomy::create_type))
        .routes(routes!(taxonomy::update_type))
        .routes(routes!(users::list, users::create))
        .routes(routes!(users::update, users::delete))
        .routes(routes!(tokens::list, tokens::create))
        .routes(routes!(tokens::revoke))
        .routes(routes!(connections::list))
        .routes(routes!(connections::revoke))
        .routes(routes!(passkeys::list, passkeys::create))
        .routes(routes!(passkeys::challenge))
        .routes(routes!(passkeys::revoke))
        .routes(routes!(stats::get))
        .routes(routes!(openapi::document))
}
