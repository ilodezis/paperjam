//! The tools. `tools.json` declares them (names, schemas, annotations); this
//! file is what they do. Every tool is a thin call into the service layer.

use std::str::FromStr;

use axum::http::Method;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rmcp::model::ContentBlock;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::error::{Result, ServiceError};
use crate::ids::{CorrespondentId, DocumentId, PersonId, TagId, TypeId};
use crate::jobs::queue::ProcessOptions;
use crate::model::document::{DocumentPatch, GetOptions};
use crate::model::fields::{FieldDef, Labels};
use crate::model::taxonomy::{
    CorrespondentPatch, NewCorrespondent, NewPerson, NewType, PersonPatch, TypePatch,
};
use crate::search::{SearchRequest, sync};
use crate::service::upload::{UploadInput, upload};
use crate::service::{Actor, App, Ctx, documents, links, shares, stats, taxonomy};

/// The checked-in contract: server instructions and every tool's schemas.
pub const CONTRACT: &str = include_str!("tools.json");

/// `upload_document` accepts at most this many decoded bytes.
pub const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;
/// Base64 inflates by 4/3; this is the longest text that can still decode to
/// [`MAX_UPLOAD_BYTES`], so oversized input is refused before decoding.
const MAX_ENCODED_BYTES: usize = MAX_UPLOAD_BYTES.div_ceil(3) * 4;
const PAGE_IMAGE_MIME: &str = "image/webp";

/// The REST routes (relative to `/api/v1`) that offer the same capability as a
/// tool. `tests/mcp_parity.rs` holds this table against the REST router: every
/// route is either listed here or on the test's explicit exception list.
pub struct ToolRoutes {
    pub tool: &'static str,
    pub rest: &'static [(Method, &'static str)],
}

pub const REST_ROUTES: &[ToolRoutes] = &[
    ToolRoutes {
        tool: "search_documents",
        rest: &[(Method::POST, "/search")],
    },
    ToolRoutes {
        tool: "get_document",
        rest: &[(Method::GET, "/documents/{id}")],
    },
    ToolRoutes {
        tool: "get_page_image",
        rest: &[(Method::GET, "/documents/{id}/pages/{file}")],
    },
    ToolRoutes {
        tool: "get_download_link",
        rest: &[
            (Method::POST, "/documents/{id}/download-link"),
            // The same bytes, streamed to an authenticated caller directly.
            (Method::GET, "/documents/{id}/original"),
            // The clean copy of a scan when there is one, streamed the same way.
            (Method::GET, "/documents/{id}/download"),
        ],
    },
    ToolRoutes {
        tool: "share_document",
        rest: &[(Method::POST, "/documents/{id}/shares")],
    },
    ToolRoutes {
        tool: "list_taxonomy",
        rest: &[(Method::GET, "/taxonomy")],
    },
    ToolRoutes {
        tool: "upload_document",
        rest: &[(Method::POST, "/documents")],
    },
    ToolRoutes {
        tool: "create_upload_link",
        rest: &[(Method::POST, "/upload-links")],
    },
    ToolRoutes {
        tool: "update_document",
        rest: &[(Method::PATCH, "/documents/{id}")],
    },
    ToolRoutes {
        tool: "reprocess_document",
        rest: &[(Method::POST, "/documents/{id}/reprocess")],
    },
    ToolRoutes {
        tool: "manage_taxonomy",
        rest: &[
            (Method::POST, "/people"),
            (Method::PATCH, "/people/{id}"),
            (Method::POST, "/people/{id}/aliases"),
            (Method::DELETE, "/people/{id}/aliases"),
            (Method::POST, "/people/{id}/merge"),
            (Method::POST, "/people/{id}/relink"),
            (Method::POST, "/correspondents"),
            (Method::PATCH, "/correspondents/{id}"),
            (Method::POST, "/correspondents/{id}/aliases"),
            (Method::DELETE, "/correspondents/{id}/aliases"),
            (Method::POST, "/correspondents/{id}/merge"),
            (Method::POST, "/tags"),
            (Method::PATCH, "/tags/{id}"),
            (Method::DELETE, "/tags/{id}"),
            (Method::POST, "/types"),
            (Method::PATCH, "/types/{id}"),
        ],
    },
    ToolRoutes {
        tool: "trash_document",
        rest: &[(Method::POST, "/documents/{id}/trash")],
    },
    ToolRoutes {
        tool: "restore_document",
        rest: &[(Method::POST, "/documents/{id}/restore")],
    },
    ToolRoutes {
        tool: "get_stats",
        rest: &[(Method::GET, "/stats")],
    },
];

