//! The OpenAPI document served at `/api/v1/openapi.json`.

use std::sync::Arc;

use axum::Json;
use axum::extract::Extension;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::tag::TagBuilder;
use utoipa::openapi::{ComponentsBuilder, InfoBuilder, OpenApi, OpenApiBuilder};

pub fn build() -> OpenApi {
    OpenApiBuilder::new()
        .info(
            InfoBuilder::new()
                .title("paperjam")
                .version(env!("CARGO_PKG_VERSION"))
                .description(Some(
                    "Self-hosted document archive with a first-class MCP server",
                ))
                .build(),
        )
        .tags(Some(vec![
            TagBuilder::new()
                .name("auth")
                .description(Some("Authentication and session management"))
                .build(),
            TagBuilder::new()
                .name("documents")
                .description(Some("Document storage, retrieval, and processing"))
                .build(),
            TagBuilder::new()
                .name("shares")
                .description(Some("Share links: a page that shows one document to anyone holding the link, for a short while"))
                .build(),
            TagBuilder::new()
                .name("search")
                .description(Some("Full-text and structured search"))
                .build(),
            TagBuilder::new()
                .name("taxonomy")
                .description(Some("People, correspondents, tags, and document types"))
                .build(),
            TagBuilder::new()
                .name("users")
                .description(Some("User administration"))
                .build(),
            TagBuilder::new()
                .name("tokens")
                .description(Some("API tokens"))
                .build(),
            TagBuilder::new()
                .name("calendar")
                .description(Some("The calendar subscription of the signed-in user: due dates as an .ics feed"))
                .build(),
            TagBuilder::new()
                .name("passkeys")
                .description(Some("Passkeys (WebAuthn) of the signed-in web user"))
                .build(),
            TagBuilder::new()
                .name("stats")
                .description(Some("Dashboard numbers and statistics"))
                .build(),
            TagBuilder::new()
                .name("settings")
                .description(Some("Instance configuration and runtime settings"))
                .build(),
            TagBuilder::new()
                .name("system")
                .description(Some("System endpoints"))
                .build(),
        ]))
        .components(Some(
            ComponentsBuilder::new()
                .security_scheme(
                    "session_cookie",
                    SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new("pj_session"))),
                )
                .security_scheme(
                    "bearer_auth",
                    SecurityScheme::Http(
                        HttpBuilder::new()
                            .scheme(HttpAuthScheme::Bearer)
                            .bearer_format("pj_*")
                            .build(),
                    ),
                )
                .build(),
        ))
        .build()
}

/// The OpenAPI specification of this API.
#[utoipa::path(
    get,
    path = "/openapi.json",
    tag = "system",
    summary = "The OpenAPI specification of this API.",
    responses(
        (status = 200, description = "The OpenAPI specification", body = serde_json::Value),
    )
)]
pub async fn document(Extension(spec): Extension<Arc<OpenApi>>) -> Json<Arc<OpenApi>> {
    Json(spec)
}
