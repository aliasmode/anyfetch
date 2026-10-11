use crate::{ToolError, fetch, transport};
use clap::ValueEnum;
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize, ValueEnum, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lower")]
pub enum Provider {
    // Plain comments here: a documented variant turns the tool's JSON schema from a simple
    // list of names into a list of objects, which some agent harnesses read badly.
    // Brave, then DuckDuckGo when Brave gives no results.
    #[default]
    Auto,
    Brave,
    Duckduckgo,
    Searxng,
    // Repositories on GitHub.
    Github,
    // Questions on Stack Overflow.
    Stackoverflow,
    // Stories on Hacker News.
    Hackernews,
    // Papers on arXiv.
    Arxiv,
    // Papers in OpenAlex, with citation counts and open-access links.
    Papers,
}

/// How recent a result must be. Web engines only.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lower")]
pub enum Time {
    Day,
    Week,
    Month,
    Year,
}

const OPENALEX_FIELDS: &str =
    "id,doi,title,publication_year,cited_by_count,open_access,abstract_inverted_index";

#[derive(Clone, Debug, Default)]
pub struct Filters {
    /// Keep results from this domain. Web engines only.
    pub site: Option<String>,
    pub time: Option<Time>,
    /// The result page, from 1.
    pub page: usize,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Brave => "brave",
            Self::Duckduckgo => "duckduckgo",
            Self::Searxng => "searxng",
            Self::Github => "github",
            Self::Stackoverflow => "stackoverflow",
            Self::Hackernews => "hackernews",
            Self::Arxiv => "arxiv",
            Self::Papers => "papers",
        }
    }

    fn url(self, query: &str, instance: Option<&str>, filters: &Filters) -> Result<Url, ToolError> {
        let invalid = |message: &str| ToolError::new("invalid_request", message);
        let page = filters.page.max(1);
        let site = filters.site.as_deref();
        let with_site = match site {
            Some(site) => format!("{query} site:{site}"),
            None => query.to_owned(),
        };
        let (base, pairs): (&str, Vec<(&str, String)>) = match self {
            Self::Auto => unreachable!("auto is resolved to an engine before a request"),
            Self::Brave => {
                let mut pairs = vec![("q", with_site), ("source", "web".into())];
                if let Some(time) = filters.time {
                    let code = ["pd", "pw", "pm", "py"][time as usize];
                    pairs.push(("tf", code.into()));
                }
                if page > 1 {
                    pairs.push(("offset", (page - 1).to_string()));
                }
                ("https://search.brave.com/search", pairs)
            }
            Self::Duckduckgo => {
                if page > 1 {
                    return Err(invalid("DuckDuckGo gives the first page only"));
                }
                // DuckDuckGo refuses the `site:` operator from many addresses. The domain as a
                // keyword ranks its pages first, and the results are filtered afterwards.
                let query = [query, site.unwrap_or_default()].join(" ");
                let mut pairs = vec![("q", query.trim().to_owned())];
                if let Some(time) = filters.time {
                    pairs.push(("df", ["d", "w", "m", "y"][time as usize].into()));
                }
                ("https://html.duckduckgo.com/html/", pairs)
            }
            Self::Searxng => {
                let mut pairs = vec![("q", with_site), ("format", "json".into())];
                if let Some(time) = filters.time {
                    let range = ["day", "week", "month", "year"][time as usize];
                    pairs.push(("time_range", range.into()));
                }
                if page > 1 {
                    pairs.push(("pageno", page.to_string()));
                }
                let instance_error = || invalid("SearXNG requires an HTTP(S) instance base URL");
                let mut url = Url::parse(instance.ok_or_else(instance_error)?)
                    .map_err(|_| instance_error())?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                    return Err(instance_error());
                }
                url.set_path(&format!("{}/search", url.path().trim_end_matches('/')));
                url.set_query(None);
                url.set_fragment(None);
                url.query_pairs_mut().extend_pairs(pairs);
                return Ok(url);
            }
            Self::Github => (
                "https://api.github.com/search/repositories",
                vec![
                    ("q", query.into()),
                    ("per_page", "10".into()),
                    ("page", page.to_string()),
                ],
            ),
            Self::Stackoverflow => (
                "https://api.stackexchange.com/2.3/search/advanced",
                vec![
                    ("q", query.into()),
                    ("site", "stackoverflow".into()),
                    ("order", "desc".into()),
                    ("sort", "relevance".into()),
                    ("pagesize", "10".into()),
                    ("page", page.to_string()),
                ],
            ),
            Self::Hackernews => (
                "https://hn.algolia.com/api/v1/search",
                vec![
                    ("query", query.into()),
                    ("tags", "story".into()),
                    ("hitsPerPage", "10".into()),
                    ("page", (page - 1).to_string()),
                ],
            ),
            Self::Arxiv => (
                "https://export.arxiv.org/api/query",
                vec![
                    ("search_query", format!("all:{query}")),
                    ("max_results", "10".into()),
                    ("start", ((page - 1) * 10).to_string()),
                ],
            ),
            Self::Papers => (
                "https://api.openalex.org/works",
                vec![
                    ("search", query.into()),
                    ("per-page", "10".into()),
                    ("page", page.to_string()),
                    ("select", OPENALEX_FIELDS.into()),
                ],
            ),
        };
        Ok(Url::parse_with_params(base, pairs).expect("the base URLs are valid"))
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
    /// The page content, when the search was asked to read the results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl SearchHit {
    fn new(title: impl Into<String>, url: impl Into<String>, snippet: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            content: None,
        }
    }
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
    search_with(query, provider, searxng_url, &Filters::default()).await
}

