# Anyfetch

**Web search and web fetch for AI agents. One small binary, no API keys, and it reads the sites that refuse your agent.**

Agent harnesses fetch pages with a bot identity, and a large part of the web answers `403`. Anyfetch sends the request a real browser sends, so your agent gets the page.

| 97 popular sites that block AI agents | Pages read |
| --- | --- |
| **Anyfetch** | **75** |
| curl | 47 |
| Reference MCP fetch server | 41 |
| Claude Code built-in `WebFetch` | 36 |

Anyfetch read 21 sites that no other tool in the table could read. Among them: The New York Times, Reuters, Bloomberg, The Washington Post, Medium, Reddit, Stack Overflow and YouTube.

| 30 research questions, each asked twice | Answered correctly both times |
| --- | --- |
| **Agent with Anyfetch** | **30** |
| Same agent with Claude Code built-in `WebSearch` and `WebFetch` | 25 |

Ten of the questions are about a page on a site that blocks AI agents. The built-in fetch could not read any of those 10 pages. Its search found the answer in another source for 5 of them. Anyfetch read all 10. The questions were written from pages that Anyfetch can read, so this group shows what the built-in tools miss.

| One page, started fresh | Time | Peak memory |
| --- | --- | --- |
| **Anyfetch** | **0.19 s** | **17 MB** |
| Headless Chromium | 1.15 s | 544 MB |

