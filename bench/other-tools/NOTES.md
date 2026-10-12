# Other tools on the same 97 sites

Run on 2026-10-12 from the same cloud server as the main test, with no proxy. Each tool read each site in [`../sites.txt`](../sites.txt) once. The rule is the `judge` function of [`../bench.py`](../bench.py): a page counts as read when the tool returns at least 500 characters of text and the text is not a bot check. A page that the tool reports with an HTTP error status (400 or higher) counts as not read. Results are in [`../results`](../results).

| Tool | Read | How it ran |
| --- | --- | --- |
| Exa MCP, no API key | 78 | Hosted server `https://mcp.exa.ai/mcp`, tool `web_fetch_exa` with `urls`. |
| Scrapling 0.4.15, stealth browser | 71 | `ScraplingMCPServer().stealthy_fetch(url)`, the function behind the MCP tool `stealthy_fetch`. Headless, Markdown, default options. |
| Scrapling 0.4.15, plain HTTP | 69 | `ScraplingMCPServer().make_request(url)`, the function behind the MCP tool `get`. Default options. |
| Hermes Agent `web_extract` | 68 | `tools.web_tools.web_extract_tool([url])` from the Hermes Agent source, with no web API key. |
| Crawl4AI 0.9.4 | 66 | `AsyncWebCrawler()` with the default headless config, `arun(url)`, raw Markdown. |
| Jina Reader, no API key | 61 | `curl https://r.jina.ai/<url>`, at 20 requests a minute. |
| Playwright MCP 0.0.83 | 52 | `npx @playwright/mcp@latest --headless --isolated --browser chromium`, `browser_navigate`, then the text of `browser_snapshot`. A new server for each URL. |
| DuckDuckGo MCP server | 49 | `uvx duckduckgo-mcp-server`, tool `fetch_content`. |

Two more rows show what a browser user agent alone changes:

| Tool | Read | Default |
| --- | --- | --- |
| curl with the user agent of Chrome (`-A`) | 52 | 46 |
| Reference MCP fetch server with the user agent of Chrome and no robots.txt check | 41 | 39 |

Things to know before you compare:

- Hosted tools (Exa, Jina Reader and the readers behind Hermes Agent) can answer from a stored copy of a page. Exa returned about 3,000 characters for each page, and its copy of the New York Times home page was dated September 16.
- Hermes Agent with no key rotates between the free use of Exa, Parallel, Tavily, Firecrawl and Keenable. A second pass can give a different result for single sites.
- Jina Reader refused 11 of the sites for users with no key at the time of the test ("Anonymous access to domain ... blocked until ..."), and 2 sites are excluded at the request of their owners.
- Scrapling ran twice. The first pass read 67 (plain HTTP) and 73 (stealth). The table shows the second pass.
- The timeout of each tool was set to 90 seconds. Playwright MCP needs `--browser chromium` on a machine with no Chrome, and `--isolated` to run more than one server at the same time.
- Firecrawl is not in the table. After about six pages, its MCP server with no API key answered "You've hit Firecrawl's free MCP rate limit", and it gave the same answer for the next 27 minutes. The server had also made a small number of Firecrawl calls with no key earlier that day, through Hermes Agent.
- The rule is simple. A consent page or a country chooser of more than 500 characters counts as read, for every tool.

Time and memory for one page (the Wikipedia article "Web scraping"), with the program started fresh, three runs. Memory is the peak of the whole process tree, as PSS, so shared memory is counted once. The server had other load, so the times are a rough guide.

| Tool | Time | Peak memory |
| --- | --- | --- |
| Anyfetch | 0.18 to 0.21 s | 20 MB |
| Scrapling, plain HTTP | 2.1 to 2.4 s | 95 MB |
| Crawl4AI | 3.8 s | 453 MB |
| Playwright MCP | 4.0 to 4.1 s | 600 MB |
| Scrapling, stealth browser | 4.1 to 4.5 s | 630 MB |

To run a row again, install the tool and start its script from this folder, for example `python3 run_scrapling.py http`. The scripts write the text that each tool returned to `raw/`.
