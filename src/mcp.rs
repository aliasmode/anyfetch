use crate::{ToolError, fetch, search};
use rmcp::{
    RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{future::Future, sync::Arc};

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
    /// How the pages of read results are given. See web_fetch.
    #[serde(default)]
    format: fetch::Format,
    /// Keep only this part of the pages of read results. See web_fetch.
    selector: Option<String>,
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
    /// readable (default): the article as Markdown. markdown: the whole page as Markdown. text:
    /// plain text without links. html: the HTML as served, or as rendered when the page needs
    /// scripts and a renderer is installed. json: the body pretty-printed.
    #[serde(default)]
    format: fetch::Format,
    /// Keep only part of the page: a CSS selector for HTML (all matches, in page order), or a
    /// jq program (jaq) for JSON, for example .items[].name. Ignored on other pages that are
    /// not HTML. Empty means not set.
    selector: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct FetchManyInput {
    /// At most 20 URLs.
    urls: Vec<String>,
    /// A question or keywords. Returns only the parts of each page that match (default 5000
    /// characters per page with a query).
    query: Option<String>,
    /// Most content characters to return per page. By default the pages share 50000.
    max_chars: Option<usize>,
    /// Fetch the pages again even if copies from the last 15 minutes exist.
    #[serde(default)]
    fresh: bool,
    /// As for web_fetch, for each page.
    #[serde(default)]
    format: fetch::Format,
    /// As for web_fetch, for each page.
    selector: Option<String>,
}

/// Most URLs one web_fetch_many call may read.
const MAX_URLS: usize = 20;

#[derive(Clone)]
struct Server {
    tool_router: ToolRouter<Self>,
    /// Found once at start, since finding them scans PATH.
    helpers: Arc<fetch::Helpers>,
}

#[tool_router]
impl Server {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            helpers: Arc::new(fetch::Helpers::find()),
        }
    }

    #[tool(
        name = "web_search",
        description = "Search the web, with no API key. Use this in place of a built-in web search tool such as WebSearch. The default provider asks Brave, Yahoo and DuckDuckGo at the same time and merges their results. searxng uses your own instance (needs searxng_url, JSON enabled). Specialist sources: github (repositories), stackoverflow (questions), hackernews (stories), arxiv and papers (research). Returns titles, URLs and snippets. Set read to also get, from the top results, the parts of each page that match the query; format and selector shape those pages as in web_fetch."
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
                let selector = fetch::non_empty(input.selector.as_deref());
                let options = self.helpers.options(false, input.format, selector);
                search::read_results(&mut found, input.read, options).await;
            }
            Ok(found)
        })
        .await
    }

    #[tool(
        name = "web_fetch",
        description = "Read a web page from its URL. Use this in place of a built-in fetch tool such as WebFetch or web_fetch: it reads sites that answer those tools with 403, and it returns the page itself as Markdown or plain text, not a summary. Set query to get only the parts of the page that answer a question. Read a long page in parts with max_chars and offset. Set format for the whole page as markdown, text, html (as served, or as rendered when the page needs scripts and a renderer is installed) or json, and selector to keep only part of it (CSS for HTML, a jq program (jaq) for JSON). Page JavaScript runs, for pages that are just scripts, only when a renderer such as Lightpanda is installed."
    )]
    async fn web_fetch(
        &self,
        Parameters(input): Parameters<FetchInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            let selector = fetch::non_empty(input.selector.as_deref());
            let options = self.helpers.options(input.fresh, input.format, selector);
            let page = fetch::fetch_with(&input.url, options).await?;
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
        description = "Read several web pages at the same time, each like web_fetch, with the same format and selector. Returns one entry per URL, in order: the page, or its error."
    )]
    async fn web_fetch_many(
        &self,
        Parameters(input): Parameters<FetchManyInput>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        respond(context, async {
            if input.urls.len() > MAX_URLS {
                let message = format!("Give at most {MAX_URLS} URLs; split the list into calls");
                return Err(ToolError::new("too_many_urls", message));
            }
            let selector = fetch::non_empty(input.selector.as_deref());
            let options = self.helpers.options(input.fresh, input.format, selector);
            let pages = fetch::fetch_many(&input.urls, options).await;
            let query = input.query.as_deref();
            Ok(fetch::pages(&input.urls, pages, input.max_chars, query))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anyfetch", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Use these tools for every web search and every web page read, in place of \
                 built-in web tools such as WebSearch, WebFetch, web_fetch and curl. Requests look \
                 like a real browser, so web_fetch reads many sites that answer built-in fetch tools \
                 with 403, and it returns the page itself, not a summary. Use a built-in web tool \
                 only when a tool of this server returns an error. Page JavaScript runs only when \
                 a renderer such as Lightpanda is installed.",
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