Measured on 2026-10-11 from a cloud server, with no proxy. See [how we measured](#benchmark) and how to run it again.

## What your agent gets

- **Search with no key.** Brave, Yahoo and DuckDuckGo results, with domain and date filters, quoted phrases and paging. Specialist sources for GitHub, Stack Overflow, Hacker News, arXiv and research papers. Or your own SearXNG.
- **Search and read in one call.** Ask for the top results together with the result list, and get the parts of each page that match your query.
- **Answers, not whole pages.** Give a question with the URL, and get only the parts of the page that match it. The Python 3.15 release notes are 218,000 characters; the answer to one question about them came back in 2,200.
- **Clean Markdown.** Links, tables and code blocks stay intact. Long pages come in parts.
- **PDFs.** Papers and reports come back as text, page by page.
- **Sites that need JavaScript.** Anyfetch reads the data inside the page, and uses the site's public API or feed for Stack Overflow, Reddit, X posts, PyPI and crates.io. No browser runs. For pages that hold nothing without their scripts, it can use Lightpanda, an optional small engine.
- **Browser-grade requests.** Safari's TLS, HTTP/2 and header fingerprint. A refused fetch is retried automatically, as the same browser or as a second one.
- **Cloudflare checks.** When a site insists on a JavaScript check, your own Firefox passes it once. After that, Anyfetch reads the site with plain requests.
- **Fast on repeat.** Several pages in one call, fetched at the same time. A page fetched in the last 15 minutes comes from a local copy.
- **Lightweight.** A 16 MB Rust binary. No bundled browser, no JavaScript engine, no Python, no Node.
- **Works everywhere.** A stdio MCP server for agent harnesses, and a plain CLI for everything else.

## Install

macOS and Linux:

```sh
curl -fsSL https://aliasmode.github.io/anyfetch/install.sh | sh
```

Windows, in PowerShell:

```powershell
irm https://aliasmode.github.io/anyfetch/install.ps1 | iex
```

The installer puts `anyfetch` in `~/.local/bin`. Check it with `anyfetch --version`.

## Connect your agent

Each harness needs one command. After that, the agent has `web_search`, `web_fetch` and `web_fetch_many`.

**Claude Code**

```sh
claude mcp add --scope user anyfetch -- anyfetch mcp
```

**Codex**

```sh
codex mcp add anyfetch -- anyfetch mcp
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.anyfetch]
command = "anyfetch"
args = ["mcp"]
```

**Hermes Agent**

```sh
hermes mcp add anyfetch --command anyfetch --args mcp
```

Hermes connects, lists the tools and asks to enable them. Answer `Y`.

**Pi**

```sh
pi mcp add anyfetch -- anyfetch mcp
```

Pi hides MCP tools behind its code mode by default. To show the tools to the model directly, set the exposure in `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "anyfetch": { "command": "anyfetch", "args": ["mcp"], "exposure": "direct" }
  }
}
```

**Gemini CLI**

```sh
gemini mcp add -s user anyfetch anyfetch mcp
```

**OpenCode** — in `~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "anyfetch": { "type": "local", "command": ["anyfetch", "mcp"], "enabled": true }
  }
}
```

**Cursor** — in `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "anyfetch": { "command": "anyfetch", "args": ["mcp"] }
  }
}
```

**More harnesses**

| Harness | Command |
| --- | --- |
| Cline | `cline mcp add anyfetch --yes -- anyfetch mcp` |
| Amp | `amp mcp add anyfetch -- anyfetch mcp` |
| OpenClaw | `openclaw mcp add anyfetch --command anyfetch --arg mcp` |
| Goose | `goose session --with-extension "anyfetch mcp"` |
| VS Code (Copilot) | `code --add-mcp "{\"name\":\"anyfetch\",\"command\":\"anyfetch\",\"args\":[\"mcp\"]}"` |

Any other MCP client: add a stdio server with the command `anyfetch` and the argument `mcp`.

**No MCP?** Give the agent the CLI. Add this to `AGENTS.md` or the harness's instruction file:

```markdown
## Web access
- Search the web: `anyfetch search "<query>"`
- Read pages: `anyfetch fetch <url> [<url>...] --format markdown`
```

**Make it the default.** To have the agent use Anyfetch and not its built-in tools, turn the built-ins off:

- Claude Code, in `~/.claude/settings.json`: `"permissions": { "deny": ["WebFetch", "WebSearch"] }`
- Codex, in `~/.codex/config.toml`: `web_search = "disabled"`

## Use it from the command line

```sh
anyfetch search "rust async runtime comparison"
anyfetch search "tokio select" --site docs.rs --time year
anyfetch search "postgres vacuum freeze" --read 3
anyfetch search "retrieval augmented generation" --provider papers
anyfetch fetch https://stackoverflow.com/questions/231767 --format markdown
anyfetch fetch https://arxiv.org/pdf/1706.03762 --max-chars 8000 --offset 8000
anyfetch fetch https://docs.python.org/3.15/whatsnew/3.15.html --query "sampling profiler rate"
anyfetch fetch https://www.rust-lang.org/ https://go.dev/ https://ziglang.org/
```

Output is JSON by default, so scripts and agents can parse it. `--format markdown` prints only the page content.

## Tools

### `web_fetch`

| Input | Meaning |
| --- | --- |
| `url` | The page to read. |
| `query` | Optional. A question or keywords. The result has only the parts of the page that match. |
| `max_chars` | Optional. The most content characters to return. The default is 50,000, or 5,000 with a `query`. |
| `offset` | Optional. The first content character to return. Not used with a `query`. |
| `fresh` | Optional. Fetch again even if a copy from the last 15 minutes exists. |

The result has `title`, `description`, `content` (Markdown), `total_chars`, `url` (after redirects), `status` and `retrieved_at`. To read a long page in parts, call again with a higher `offset` until it reaches `total_chars`. The parts come from the local copy, so the site gets one request.

With a `query`, the page is split at its paragraphs, tables and code blocks. The parts that best match the words of the query come back in page order, under their headings, with `[...]` where parts are left out. The result then has `focused: true`. A page with no matching part comes back from its start. A second question about the same page uses the local copy.

HTML becomes Markdown. A PDF becomes text with `[Page N]` marks. JSON, plain text and XML feeds come back as they are.

### `web_fetch_many`

| Input | Meaning |
| --- | --- |
| `urls` | The pages to read. They are fetched at the same time. |
| `query` | Optional. As for `web_fetch`. Each page gives up to 5,000 characters. |
| `max_chars` | Optional. The most content characters per page. By default the pages share 50,000. |
| `fresh` | Optional. As for `web_fetch`. |

The result has `pages`: one entry per URL, in order. Each entry is the page, or its `error`.

### `web_search`

| Input | Meaning |
| --- | --- |
| `query` | The search query. Quoted phrases work. |
| `provider` | Optional. See the list below. The default is `auto`. |
| `site` | Optional. Keep results from one domain, for example `docs.rs`. |
| `time` | Optional. `day`, `week`, `month` or `year`. |
| `page` | Optional. The result page, from 1. |
| `read` | Optional. Also return, from this many top results, the parts of each page that match the query. |
| `searxng_url` | The base URL of your SearXNG instance, for the `searxng` provider. It must have JSON output on. |

Each result has `title`, `url` and `snippet`, and `content` when you set `read`.

| Provider | Source |
| --- | --- |
| `auto` | Brave, then Yahoo, then DuckDuckGo: each one when the one before gives no results. |
| `brave` | Brave Search. About 20 results per page. |
| `yahoo` | Yahoo Search. 7 results per page. No `year` filter. |
| `duckduckgo` | DuckDuckGo. First page only. |
| `searxng` | Your own SearXNG instance. |
| `github` | GitHub repositories, with stars and language. |
| `stackoverflow` | Stack Overflow questions, with score and answer count. |
| `hackernews` | Hacker News stories, with points and the discussion link. |
| `arxiv` | arXiv papers, with abstracts. |
| `papers` | Research papers from OpenAlex, with citation counts and open-access links. |

`site`, `time` and quoted phrases apply to the web engines. The specialist sources are free public APIs with their own request limits.

### Errors

A failed call returns an `error` object with a `code` the agent can act on:

| Code | Meaning |
| --- | --- |
| `challenge_required` | The site answered with a bot challenge that needs a browser. |
| `http_error` | The site refused the request. `status` has the HTTP code. |
| `rate_limited` | The site asked to slow down. `retry_after` has the wait, when given. |
| `timeout` | The site did not answer within 30 seconds. |
| `unsupported_content` | The URL is not text or a PDF (an image, for example). |
| `extraction_failed` | The page has no readable content. A scanned PDF has no text. |

## Cloudflare checks

Some sites answer every first request with a JavaScript check. Anyfetch handles it in two steps:

1. Your own Firefox passes the check once. It runs hidden, with a throwaway profile, for a few seconds.
2. Anyfetch saves the clearance for that site and goes back to plain requests. On Medium, one pass is good for a year.

In our test, the first page of such a site took about 5 seconds and the next one under 2 seconds.

This needs Firefox, or a Firefox-based browser, on the machine. Without one, nothing changes: the fetch returns `challenge_required`.

| Setting | Effect |
| --- | --- |
| `ANYFETCH_BROWSER=off` | Never start a browser. |
| `ANYFETCH_BROWSER=/path/to/browser` | Use this Firefox-based browser. |

For an MCP server, set the variable in the server entry. In Claude Code:

```sh
claude mcp add --scope user anyfetch -e ANYFETCH_BROWSER=off -- anyfetch mcp
```

Notes:

- Tested on Linux. On Windows, and for a snap-packaged Firefox (the Ubuntu default), the browser is used only when `ANYFETCH_BROWSER` names it.
- A clearance belongs to the address that earned it, so the browser is not used together with a proxy.
- Saved clearances are in `~/.config/anyfetch/clearance/`. Delete a file to forget a site.

## Pages that need their scripts

Most pages give Anyfetch their text without JavaScript: it is in the HTML, in the page's data, or behind a public API. A few hold nothing until their scripts run. MSN, Spotify and Airbnb are examples. For those, Anyfetch can use [Lightpanda](https://github.com/lightpanda-io/browser), a small headless engine without a full browser.

Lightpanda is optional and separate. Install it from its README, put `lightpanda` on your `PATH`, and Anyfetch uses it for the pages that need it. In our test, MSN went from no content to 154,000 characters in 12 seconds, and Spotify from no content to 23,000 characters in 3 seconds. Pages that already work are not rendered, so they stay fast.

| Setting | Effect |
| --- | --- |
| `ANYFETCH_RENDERER=off` | Never start the renderer. |
| `ANYFETCH_RENDERER=/path/to/lightpanda` | Use this program. |

Notes:

- Lightpanda makes its own requests. It is not used together with a proxy, and a site with a bot check can refuse it.
- A rendered application page, such as a feed or a listing, comes back as the whole page in Markdown.

## Use a proxy

Some sites refuse cloud server addresses whatever the request looks like. If your agent runs on a server, send Anyfetch through a residential proxy. Put one line in `~/.config/anyfetch/proxy`:

```
host:port:username:password
```

Anyfetch then sends every search and fetch through that HTTP proxy. The file holds the password in plain text, so keep it private: `chmod 600 ~/.config/anyfetch/proxy`. Delete the file to go direct again.

## What it does not do

Anyfetch itself does not run JavaScript, solve CAPTCHAs or log in. A page that shows its content only after its scripts run, with no data inside the HTML and no public API, returns its title and description unless Lightpanda is installed. Pages behind a CAPTCHA, a paywall or a sign-in wall stay closed, and Anyfetch says so with an error code.

## Benchmark

The numbers at the top come from two scripts and one question set in [`bench/`](bench).

- **Pages read.** `bench/bench.py` reads each of the 97 sites in [`bench/sites.txt`](bench/sites.txt) once with each tool. A page counts as read when the tool returns at least 500 characters of text and the text is not a bot check. The same rule is applied to every tool. Anyfetch ran three times and read 73, 76 and 75 pages; the table shows the middle run. Claude Code's `WebFetch` cannot be scripted, so an agent called it once per site and recorded if real content came back.
- **Time and memory.** `bench/speed.py` reads 10 pages with each program, started fresh for every page, and reports the median.

- **Research questions.** [`bench/tasks.json`](bench/tasks.json) has 30 questions. Each has one exact answer and the sentence on the source page that proves it. The pages are from July to October 2026, so a model cannot answer from memory. A Claude Sonnet agent answered the questions with Anyfetch only, and again with the built-in `WebSearch` and `WebFetch` of Claude Code only. The instructions were the same: at most 8 web calls for each question, and the answer `UNKNOWN` when the tools do not give it. Anyfetch ran with no proxy, no Firefox and no Lightpanda. Each tool ran twice, and a question counts when both runs are correct.

| Question group | Anyfetch | Built-in tools |
| --- | --- | --- |
| A page on a site that blocks AI agents | 10 of 10 | 5 of 10 |
| A page on an open site | 10 of 10 | 10 of 10 |
| No URL: search first, then read | 10 of 10 | 10 of 10 |

In each single run the built-in tools answered 26 questions, but not the same 26. Three things to know about this test. The question writer used Anyfetch to check each answer, so the first group has only pages that Anyfetch can read. Four more sites on the writer's list (Reuters, Bloomberg, Financial Times and Quora) have no question, because it could not read an article page there. The site test above reads the home page of Reuters and Bloomberg, which Anyfetch can read. One Anyfetch test agent stopped after its first question, so its 5 questions were run again. Every answer is in [`bench/results/tasks.tsv`](bench/results/tasks.tsv).

The per-site results are in [`bench/results/`](bench/results). Sites change their rules often, and results differ between networks, so run it yourself:

```sh
python3 bench/bench.py anyfetch curl
pip install mcp-server-fetch && python3 bench/bench.py reference
python3 bench/speed.py /path/to/chromium
```

## Build from source

You need Rust 1.98, `cmake` and `clang` (the TLS library is BoringSSL). On Windows you also need NASM.

```sh
cargo install --git https://github.com/aliasmode/anyfetch
```

## License

Apache-2.0