/// What a tool returns: the structured result plus any extra content blocks
/// (an image) that follow the JSON text block.
pub struct ToolOutput {
    pub structured: Value,
    pub extra: Vec<ContentBlock>,
}

impl ToolOutput {
    fn json(structured: Value) -> Result<Self> {
        Ok(Self {
            structured,
            extra: Vec::new(),
        })
    }
}

pub type Args = Option<Map<String, Value>>;

/// Who is calling and how clients should reach this server.
pub struct Caller<'a> {
    pub app: &'a App,
    pub actor: Actor,
    pub base_url: String,
}

impl Caller<'_> {
    fn ctx(&self) -> Ctx<'_> {
        self.app.ctx(self.actor)
    }
}

fn invalid_arguments(what: impl std::fmt::Display) -> ServiceError {
    ServiceError::invalid("invalid_arguments", what.to_string())
}

fn parse<T: DeserializeOwned>(args: Args) -> Result<T> {
    serde_json::from_value(Value::Object(args.unwrap_or_default())).map_err(invalid_arguments)
}

/// Fails on a key the tool does not take (a typo must not silently widen a search).
fn reject_unknown(map: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<()> {
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(key) => Err(invalid_arguments(format!(
            "unknown argument {key:?} in {what}; allowed: {}",
            allowed.join(", ")
        ))),
        None => Ok(()),
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| ServiceError::internal("serialization_failed", "could not encode the result"))
}

/// Runs a tool. `None` means no such tool.
pub async fn call(caller: &Caller<'_>, name: &str, args: Args) -> Option<Result<ToolOutput>> {
    let result = match name {
        "search_documents" => search_documents(caller, args).await,
        "get_document" => get_document(caller, args).await,
        "get_page_image" => get_page_image(caller, args).await,
        "get_download_link" => get_download_link(caller, args).await,
        "share_document" => share_document(caller, args).await,
        "list_taxonomy" => list_taxonomy(caller, args).await,
        "upload_document" => upload_document(caller, args).await,
        "create_upload_link" => create_upload_link(caller, args).await,
        "update_document" => update_document(caller, args).await,
        "reprocess_document" => reprocess_document(caller, args).await,
        "manage_taxonomy" => manage_taxonomy(caller, args).await,
        "trash_document" => set_trashed(caller, args, true).await,
        "restore_document" => set_trashed(caller, args, false).await,
        "get_stats" => get_stats(caller, args).await,
        _ => return None,
    };
    Some(result)
}

fn no_arguments(args: Args) -> Result<()> {
    match args {
        Some(map) => reject_unknown(&map, &[], "this tool"),
        None => Ok(()),
    }
}

// --- search and read ---------------------------------------------------------------

