use crate::{ToolError, fetch, search};
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
    /// Keep results from this domain, for example docs.rs. Web engines only.
    site: Option<String>,
    /// How recent the results must be. Web engines only.
    time: Option<search::Time>,
    /// Result page, from 1.
    #[serde(default)]
    page: usize,
    /// Also return the page content of this many top results.
    #[serde(default)]
    read: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct FetchInput {
    url: String,
    /// A question or keywords. Returns only the parts of the page that match, with [...] where
    /// parts are left out, in at most max_chars (default 5000 with a query). Use it when you
    /// need one fact from a long page. offset is not used with a query.
    query: Option<String>,
    /// First content character to return. Use with max_chars to read a long page in parts.
    #[serde(default)]
    offset: usize,
    /// Most content characters to return (default 50000). total_chars in the result is the
    /// full length.
    max_chars: Option<usize>,
    /// Fetch the page again even if a copy from the last 15 minutes exists.
    #[serde(default)]
    fresh: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct FetchManyInput {
    urls: Vec<String>,
    /// A question or keywords. Returns only the parts of each page that match (default 5000
    /// characters per page with a query).
    query: Option<String>,
    /// Most content characters to return per page. By default the pages share 50000.
    max_chars: Option<usize>,
    /// Fetch the pages again even if copies from the last 15 minutes exist.
    #[serde(default)]
    fresh: bool,
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
        description = "Web search without an API key. The default provider tries Brave, then Yahoo, then DuckDuckGo. searxng uses your own instance (needs searxng_url, JSON enabled). Specialist sources: github (repositories), stackoverflow (questions), hackernews (stories), arxiv and papers (research). Returns titles, URLs and snippets. Set read to also get, from the top results, the parts of each page that match the query."
    )]
    async fn web_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            let filters = search::Filters {
                site: input.site,
                time: input.time,
                page: input.page,
            };
            let instance = input.searxng_url.as_deref();
            let mut found =
                search::search_with(&input.query, input.provider, instance, &filters).await?;
            if input.read > 0 {
                let helpers = fetch::Helpers::find();
                search::read_results(&mut found, input.read, helpers.options(false)).await;
            }
            Ok(found)
        })
        .await
    }

    #[tool(
        name = "web_fetch",
        description = "Fetch a URL with plain requests. Returns readable Markdown or plain text. Page JavaScript is not run. Set query to get only the parts of the page that answer a question. Read a long page in parts with max_chars and offset."
    )]
    async fn web_fetch(
        &self,
        Parameters(input): Parameters<FetchInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            let helpers = fetch::Helpers::find();
            let page = fetch::fetch_with(&input.url, helpers.options(input.fresh)).await?;
            Ok(match &input.query {
                Some(query) => {
                    page.focus(query, input.max_chars.unwrap_or(fetch::DEFAULT_QUERY_CHARS))
                }
                None => {
                    let max_chars = input.max_chars.unwrap_or(fetch::DEFAULT_MAX_CHARS);
                    page.window(input.offset, max_chars)
                }
            })
        })
        .await
    }

    #[tool(
        name = "web_fetch_many",
        description = "Fetch several URLs at the same time, each like web_fetch. Returns one entry per URL, in order: the page, or its error."
    )]
    async fn web_fetch_many(
        &self,
        Parameters(input): Parameters<FetchManyInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            let helpers = fetch::Helpers::find();
            let pages = fetch::fetch_many(&input.urls, helpers.options(input.fresh)).await;
            let query = input.query.as_deref();
            Ok(fetch::pages(&input.urls, pages, input.max_chars, query))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anyfetch", env!("CARGO_PKG_VERSION")))
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
