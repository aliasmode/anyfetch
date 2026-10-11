use anyfetch::{fetch, search};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use std::{io::Write, process::ExitCode};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the web tools over stdio MCP.
    Mcp,
    /// Search without an API key. The default provider tries Brave, then Yahoo, then DuckDuckGo.
    Search {
        query: String,
        #[arg(long, value_enum, default_value_t = search::Provider::default())]
        provider: search::Provider,
        /// SearXNG instance base URL; the instance must enable JSON search.
        #[arg(long, required_if_eq("provider", "searxng"))]
        searxng_url: Option<String>,
        /// Keep results from this domain, for example docs.rs. Web engines only.
        #[arg(long)]
        site: Option<String>,
        /// How recent the results must be. Web engines only.
        #[arg(long, value_enum)]
        time: Option<search::Time>,
        /// Result page.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Also return, from this many top results, the parts of each page that match the query.
        #[arg(long, default_value_t = 0)]
        read: usize,
    },
    /// Fetch one or more pages and extract their readable content.
    Fetch {
        #[arg(required = true, value_name = "URL")]
        urls: Vec<String>,
        #[arg(long, value_enum, default_value_t = Format::Json)]
        format: Format,
        /// A question or keywords: return only the parts of each page that match, 5000
        /// characters by default.
        #[arg(long)]
        query: Option<String>,
        /// First content character to return, for one URL without a query.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Most content characters to return per page. The default is 50000, shared between
        /// the pages. The JSON total_chars is the full length.
        #[arg(long)]
        max_chars: Option<usize>,
        /// Fetch again even if a copy from the last 15 minutes exists.
        #[arg(long)]
        fresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Json,
    Markdown,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let markdown = matches!(
        cli.command,
        Command::Fetch {
            format: Format::Markdown,
            ..
        }
    );
    let result = match cli.command {
        Command::Mcp => {
            return match anyfetch::mcp::serve().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Search {
            query,
            provider,
            searxng_url,
            site,
            time,
            page,
            read,
        } => {
            let filters = search::Filters { site, time, page };
            let instance = searxng_url.as_deref();
            let mut found = search::search_with(&query, provider, instance, &filters).await;
            if read > 0
                && let Ok(found) = &mut found
            {
                let helpers = fetch::Helpers::find();
                search::read_results(found, read, helpers.options(false)).await;
            }
            found.map(|value| json!(value))
        }
        Command::Fetch {
            urls,
            query,
            offset,
            max_chars,
            fresh,
            ..
        } => {
            let helpers = fetch::Helpers::find();
            let options = helpers.options(fresh);
            if let ([url], Some(query)) = (urls.as_slice(), &query) {
                let max_chars = max_chars.unwrap_or(fetch::DEFAULT_QUERY_CHARS);
                let page = fetch::fetch_with(url, options).await;
                page.map(|page| json!(page.focus(query, max_chars)))
            } else if let [url] = urls.as_slice() {
                let max_chars = max_chars.unwrap_or(fetch::DEFAULT_MAX_CHARS);
                let page = fetch::fetch_with(url, options).await;
                page.map(|page| {
                    let shown = offset + max_chars.min(page.total_chars.saturating_sub(offset));
                    if markdown && shown < page.total_chars {
                        eprintln!(
                            "Showing characters {offset} to {shown} of {}. Add --offset {shown} for the next part.",
                            page.total_chars
                        );
                    }
                    json!(page.window(offset, max_chars))
                })
            } else {
                let pages = fetch::fetch_many(&urls, options).await;
                Ok(fetch::pages(&urls, pages, max_chars, query.as_deref()))
            }
        }
    };
    let (value, success) = match result {
        Ok(value) => (value, true),
        Err(error) => (json!({ "error": error }), false),
    };
    let mut output = std::io::stdout().lock();
    let written = if success && markdown {
        writeln!(output, "{}", markdown_text(&value))
    } else {
        writeln!(output, "{value}")
    };
    if let Err(error) = written {
        eprintln!("Could not write output: {error}");
        return ExitCode::FAILURE;
    }
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The content of one page, or of several pages under a heading each.
fn markdown_text(value: &serde_json::Value) -> String {
    let content = |page: &serde_json::Value| match page["content"].as_str() {
        Some(content) => content.to_owned(),
        None => page["error"].to_string(),
    };
    let Some(pages) = value["pages"].as_array() else {
        return content(value);
    };
    let sections = pages.iter().map(|page| {
        let url = page["requested_url"].as_str().unwrap_or_default();
        format!("# {url}\n\n{}", content(page))
    });
    sections.collect::<Vec<_>>().join("\n\n---\n\n")
}