async fn search_documents(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    if let Some(map) = &args {
        reject_unknown(
            map,
            &["query", "filters", "sort", "limit", "cursor"],
            "search_documents",
        )?;
        if let Some(Value::Object(filters)) = map.get("filters") {
            reject_unknown(
                filters,
                &[
                    "types",
                    "people",
                    "correspondents",
                    "tags",
                    "date_from",
                    "date_to",
                    "has_attention",
                    "local_only",
                    "fields",
                    "include_trashed",
                ],
                "filters",
            )?;
        }
    }
    let request: SearchRequest = parse(args)?;
    let page = sync::search(&caller.ctx(), request).await?;
    ToolOutput::json(json!({
        "result": to_value(&page.hits)?,
        "next_cursor": page.next_cursor,
        "total": page.total,
        "partial": page.partial,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetDocumentArgs {
    id: DocumentId,
    text_page: Option<u32>,
}

async fn get_document(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: GetDocumentArgs = parse(args)?;
    if args.text_page == Some(0) {
        return Err(invalid_arguments("text_page counts from 1"));
    }
    let view = documents::get_document(
        &caller.ctx(),
        args.id,
        GetOptions {
            text_page: args.text_page,
        },
    )
    .await?;
    ToolOutput::json(to_value(&view)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageArgs {
    id: DocumentId,
    page: u32,
}

async fn get_page_image(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: PageArgs = parse(args)?;
    if args.page == 0 {
        return Err(invalid_arguments("page counts from 1"));
    }
    // Confirms the document exists (and the caller may see it) first.
    documents::original_file(&caller.ctx(), args.id).await?;
    let path = caller
        .app
        .files
        .derived_path(args.id, &format!("page-{}.webp", args.page));
    let bytes = tokio::fs::read(&path).await.map_err(|_| {
        ServiceError::not_found(
            "page_not_found",
            "this page has no image: it does not exist or has not been rendered yet",
        )
    })?;
    Ok(ToolOutput {
        structured: json!({"id": args.id, "page": args.page, "mime_type": PAGE_IMAGE_MIME}),
        extra: vec![ContentBlock::image(STANDARD.encode(bytes), PAGE_IMAGE_MIME)],
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: DocumentId,
}

async fn get_download_link(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: IdArgs = parse(args)?;
    let link = links::create_download_link(&caller.ctx(), &caller.base_url, args.id).await?;
    ToolOutput::json(to_value(&link)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareArgs {
    id: DocumentId,
    #[serde(default)]
    ttl: shares::ShareTtl,
    #[serde(default = "default_true")]
    show_details: bool,
}

fn default_true() -> bool {
    true
}

async fn share_document(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: ShareArgs = parse(args)?;
    let share = shares::create_share(
        &caller.ctx(),
        &caller.base_url,
        args.id,
        shares::NewShare {
            ttl: args.ttl,
            show_details: args.show_details,
        },
    )
    .await?;
    ToolOutput::json(to_value(&share)?)
}

async fn list_taxonomy(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    no_arguments(args)?;
    ToolOutput::json(to_value(&taxonomy::list_taxonomy(&caller.ctx()).await?)?)
}

async fn get_stats(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    no_arguments(args)?;
    ToolOutput::json(to_value(&stats::get_stats(&caller.ctx()).await?)?)
}

// --- upload ----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadArgs {
    filename: String,
    content_base64: String,
    #[serde(default)]
    local_only: bool,
    #[serde(default)]
    tags: Vec<TagId>,
    #[serde(default)]
    people: Vec<PersonId>,
    type_id: Option<TypeId>,
}

fn too_large() -> ServiceError {
    ServiceError::invalid(
        "payload_too_large",
        "upload_document takes at most 10 MB; use create_upload_link for bigger files",
    )
}

async fn upload_document(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: UploadArgs = parse(args)?;
    let encoded: String = args
        .content_base64
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(too_large());
    }
    let bytes = STANDARD.decode(encoded.as_bytes()).map_err(|_| {
        ServiceError::invalid(
            "invalid_base64",
            "content_base64 is not valid standard base64",
        )
    })?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(too_large());
    }
    let input = UploadInput {
        original_name: args.filename,
        declared_mime: None,
        local_only: args.local_only,
        tags: args.tags,
        people: args.people,
        type_id: args.type_id,
    };
    let outcome = upload(&caller.ctx(), input, bytes.as_slice()).await?;
    ToolOutput::json(json!({
        "id": outcome.document_id(),
        "status": "queued",
        "restored": matches!(outcome, crate::service::upload::UploadOutcome::Restored(_)),
    }))
}

async fn create_upload_link(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    no_arguments(args)?;
    let link = links::issue_upload_link(&caller.ctx(), &caller.base_url).await?;
    ToolOutput::json(to_value(&link)?)
}

// --- edit ------------------------------------------------------------------------

const PATCH_KEYS: &[&str] = &[
    "id",
    "title",
    "document_date",
    "type_id",
    "organizations",
    "people",
    "tags",
    "fields",
    "summary",
    "local_only",
];

async fn update_document(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let mut map = args.unwrap_or_default();
    reject_unknown(&map, PATCH_KEYS, "update_document")?;
    let id: DocumentId = serde_json::from_value(
        map.remove("id")
            .ok_or_else(|| invalid_arguments("id is required"))?,
    )
    .map_err(invalid_arguments)?;
    let patch: DocumentPatch = parse(Some(map))?;
    let view = documents::update_document(&caller.ctx(), id, patch).await?;
    ToolOutput::json(to_value(&view)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReprocessArgs {
    id: DocumentId,
    local: Option<bool>,
    llm: Option<bool>,
    model: Option<String>,
}

async fn reprocess_document(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let args: ReprocessArgs = parse(args)?;
    let opts = ProcessOptions {
        local: args.local.unwrap_or(true),
        llm: args.llm.unwrap_or(true),
        model_override: args.model,
    };
    documents::reprocess_document(&caller.ctx(), args.id, opts).await?;
    ToolOutput::json(json!({"id": args.id, "status": "queued"}))
}

async fn set_trashed(caller: &Caller<'_>, args: Args, trashed: bool) -> Result<ToolOutput> {
    let args: IdArgs = parse(args)?;
    let ctx = caller.ctx();
    if trashed {
        documents::trash_document(&ctx, args.id).await?;
    } else {
        documents::restore_document(&ctx, args.id).await?;
    }
    ToolOutput::json(json!({"id": args.id, "trashed": trashed}))
}

// --- taxonomy --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    CreatePerson,
    UpdatePerson,
    AddAlias,
    RemoveAlias,
    CreateCorrespondent,
    UpdateCorrespondent,
    CreateTag,
    RenameTag,
    DeleteTag,
    CreateType,
    UpdateType,
    MergePeople,
    MergeCorrespondents,
    RelinkPerson,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Person,
    Correspondent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageArgs {
    action: Action,
    kind: Option<Kind>,
    id: Option<String>,
    into_id: Option<String>,
    from_id: Option<String>,
    name: Option<String>,
    alias: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    notes: Option<String>,
    color: Option<String>,
    key: Option<String>,
    labels: Option<Labels>,
    fields: Option<Vec<FieldDef>>,
}

fn need<'a, T>(value: &'a Option<T>, argument: &str, action: &str) -> Result<&'a T> {
    value.as_ref().ok_or_else(|| {
        ServiceError::invalid(
            "missing_argument",
            format!("{action} needs the argument {argument}"),
        )
    })
}

fn need_id<T: FromStr>(value: &Option<String>, argument: &str, action: &str) -> Result<T> {
    need(value, argument, action)?
        .parse()
        .map_err(|_| invalid_arguments(format!("{argument} is not a valid id")))
}

fn item(kind: &str, view: &impl serde::Serialize, action: &str) -> Result<ToolOutput> {
    ToolOutput::json(json!({"action": action, "kind": kind, "item": to_value(view)?}))
}

async fn manage_taxonomy(caller: &Caller<'_>, args: Args) -> Result<ToolOutput> {
    let a: ManageArgs = parse(args)?;
    let ctx = caller.ctx();
    match a.action {
        Action::CreatePerson => {
            let view = taxonomy::create_person(
                &ctx,
                NewPerson {
                    name: need(&a.name, "name", "create_person")?.clone(),
                    aliases: a.aliases.clone(),
                    notes: a.notes.clone().unwrap_or_default(),
                },
            )
            .await?;
            item("person", &view, "create_person")
        }
        Action::UpdatePerson => {
            let id: PersonId = need_id(&a.id, "id", "update_person")?;
            let view = taxonomy::update_person(
                &ctx,
                id,
                PersonPatch {
                    name: a.name.clone(),
                    notes: a.notes.clone(),
                },
            )
            .await?;
            item("person", &view, "update_person")
        }
        Action::AddAlias | Action::RemoveAlias => {
            let (name, add) = match a.action {
                Action::AddAlias => ("add_alias", true),
                _ => ("remove_alias", false),
            };
            let kind = *need(&a.kind, "kind", name)?;
            let alias = need(&a.alias, "alias", name)?;
            match (kind, add) {
                (Kind::Person, true) => {
                    let id: PersonId = need_id(&a.id, "id", name)?;
                    let view = taxonomy::add_person_alias(&ctx, id, alias).await?;
                    item("person", &view, name)
                }
                (Kind::Person, false) => {
                    let id: PersonId = need_id(&a.id, "id", name)?;
                    let view = taxonomy::remove_person_alias(&ctx, id, alias).await?;
                    item("person", &view, name)
                }
                (Kind::Correspondent, true) => {
                    let id: CorrespondentId = need_id(&a.id, "id", name)?;
                    let view = taxonomy::add_correspondent_alias(&ctx, id, alias).await?;
                    item("correspondent", &view, name)
                }
                (Kind::Correspondent, false) => {
                    let id: CorrespondentId = need_id(&a.id, "id", name)?;
                    let view = taxonomy::remove_correspondent_alias(&ctx, id, alias).await?;
                    item("correspondent", &view, name)
                }
            }
        }
        Action::CreateCorrespondent => {
            let view = taxonomy::create_correspondent(
                &ctx,
                NewCorrespondent {
                    name: need(&a.name, "name", "create_correspondent")?.clone(),
                    aliases: a.aliases.clone(),
                },
            )
            .await?;
            item("correspondent", &view, "create_correspondent")
        }
        Action::UpdateCorrespondent => {
            let id: CorrespondentId = need_id(&a.id, "id", "update_correspondent")?;
            let view = taxonomy::update_correspondent(
                &ctx,
                id,
                CorrespondentPatch {
                    name: a.name.clone(),
                },
            )
            .await?;
            item("correspondent", &view, "update_correspondent")
        }
        Action::CreateTag => {
            let view =
                taxonomy::create_tag(&ctx, need(&a.name, "name", "create_tag")?, a.color.clone())
                    .await?;
            item("tag", &view, "create_tag")
        }
        Action::RenameTag => {
            let id: TagId = need_id(&a.id, "id", "rename_tag")?;
            let view = taxonomy::rename_tag(&ctx, id, need(&a.name, "name", "rename_tag")?).await?;
            item("tag", &view, "rename_tag")
        }
        Action::DeleteTag => {
            let id: TagId = need_id(&a.id, "id", "delete_tag")?;
            let deleted = taxonomy::delete_tag(&ctx, id).await?;
            ToolOutput::json(json!({
                "action": "delete_tag",
                "kind": "tag",
                "item": {
                    "deleted": true,
                    "id": deleted.id,
                    "name": deleted.name,
                    "documents_detached": deleted.documents_detached,
                },
            }))
        }
        Action::CreateType => {
            let view = taxonomy::create_type(
                &ctx,
                NewType {
                    key: need(&a.key, "key", "create_type")?.clone(),
                    labels: need(&a.labels, "labels", "create_type")?.clone(),
                    fields: a.fields.clone().unwrap_or_default(),
                },
            )
            .await?;
            item("type", &view, "create_type")
        }
        Action::UpdateType => {
            let id: TypeId = need_id(&a.id, "id", "update_type")?;
            let view = taxonomy::update_type(
                &ctx,
                id,
                TypePatch {
                    labels: a.labels.clone(),
                    fields: a.fields.clone(),
                },
            )
            .await?;
            item("type", &view, "update_type")
        }
        Action::MergePeople => {
            let into: PersonId = need_id(&a.into_id, "into_id", "merge_people")?;
            let from: PersonId = need_id(&a.from_id, "from_id", "merge_people")?;
            let view = taxonomy::merge_people(&ctx, into, from).await?;
            item("person", &view, "merge_people")
        }
        Action::RelinkPerson => {
            let id: PersonId = need_id(&a.id, "id", "relink_person")?;
            let view = taxonomy::relink_person(&ctx, id).await?;
            item("person", &view, "relink_person")
        }
        Action::MergeCorrespondents => {
            let into: CorrespondentId = need_id(&a.into_id, "into_id", "merge_correspondents")?;
            let from: CorrespondentId = need_id(&a.from_id, "from_id", "merge_correspondents")?;
            let view = taxonomy::merge_correspondents(&ctx, into, from).await?;
            item("correspondent", &view, "merge_correspondents")
        }
    }
}