pub async fn search_with(
    query: &str,
    provider: Provider,
    searxng_url: Option<&str>,
    filters: &Filters,
) -> Result<SearchResponse, ToolError> {
    if provider != Provider::Auto {
        return ask(query, provider, searxng_url, filters).await;
    }
    match ask(query, Provider::Brave, None, filters).await {
        Ok(response) if !response.results.is_empty() => Ok(response),
        // When Brave has no answer, DuckDuckGo's answer or error is the one to act on.
        _ => ask(query, Provider::Duckduckgo, None, filters).await,
    }
}

async fn ask(
    query: &str,
    provider: Provider,
    searxng_url: Option<&str>,
    filters: &Filters,
) -> Result<SearchResponse, ToolError> {
    let result = async {
        let url = provider.url(query, searxng_url, filters)?;
        let response = transport::get(url.as_str(), |_| true).await?;
        let body = response.text();
        let mut results = match provider {
            Provider::Auto => unreachable!("auto is resolved to an engine before a request"),
            Provider::Brave => parse_brave(&body, response.status),
            Provider::Duckduckgo => parse_duckduckgo(&body, response.status),
            Provider::Searxng => parse_searxng(&body, response.status),
            Provider::Arxiv => Ok(parse_arxiv(&body)),
            api => serde_json::from_str(&body)
                .map(|data| parse_api(api, &data))
                .map_err(|_| ToolError {
                    status: Some(response.status),
                    ..ToolError::new("unexpected_response", "The source did not return JSON")
                }),
        }?;
        if provider == Provider::Duckduckgo
            && let Some(site) = &filters.site
        {
            results.retain(|hit| {
                let host = Url::parse(&hit.url).ok();
                let host = host.as_ref().and_then(Url::host_str).unwrap_or_default();
                host == site || host.ends_with(&format!(".{site}"))
            });
        }
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

/// Adds the page content to the first `count` results. The pages share the default size.
pub async fn read_results(
    response: &mut SearchResponse,
    count: usize,
    options: fetch::Options<'_>,
) {
    let hits = response.results.iter_mut().take(count);
    let urls: Vec<String> = hits.map(|hit| hit.url.clone()).collect();
    let share = fetch::DEFAULT_MAX_CHARS / urls.len().max(1);
    let pages = fetch::fetch_many(&urls, options).await;
    for (hit, page) in response.results.iter_mut().zip(pages) {
        hit.content = page.ok().map(|page| page.window(0, share).content);
    }
}

pub fn parse_brave(html: &str, status: u16) -> Result<Vec<SearchHit>, ToolError> {
    let document = dom_query::Document::from(html);
    let hits: Vec<_> = document
        .select("div.snippet[data-type=web]")
        .iter()
        .filter_map(|result| {
            let url = result.select("a").attr("href")?;
            let title = result.select(".title").text();
            let snippet = result.select(".generic-snippet .content").text();
            let web = Url::parse(&url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"));
            (web && !title.trim().is_empty())
                .then(|| SearchHit::new(title.trim(), &*url, snippet.trim()))
        })
        .collect();
    if hits.is_empty() {
        return Err(ToolError {
            status: Some(status),
            ..ToolError::new(
                "unexpected_response",
                "Brave did not return recognizable search results",
            )
        });
    }
    Ok(hits)
}

/// Results from the JSON APIs of the specialist sources.
fn parse_api(provider: Provider, data: &Value) -> Vec<SearchHit> {
    let text = |value: &Value| value.as_str().unwrap_or_default().trim().to_owned();
    let day = |value: &Value| text(value).chars().take(10).collect::<String>();
    let list = match provider {
        Provider::Hackernews => &data["hits"],
        Provider::Papers => &data["results"],
        _ => &data["items"],
    };
    let hit = |item: &Value| match provider {
        Provider::Github => SearchHit::new(
            text(&item["full_name"]),
            text(&item["html_url"]),
            format!(
                "{} Stars: {}. Language: {}. Updated: {}.",
                text(&item["description"]),
                item["stargazers_count"],
                text(&item["language"]),
                day(&item["pushed_at"])
            ),
        ),
        Provider::Stackoverflow => {
            let tags: Vec<String> = item["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            let title = dom_query::Document::from(text(&item["title"]))
                .select("body")
                .text();
            SearchHit::new(
                &*title,
                text(&item["link"]),
                format!(
                    "Score: {}. Answers: {}. Tags: {}.",
                    item["score"],
                    item["answer_count"],
                    tags.join(", ")
                ),
            )
        }
        Provider::Hackernews => {
            let discussion = format!(
                "https://news.ycombinator.com/item?id={}",
                text(&item["objectID"])
            );
            let url = Some(text(&item["url"])).filter(|url| !url.is_empty());
            SearchHit::new(
                text(&item["title"]),
                url.unwrap_or_else(|| discussion.clone()),
                format!(
                    "Points: {}. Comments: {}. Date: {}. Discussion: {discussion}",
                    item["points"],
                    item["num_comments"],
                    day(&item["created_at"])
                ),
            )
        }
        _ => {
            // OpenAlex stores an abstract as a map from each word to its positions.
            let positions = item["abstract_inverted_index"].as_object();
            let mut words: Vec<(u64, &str)> = positions
                .into_iter()
                .flatten()
                .flat_map(|(word, places)| {
                    let places = places.as_array().into_iter().flatten();
                    places.filter_map(move |place| Some((place.as_u64()?, word.as_str())))
                })
                .collect();
            words.sort();
            let summary: Vec<&str> = words.into_iter().map(|(_, word)| word).collect();
            // Prefer the link that anyone can read, which is often the PDF.
            let links = [&item["open_access"]["oa_url"], &item["doi"], &item["id"]];
            let url = links.into_iter().map(text).find(|link| !link.is_empty());
            SearchHit::new(
                text(&item["title"]),
                url.unwrap_or_default(),
                format!(
                    "Year: {}. Citations: {}. {}",
                    item["publication_year"],
                    item["cited_by_count"],
                    summary.join(" ")
                ),
            )
        }
    };
    list.as_array().into_iter().flatten().map(hit).collect()
}

/// Results from arXiv's Atom feed.
fn parse_arxiv(xml: &str) -> Vec<SearchHit> {
    let feed = dom_query::Document::from(xml);
    let entries = feed.select("entry");
    let hits = entries.iter().map(|entry| {
        let field = |name: &str| {
            let words: Vec<_> = entry
                .select(name)
                .text()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            words.join(" ")
        };
        let published: String = field("published").chars().take(10).collect();
        SearchHit::new(
            field("title"),
            field("id"),
            format!("Published: {published}. {}", field("summary")),
        )
    });
    hits.collect()
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
            "DuckDuckGo answered this query with a challenge; use the searxng provider or a proxy",
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
            let snippet = node.select(".result__snippet").text();
            Ok(SearchHit::new(title, url, snippet.trim()))
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
            Ok(SearchHit::new(hit.title, url, hit.content))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(url: &Url) -> Vec<(String, String)> {
        url.query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn provider_selection_builds_only_the_selected_endpoint() {
        assert_eq!(Provider::default().name(), "auto");
        let filters = Filters {
            site: Some("example.com".into()),
            time: Some(Time::Month),
            page: 2,
        };
        let instance = Some("https://example.org/nested/?old=value#fragment");
        for (provider, address, expected) in [
            (
                Provider::Brave,
                "https://search.brave.com/search",
                vec![
                    ("q", "Rust & 日本語 site:example.com"),
                    ("source", "web"),
                    ("tf", "pm"),
                    ("offset", "1"),
                ],
            ),
            (
                Provider::Searxng,
                "https://example.org/nested/search",
                vec![
                    ("q", "Rust & 日本語 site:example.com"),
                    ("format", "json"),
                    ("time_range", "month"),
                    ("pageno", "2"),
                ],
            ),
            (
                Provider::Github,
                "https://api.github.com/search/repositories",
                vec![("q", "Rust & 日本語"), ("per_page", "10"), ("page", "2")],
            ),
            (
                Provider::Arxiv,
                "https://export.arxiv.org/api/query",
                vec![
                    ("search_query", "all:Rust & 日本語"),
                    ("max_results", "10"),
                    ("start", "10"),
                ],
            ),
        ] {
            let url = provider.url("Rust & 日本語", instance, &filters).unwrap();
            assert_eq!(&url[..url::Position::AfterPath], address);
            let expected: Vec<_> = expected
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert_eq!(pairs(&url), expected);
            assert!(url.fragment().is_none());
        }
        // DuckDuckGo: first page only, the domain as a keyword, and its own date codes.
        assert!(Provider::Duckduckgo.url("rust", None, &filters).is_err());
        let first = Filters { page: 1, ..filters };
        let url = Provider::Duckduckgo.url("rust", None, &first).unwrap();
        assert_eq!(
            pairs(&url),
            [
                ("q".into(), "rust example.com".into()),
                ("df".into(), "m".into())
            ]
        );
    }

    #[test]
    fn specialist_sources_become_titles_links_and_summaries() {
        let github = json!({"items":[{"full_name":"tokio-rs/tokio","html_url":"https://github.com/tokio-rs/tokio",
            "description":"A runtime.","stargazers_count":30000,"language":"Rust","pushed_at":"2026-10-01T10:00:00Z"}]});
        let hit = &parse_api(Provider::Github, &github)[0];
        assert_eq!(
            (&*hit.title, &*hit.url),
            ("tokio-rs/tokio", "https://github.com/tokio-rs/tokio")
        );
        assert_eq!(
            hit.snippet,
            "A runtime. Stars: 30000. Language: Rust. Updated: 2026-10-01."
        );

        let questions = json!({"items":[{"title":"What does &quot;yield&quot; do?","link":"https://stackoverflow.com/q/1",
            "score":12,"answer_count":3,"tags":["python","generator"]}]});
        let hit = &parse_api(Provider::Stackoverflow, &questions)[0];
        assert_eq!(hit.title, "What does \"yield\" do?");
        assert_eq!(
            hit.snippet,
            "Score: 12. Answers: 3. Tags: python, generator."
        );

        let stories = json!({"hits":[{"title":"Show HN","url":null,"objectID":"42","points":10,"num_comments":2,
            "created_at":"2026-10-11T08:00:00Z"}]});
        let hit = &parse_api(Provider::Hackernews, &stories)[0];
        assert_eq!(hit.url, "https://news.ycombinator.com/item?id=42");
        assert!(
            hit.snippet
                .starts_with("Points: 10. Comments: 2. Date: 2026-10-11.")
        );

        let papers = json!({"results":[{"id":"https://openalex.org/W1","doi":"https://doi.org/10.1/x",
            "title":"A paper","publication_year":2017,"cited_by_count":9,
            "open_access":{"oa_url":"https://example.org/paper.pdf"},
            "abstract_inverted_index":{"show":[1],"We":[0,3],"that":[2]}}]});
        let hit = &parse_api(Provider::Papers, &papers)[0];
        assert_eq!(hit.url, "https://example.org/paper.pdf");
        assert_eq!(hit.snippet, "Year: 2017. Citations: 9. We show that We");
        assert!(parse_api(Provider::Github, &json!({"message":"rate limited"})).is_empty());

        let arxiv = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>query</title>
            <entry><id>http://arxiv.org/abs/1706.03762v7</id><published>2017-06-12T17:57:34Z</published>
            <title>Attention Is All
              You Need</title><summary>  The dominant models.  </summary></entry></feed>"#;
        let hit = &parse_arxiv(arxiv)[0];
        assert_eq!(hit.title, "Attention Is All You Need");
        assert_eq!(hit.url, "http://arxiv.org/abs/1706.03762v7");
        assert_eq!(hit.snippet, "Published: 2017-06-12. The dominant models.");
    }
}
