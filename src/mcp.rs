use crate::{ToolError, clearance, fetch, search};
use rmcp::{
    RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::Future;

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchInput {
    query: String,
    #[serde(default)]
    provider: search::Provider,
    /// SearXNG instance base URL. Required for searxng; the instance must enable JSON.
    searxng_url: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct FetchInput {
    url: String,
    /// First content character to return. Use with max_chars to read a long page in parts.
    #[serde(default)]
    offset: usize,
    /// Most content characters to return. total_chars in the result is the full length.
    max_chars: Option<usize>,
}

#[derive(Clone)]
struct Server {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Server {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "web_search",
        description = "No-key web search. Defaults to DuckDuckGo. Select searxng explicitly to use your own instance; no automatic fallback. SearXNG requires searxng_url and an instance with JSON enabled. Returns source URLs and snippets."
    )]
    async fn web_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(
            context,
            search::search(&input.query, input.provider, input.searxng_url.as_deref()),
        )
        .await
    }

    #[tool(
        name = "web_fetch",
        description = "Fetch a URL with plain requests. Returns readable Markdown or plain text. Page JavaScript is not run. Read a long page in parts with max_chars and offset."
    )]
    async fn web_fetch(
        &self,
        Parameters(input): Parameters<FetchInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            let browser = clearance::browser();
            let page = fetch::fetch_with(&input.url, browser.as_deref()).await?;
            Ok(page.window(input.offset, input.max_chars))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("fetchroam", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Web search and page fetch for research. Requests look like a real browser, so \
                 web_fetch reads many sites that refuse default AI fetchers. Page JavaScript is not run.",
            )
    }
}

async fn respond<T: Serialize>(
    context: RequestContext<RoleServer>,
    operation: impl Future<Output = Result<T, ToolError>>,
) -> CallToolResult {
    let result = tokio::select! {
        biased;
        _ = context.ct.cancelled() => Err(ToolError::new("cancelled", "Request cancelled")),
        result = operation => result,
    };
    match result {
        Ok(value) => CallToolResult::structured(json!(value)),
        Err(error) => CallToolResult::structured_error(json!({ "error": error })),
    }
}

pub async fn serve() -> Result<(), ToolError> {
    let service = Server::new()
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| ToolError::new("mcp_error", error.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|error| ToolError::new("mcp_error", error.to_string()))?;
    Ok(())
}
