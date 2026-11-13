//! People, correspondents, tags and document types.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::http::auth::Principal;
use crate::http::{ApiJson, ApiPath, ApiQuery, ApiResult, HttpState};
use crate::ids::{CorrespondentId, PersonId, TagId, TypeId};
use crate::model::taxonomy::{
    CorrespondentPatch, CorrespondentView, NewCorrespondent, NewPerson, NewType, PersonPatch,
    PersonView, TagView, Taxonomy, TypePatch, TypeView,
};
use crate::service::taxonomy;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct AliasBody {
    pub alias: String,
}

#[derive(Deserialize)]
pub struct AliasQuery {
    pub alias: String,
}

/// Everything documents can be filed under, with document counts.
#[utoipa::path(
    get,
    path = "/taxonomy",
    tag = "taxonomy",
    summary = "Everything documents can be filed under, with document counts.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    responses(
        (status = 200, description = "Taxonomy listing", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
    )
)]
pub async fn list(
    State(state): State<HttpState>,
    principal: Principal,
) -> ApiResult<Json<Taxonomy>> {
    Ok(Json(
        taxonomy::list_taxonomy(&state.app.ctx(principal.actor)).await?,
    ))
}

// --- people ----------------------------------------------------------------

/// Create a new person.
#[utoipa::path(
    post,
    path = "/people",
    tag = "taxonomy",
    summary = "Create a new person.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = serde_json::Value,
    responses(
        (status = 201, description = "Person created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 409, description = "Person name already exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid person data", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_person(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewPerson>,
) -> ApiResult<(StatusCode, Json<PersonView>)> {
    let view = taxonomy::create_person(&state.app.ctx(principal.actor), new).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Update a person.
#[utoipa::path(
    patch,
    path = "/people/{id}",
    tag = "taxonomy",
    summary = "Update a person.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Person ID"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Updated person details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid patch data", body = crate::http::ErrorResponse),
    )
)]
pub async fn update_person(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
    ApiJson(patch): ApiJson<PersonPatch>,
) -> ApiResult<Json<PersonView>> {
    Ok(Json(
        taxonomy::update_person(&state.app.ctx(principal.actor), id, patch).await?,
    ))
}

/// Add an alias to a person.
#[utoipa::path(
    post,
    path = "/people/{id}/aliases",
    tag = "taxonomy",
    summary = "Add an alias to a person.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Person ID"),
    ),
    request_body = AliasBody,
    responses(
        (status = 200, description = "Updated person details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid alias", body = crate::http::ErrorResponse),
    )
)]
pub async fn add_person_alias(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
    ApiJson(body): ApiJson<AliasBody>,
) -> ApiResult<Json<PersonView>> {
    Ok(Json(
        taxonomy::add_person_alias(&state.app.ctx(principal.actor), id, &body.alias).await?,
    ))
}

/// Remove an alias from a person.
#[utoipa::path(
    delete,
    path = "/people/{id}/aliases",
    tag = "taxonomy",
    summary = "Remove an alias from a person.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Person ID"),
        ("alias" = String, Query, description = "Alias to remove"),
    ),
    responses(
        (status = 200, description = "Updated person details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid alias query", body = crate::http::ErrorResponse),
    )
)]
pub async fn remove_person_alias(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
    ApiQuery(query): ApiQuery<AliasQuery>,
) -> ApiResult<Json<PersonView>> {
    Ok(Json(
        taxonomy::remove_person_alias(&state.app.ctx(principal.actor), id, &query.alias).await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MergePeople {
    #[schema(value_type = String)]
    pub from: PersonId,
}

/// Moves every reference of `from` to this person and unions the aliases.
#[utoipa::path(
    post,
    path = "/people/{id}/merge",
    tag = "taxonomy",
    summary = "Moves every reference of from to this person and unions the aliases.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Target person ID"),
    ),
    request_body = MergePeople,
    responses(
        (status = 200, description = "Updated person details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid merge request", body = crate::http::ErrorResponse),
    )
)]
pub async fn merge_people(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
    ApiJson(body): ApiJson<MergePeople>,
) -> ApiResult<Json<PersonView>> {
    Ok(Json(
        taxonomy::merge_people(&state.app.ctx(principal.actor), id, body.from).await?,
    ))
}

