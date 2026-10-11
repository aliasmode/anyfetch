use clap::{Parser, Subcommand, ValueEnum};
use fetchroam::{fetch, search};
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
    /// Serve web_search and web_fetch over stdio MCP.
    Mcp,
    /// Search without an API key (experimental). Provider changes are explicit; no automatic fallback.
    Search {
        query: String,
        #[arg(long, value_enum, default_value_t = search::Provider::default())]
        provider: search::Provider,
        /// SearXNG instance base URL; the instance must enable JSON search.
        #[arg(long, required_if_eq("provider", "searxng"))]
        searxng_url: Option<String>,
    },
    /// Fetch a page and extract its readable content.
    Fetch {
        url: String,
        #[arg(long, value_enum, default_value_t = Format::Json)]
        format: Format,
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
            return match fetchroam::mcp::serve().await {
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
        } => search::search(&query, provider, searxng_url.as_deref())
            .await
            .map(|value| json!(value)),
        Command::Fetch { url, .. } => fetch::fetch(&url).await.map(|value| json!(value)),
    };
    let (value, success) = match result {
        Ok(value) => (value, true),
        Err(error) => (json!({ "error": error }), false),
    };
    let mut output = std::io::stdout().lock();
    let written = if success && markdown {
        writeln!(output, "{}", value["content"].as_str().unwrap_or_default())
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
