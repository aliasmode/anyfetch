use crate::{ToolError, transport};
use clap::ValueEnum;
use rmcp::schemars;
use roxmltree::Document;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lower")]
pub enum Provider {
    #[default]
    Duckduckgo,
    Bing,
    Searxng,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Duckduckgo => "duckduckgo",
            Self::Bing => "bing",
            Self::Searxng => "searxng",
        }
    }

    fn url(self, query: &str, instance: Option<&str>) -> Result<Url, ToolError> {
        let invalid = || {
            ToolError::new(
                "invalid_request",
                "SearXNG requires an HTTP(S) instance base URL",
            )
        };
        let mut url = match self {
            Self::Duckduckgo => Url::parse("https://html.duckduckgo.com/html/").unwrap(),
            Self::Bing => Url::parse("https://www.bing.com/search").unwrap(),
            Self::Searxng => {
                let mut url = Url::parse(instance.ok_or_else(invalid)?).map_err(|_| invalid())?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    return Err(invalid());
                }
                url.set_path(&format!("{}/search", url.path().trim_end_matches('/')));
                url.set_query(None);
                url.set_fragment(None);
                url
            }
        };
        url.query_pairs_mut().append_pair("q", query);
        match self {
            Self::Bing => {
                url.query_pairs_mut().append_pair("format", "rss");
            }
            Self::Searxng => {
                url.query_pairs_mut().append_pair("format", "json");
            }
            Self::Duckduckgo => {}
        }
        Ok(url)
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub provider: &'static str,
    pub status: u16,
    pub source_url: String,
    pub retrieved_at: String,
    pub results: Vec<SearchHit>,
}

pub async fn search(
    query: &str,
    provider: Provider,
    searxng_url: Option<&str>,
) -> Result<SearchResponse, ToolError> {
    let result = async {
        let url = provider.url(query, searxng_url)?;
        let response = transport::get(url.as_str(), |_| true).await?;
        let body = response.text();
        let results = match provider {
            Provider::Duckduckgo => parse_duckduckgo(&body, response.status),
            Provider::Bing => parse_results(&body, response.status),
            Provider::Searxng => parse_searxng(&body, response.status),
        }?;
        Ok(SearchResponse {
            query: query.to_owned(),
            provider: provider.name(),
            status: response.status,
            source_url: response.url,
            retrieved_at: crate::retrieved_at(),
            results,
        })
    }
    .await;
    result.map_err(|mut error: ToolError| {
        error.provider = Some(provider.name());
        error
    })
}

pub fn parse_duckduckgo(html: &str, status: u16) -> Result<Vec<SearchHit>, ToolError> {
    let document = dom_query::Document::from(html);
    let error = |code, message| ToolError {
        status: Some(status),
        ..ToolError::new(code, message)
    };
    if status == 202
        || document
            .select("#challenge-form, .anomaly-modal")
            .iter()
            .next()
            .is_some()
    {
        return Err(error(
            "challenge_required",
            "DuckDuckGo requires a challenge; select another provider explicitly",
        ));
    }
    let unexpected = || {
        error(
            "unexpected_response",
            "DuckDuckGo did not return recognizable search results",
        )
    };
    let base = Url::parse("https://html.duckduckgo.com/").unwrap();
    let hits: Vec<_> = document
        .select(".result:not(.result--ad):not(.result--no-result)")
        .iter()
        .map(|node| {
            let link = node.select(".result__a");
            let title = link.text().trim().to_owned();
            let href = link.attr("href").ok_or_else(unexpected)?;
            let mut url = base.join(&href).map_err(|_| unexpected())?;
            if matches!(
                url.host_str(),
                Some("duckduckgo.com" | "html.duckduckgo.com")
            ) && url.path() == "/l/"
            {
                let target = url
                    .query_pairs()
                    .find(|(key, _)| key == "uddg")
                    .map(|(_, value)| value.into_owned())
                    .ok_or_else(unexpected)?;
                url = Url::parse(&target).map_err(|_| unexpected())?;
            }
            if title.is_empty() || !matches!(url.scheme(), "http" | "https") {
                return Err(unexpected());
            }
            Ok(SearchHit {
                title,
                url: url.into(),
                snippet: node.select(".result__snippet").text().trim().to_owned(),
            })
        })
        .collect::<Result<_, ToolError>>()?;
    if hits.is_empty() && document.select(".no-results").iter().next().is_none() {
        return Err(unexpected());
    }
    Ok(hits)
}

pub fn parse_searxng(body: &str, status: u16) -> Result<Vec<SearchHit>, ToolError> {
    #[derive(Deserialize)]
    struct Response {
        results: Vec<Hit>,
    }
    #[derive(Deserialize)]
    struct Hit {
        title: String,
        url: String,
        #[serde(default)]
        content: String,
    }
    let unexpected = || ToolError {
        status: Some(status),
        ..ToolError::new(
            "unexpected_response",
            "SearXNG did not return recognizable JSON search results; the instance must enable JSON",
        )
    };
    let response: Response = serde_json::from_str(body).map_err(|_| unexpected())?;
    response
        .results
        .into_iter()
        .map(|hit| {
            let url = Url::parse(&hit.url).map_err(|_| unexpected())?;
            if hit.title.trim().is_empty() || !matches!(url.scheme(), "http" | "https") {
                return Err(unexpected());
            }
            Ok(SearchHit {
                title: hit.title,
                url: url.into(),
                snippet: hit.content,
            })
        })
        .collect()
}

pub fn parse_results(xml: &str, status: u16) -> Result<Vec<SearchHit>, ToolError> {
    let unexpected = || ToolError {
        status: Some(status),
        ..ToolError::new(
            if status == 202 {
                "provider_unavailable"
            } else {
                "unexpected_response"
            },
            "Bing did not return recognizable RSS search results",
        )
    };
    let document = Document::parse(xml).map_err(|_| unexpected())?;
    let root = document.root_element();
    if !root.has_tag_name("rss") {
        return Err(unexpected());
    }
    let channel = root
        .children()
        .find(|node| node.has_tag_name("channel"))
        .ok_or_else(unexpected)?;
    channel
        .children()
        .filter(|node| node.has_tag_name("item"))
        .map(|item| {
            let text = |name| {
                item.children()
                    .find(|node| node.has_tag_name(name))
                    .and_then(|node| node.text())
                    .unwrap_or("")
                    .trim()
                    .to_owned()
            };
            let title = text("title");
            let url = Url::parse(&text("link")).map_err(|_| unexpected())?;
            if title.is_empty() || !matches!(url.scheme(), "http" | "https") {
                return Err(unexpected());
            }
            Ok(SearchHit {
                title,
                url: url.into(),
                snippet: text("description"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_selection_builds_only_the_selected_endpoint() {
        assert_eq!(Provider::default().name(), "duckduckgo");
        for (provider, host, path, format) in [
            (Provider::Duckduckgo, "html.duckduckgo.com", "/html/", None),
            (Provider::Bing, "www.bing.com", "/search", Some("rss")),
            (
                Provider::Searxng,
                "example.org",
                "/nested/search",
                Some("json"),
            ),
        ] {
            let url = provider
                .url(
                    "Rust & 日本語",
                    Some("https://example.org/nested/?old=value#fragment"),
                )
                .unwrap();
            assert_eq!(url.host_str(), Some(host));
            assert_eq!(url.path(), path);
            let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(pairs["q"], "Rust & 日本語");
            assert_eq!(pairs.get("format").map(|v| v.as_ref()), format);
            assert_eq!(pairs.len(), if format.is_some() { 2 } else { 1 });
            assert!(url.fragment().is_none());
        }
    }
}