/// Match the existing documents against the people again, in the background.
#[utoipa::path(
    post,
    path = "/people/{id}/relink",
    tag = "taxonomy",
    summary = "Match the existing documents against the people again, in the background.",
    description = "Looks for the names and aliases of every person in the text of every document, without the LLM, and links what it finds. Documents whose people were set by hand are left alone. This also runs by itself whenever a person is created, renamed or given or losing an alias; call it to repeat the pass on request. List the documents of the person with a search filtered by `people`.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Person ID"),
    ),
    responses(
        (status = 202, description = "Matching queued; the person as of now", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn relink_person(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
) -> ApiResult<(StatusCode, Json<PersonView>)> {
    let view = taxonomy::relink_person(&state.app.ctx(principal.actor), id).await?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

/// Delete a person.
#[utoipa::path(
    delete,
    path = "/people/{id}",
    tag = "taxonomy",
    summary = "Delete a person.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Person ID"),
    ),
    responses(
        (status = 204, description = "Person deleted"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Person not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn delete_person(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<PersonId>,
) -> ApiResult<StatusCode> {
    taxonomy::delete_person(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- correspondents --------------------------------------------------------

/// Create a new correspondent.
#[utoipa::path(
    post,
    path = "/correspondents",
    tag = "taxonomy",
    summary = "Create a new correspondent.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = serde_json::Value,
    responses(
        (status = 201, description = "Correspondent created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 409, description = "Correspondent name already exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid correspondent data", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_correspondent(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewCorrespondent>,
) -> ApiResult<(StatusCode, Json<CorrespondentView>)> {
    let view = taxonomy::create_correspondent(&state.app.ctx(principal.actor), new).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Update an existing correspondent.
#[utoipa::path(
    patch,
    path = "/correspondents/{id}",
    tag = "taxonomy",
    summary = "Update an existing correspondent.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Correspondent ID"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Updated correspondent details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Correspondent not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid patch data", body = crate::http::ErrorResponse),
    )
)]
pub async fn update_correspondent(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<CorrespondentId>,
    ApiJson(patch): ApiJson<CorrespondentPatch>,
) -> ApiResult<Json<CorrespondentView>> {
    Ok(Json(
        taxonomy::update_correspondent(&state.app.ctx(principal.actor), id, patch).await?,
    ))
}

/// Add an alias to a correspondent.
#[utoipa::path(
    post,
    path = "/correspondents/{id}/aliases",
    tag = "taxonomy",
    summary = "Add an alias to a correspondent.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Correspondent ID"),
    ),
    request_body = AliasBody,
    responses(
        (status = 200, description = "Updated correspondent details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Correspondent not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid alias", body = crate::http::ErrorResponse),
    )
)]
pub async fn add_correspondent_alias(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<CorrespondentId>,
    ApiJson(body): ApiJson<AliasBody>,
) -> ApiResult<Json<CorrespondentView>> {
    Ok(Json(
        taxonomy::add_correspondent_alias(&state.app.ctx(principal.actor), id, &body.alias).await?,
    ))
}

/// Remove an alias from a correspondent.
#[utoipa::path(
    delete,
    path = "/correspondents/{id}/aliases",
    tag = "taxonomy",
    summary = "Remove an alias from a correspondent.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Correspondent ID"),
        ("alias" = String, Query, description = "Alias to remove"),
    ),
    responses(
        (status = 200, description = "Updated correspondent details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Correspondent not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid alias query", body = crate::http::ErrorResponse),
    )
)]
pub async fn remove_correspondent_alias(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<CorrespondentId>,
    ApiQuery(query): ApiQuery<AliasQuery>,
) -> ApiResult<Json<CorrespondentView>> {
    Ok(Json(
        taxonomy::remove_correspondent_alias(&state.app.ctx(principal.actor), id, &query.alias)
            .await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct MergeCorrespondents {
    #[schema(value_type = String)]
    pub from: CorrespondentId,
}

/// Moves every reference of `from` to this correspondent and unions the aliases.
#[utoipa::path(
    post,
    path = "/correspondents/{id}/merge",
    tag = "taxonomy",
    summary = "Moves every reference of from to this correspondent and unions the aliases.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Target correspondent ID"),
    ),
    request_body = MergeCorrespondents,
    responses(
        (status = 200, description = "Updated correspondent details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Correspondent not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid merge request", body = crate::http::ErrorResponse),
    )
)]
pub async fn merge_correspondents(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<CorrespondentId>,
    ApiJson(body): ApiJson<MergeCorrespondents>,
) -> ApiResult<Json<CorrespondentView>> {
    Ok(Json(
        taxonomy::merge_correspondents(&state.app.ctx(principal.actor), id, body.from).await?,
    ))
}

