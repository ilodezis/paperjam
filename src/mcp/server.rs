//! The rmcp handler: lists the tools of `tools.json` and routes calls to
//! [`tools::call`].

use std::sync::{Arc, OnceLock};

use axum::http::{self, header};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListResourcesResult, ListToolsResult, MetaObject, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use serde::Deserialize;
use serde_json::json;

use crate::error::ServiceError;
use crate::mcp::{McpActor, tools};
use crate::service::{App, links};

/// The upload widget (an MCP app): `create_upload_link` points at it.
const UPLOAD_WIDGET_URI: &str = "ui://paperjam/upload-v2";
const UPLOAD_WIDGET_HTML: &str = include_str!("upload_widget.html");
const APP_MIME: &str = "text/html;profile=mcp-app";

fn upload_widget_meta(base_url: &str) -> MetaObject {
    let serde_json::Value::Object(map) = json!({
        "ui": {
            // The frame may only talk to this server: to upload files and to
            // load the app's fonts.
            "csp": {"connectDomains": [base_url], "resourceDomains": [base_url]},
            // The widget draws its own card in the app's style.
            "prefersBorder": false,
        }
    }) else {
        unreachable!("a JSON object literal")
    };
    MetaObject(map)
}

#[derive(Deserialize)]
struct Contract {
    instructions: String,
    tools: Vec<Tool>,
}

fn contract() -> &'static Contract {
    static CONTRACT: OnceLock<Contract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        serde_json::from_str(tools::CONTRACT).expect("src/mcp/tools.json is a valid contract")
    })
}

#[derive(Clone)]
pub struct PaperjamMcp {
    app: Arc<App>,
}

impl PaperjamMcp {
    pub fn new(app: Arc<App>) -> Self {
        Self { app }
    }
}

/// A tool failure as the client sees it: `{"error": {"code", "message", "details"?}}`.
fn tool_error(e: &ServiceError) -> CallToolResult {
    let mut error = json!({"code": e.code, "message": e.message});
    if let Some(details) = &e.details {
        error["details"] = details.clone();
    }
    CallToolResult::error(vec![ContentBlock::text(
        json!({"error": error}).to_string(),
    )])
}

impl ServerHandler for PaperjamMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("paperjam", env!("CARGO_PKG_VERSION"))
                .with_title("paperjam document archive"),
        )
        .with_instructions(contract().instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(contract().tools.clone()))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(UPLOAD_WIDGET_URI, "upload_widget")
                .with_title("Upload a scan")
                .with_description("A file picker that sends a scan straight to the archive.")
                .with_mime_type(APP_MIME),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != UPLOAD_WIDGET_URI {
            return Err(McpError::resource_not_found(
                format!("unknown resource {:?}", request.uri),
                None,
            ));
        }
        let host = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get(header::HOST))
            .and_then(|h| h.to_str().ok());
        let base_url = links::base_url(&self.app, host);
        let contents = ResourceContents::text(
            UPLOAD_WIDGET_HTML.replace("__BASE__", &base_url),
            UPLOAD_WIDGET_URI,
        )
        .with_mime_type(APP_MIME)
        .with_meta(upload_widget_meta(&base_url));
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        contract().tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let parts = context
            .extensions
            .get::<http::request::Parts>()
            .ok_or_else(|| McpError::internal_error("the request has no HTTP context", None))?;
        let McpActor(actor) = *parts
            .extensions
            .get::<McpActor>()
            .ok_or_else(|| McpError::internal_error("the request is not authenticated", None))?;
        let host = parts
            .headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok());
        let caller = tools::Caller {
            app: &self.app,
            actor,
            base_url: links::base_url(&self.app, host),
        };

        let name = request.name.as_ref();
        let Some(result) = tools::call(&caller, name, request.arguments).await else {
            return Err(McpError::invalid_params(
                format!("unknown tool {name:?}"),
                None,
            ));
        };
        // Names and outcomes only: arguments and results hold document content.
        let response = match result {
            Ok(output) => {
                tracing::debug!(tool = name, "mcp tool ok");
                let mut content = vec![ContentBlock::text(output.structured.to_string())];
                content.extend(output.extra);
                let mut result = CallToolResult::structured(output.structured);
                result.content = content;
                result
            }
            Err(e) => {
                tracing::debug!(tool = name, code = e.code, "mcp tool failed");
                tool_error(&e)
            }
        };
        Ok(response.into())
    }
}