/// Delete a correspondent.
#[utoipa::path(
    delete,
    path = "/correspondents/{id}",
    tag = "taxonomy",
    summary = "Delete a correspondent.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Correspondent ID"),
    ),
    responses(
        (status = 204, description = "Correspondent deleted"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Correspondent not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn delete_correspondent(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<CorrespondentId>,
) -> ApiResult<StatusCode> {
    taxonomy::delete_correspondent(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- tags ------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewTag {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RenameTag {
    pub name: String,
}

/// Create a new tag.
#[utoipa::path(
    post,
    path = "/tags",
    tag = "taxonomy",
    summary = "Create a new tag.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = NewTag,
    responses(
        (status = 201, description = "Tag created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 409, description = "Tag name already exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid tag data", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_tag(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewTag>,
) -> ApiResult<(StatusCode, Json<TagView>)> {
    let view = taxonomy::create_tag(&state.app.ctx(principal.actor), &new.name, new.color).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Rename an existing tag.
#[utoipa::path(
    patch,
    path = "/tags/{id}",
    tag = "taxonomy",
    summary = "Rename an existing tag.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Tag ID"),
    ),
    request_body = RenameTag,
    responses(
        (status = 200, description = "Updated tag details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Tag not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid rename data", body = crate::http::ErrorResponse),
    )
)]
pub async fn rename_tag(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<TagId>,
    ApiJson(body): ApiJson<RenameTag>,
) -> ApiResult<Json<TagView>> {
    Ok(Json(
        taxonomy::rename_tag(&state.app.ctx(principal.actor), id, &body.name).await?,
    ))
}

/// Delete a tag.
#[utoipa::path(
    delete,
    path = "/tags/{id}",
    tag = "taxonomy",
    summary = "Delete a tag.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Tag ID"),
    ),
    responses(
        (status = 204, description = "Tag deleted"),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Tag not found", body = crate::http::ErrorResponse),
    )
)]
pub async fn delete_tag(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<TagId>,
) -> ApiResult<StatusCode> {
    taxonomy::delete_tag(&state.app.ctx(principal.actor), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- types -----------------------------------------------------------------

/// Create a new document type.
#[utoipa::path(
    post,
    path = "/types",
    tag = "taxonomy",
    summary = "Create a new document type.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    request_body = serde_json::Value,
    responses(
        (status = 201, description = "Document type created", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 409, description = "Document type name already exists", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid document type data", body = crate::http::ErrorResponse),
    )
)]
pub async fn create_type(
    State(state): State<HttpState>,
    principal: Principal,
    ApiJson(new): ApiJson<NewType>,
) -> ApiResult<(StatusCode, Json<TypeView>)> {
    let view = taxonomy::create_type(&state.app.ctx(principal.actor), new).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Update an existing document type.
#[utoipa::path(
    patch,
    path = "/types/{id}",
    tag = "taxonomy",
    summary = "Update an existing document type.",
    security(
        ("session_cookie" = []),
        ("bearer_auth" = [])
    ),
    params(
        ("id" = String, Path, description = "Type ID"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Updated document type details", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = crate::http::ErrorResponse),
        (status = 404, description = "Document type not found", body = crate::http::ErrorResponse),
        (status = 422, description = "Invalid patch data", body = crate::http::ErrorResponse),
    )
)]
pub async fn update_type(
    State(state): State<HttpState>,
    principal: Principal,
    ApiPath(id): ApiPath<TypeId>,
    ApiJson(patch): ApiJson<TypePatch>,
) -> ApiResult<Json<TypeView>> {
    Ok(Json(
        taxonomy::update_type(&state.app.ctx(principal.actor), id, patch).await?,
    ))
}
