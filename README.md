# Fetchroam

**Web search and web fetch for AI agents. One small binary, no API keys, and it reads the sites that refuse your agent.**

Agent harnesses fetch pages with a bot identity, and a large part of the web answers `403`. Fetchroam sends the request a real browser sends, so your agent gets the page.

| 97 popular sites that block AI agents | Pages read |
| --- | --- |
| Built-in agent fetch (Claude Code `WebFetch`) | 42 |
| **Fetchroam** | **71** |

Measured on 2026-10-11 from a cloud server, one request per site, no proxy. The list covers news, docs, forums, shops and social sites such as The New York Times, Reuters, Bloomberg, The Atlantic, Medium, The Verge, Fandom and Unsplash.

- **`web_search`** — DuckDuckGo results with no key, or your own SearXNG instance.
- **`web_fetch`** — clean Markdown with links, tables and code blocks intact. Long pages can be read in parts.
- **Browser-grade requests** — Safari's TLS, HTTP/2 and header fingerprint. A refused fetch is retried automatically, as the same browser or as a second one.
- **Passes Cloudflare checks** — when a site insists on a JavaScript check, your own Firefox passes it once. After that, Fetchroam reads the site with plain requests.
- **Anubis-aware** — completes Anubis proof-of-work checks, as a browser does.
- **Lightweight** — a 14 MB Rust binary. No bundled browser, no JavaScript engine, no Python, no Node.
- **Works everywhere** — a stdio MCP server for agent harnesses, and a plain CLI for everything else.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dtheDavid/fetchroam/releases/latest/download/fetchroam-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/dtheDavid/fetchroam/releases/latest/download/fetchroam-installer.ps1 | iex"
```

The installer puts `fetchroam` in `~/.local/bin`. Check it with `fetchroam --version`.

## Connect your agent

Each harness needs one command. After that, the agent has `web_search` and `web_fetch`.

**Claude Code**

```sh
claude mcp add --scope user fetchroam -- fetchroam mcp
```

**Codex**

```sh
codex mcp add fetchroam -- fetchroam mcp
```

or in `~/.codex/config.toml`:

```toml
[mcp_servers.fetchroam]
command = "fetchroam"
args = ["mcp"]
```

**Hermes Agent**

```sh
hermes mcp add fetchroam --command fetchroam --args mcp
```

Hermes connects, lists the two tools and asks to enable them. Answer `Y`.

**Pi**

```sh
pi mcp add fetchroam -- fetchroam mcp
```

Pi hides MCP tools behind its code mode by default. To show both tools to the model directly, set the exposure in `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "fetchroam": { "command": "fetchroam", "args": ["mcp"], "exposure": "direct" }
  }
}
```

**Gemini CLI**

```sh
gemini mcp add -s user fetchroam fetchroam mcp
```

**OpenCode** — in `~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "fetchroam": { "type": "local", "command": ["fetchroam", "mcp"], "enabled": true }
  }
}
```

**Cursor** — in `~/.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "fetchroam": { "command": "fetchroam", "args": ["mcp"] }
  }
}
```

**More harnesses**

| Harness | Command |
| --- | --- |
| Cline | `cline mcp add fetchroam --yes -- fetchroam mcp` |
| Amp | `amp mcp add fetchroam -- fetchroam mcp` |
| OpenClaw | `openclaw mcp add fetchroam --command fetchroam --arg mcp` |
| Goose | `goose session --with-extension "fetchroam mcp"` |
| VS Code (Copilot) | `code --add-mcp "{\"name\":\"fetchroam\",\"command\":\"fetchroam\",\"args\":[\"mcp\"]}"` |

Any other MCP client: add a stdio server with the command `fetchroam` and the argument `mcp`.

**No MCP?** Give the agent the CLI. Add this to `AGENTS.md` or the harness's instruction file:

```markdown
## Web access
- Search the web: `fetchroam search "<query>"`
- Read a page: `fetchroam fetch <url> --format markdown`
```

**Make it the default.** To have the agent use Fetchroam and not its built-in tools, turn the built-ins off:

- Claude Code, in `~/.claude/settings.json`: `"permissions": { "deny": ["WebFetch", "WebSearch"] }`
- Codex, in `~/.codex/config.toml`: `web_search = "disabled"`

## Use it from the command line

```sh
fetchroam search "rust http client tls fingerprint"
fetchroam fetch https://www.nytimes.com/section/technology --format markdown
fetchroam fetch https://en.wikipedia.org/wiki/Web_scraping --max-chars 8000 --offset 8000
fetchroam search "site:example.org pricing" --provider searxng --searxng-url https://searx.example.org
```

Output is JSON by default, so scripts and agents can parse it. `--format markdown` prints only the page content.

## Tools

### `web_fetch`

| Input | Meaning |
| --- | --- |
| `url` | The page to read. |
| `max_chars` | Optional. The most content characters to return. |
| `offset` | Optional. The first content character to return. |

The result has `title`, `description`, `content` (Markdown), `total_chars`, `url` (after redirects), `status` and `retrieved_at`. To read a long page in parts, call again with a higher `offset` until it reaches `total_chars`.

HTML becomes Markdown. JSON, plain text and XML feeds come back as they are. A page that renders only with JavaScript returns its title and description.

### `web_search`

| Input | Meaning |
| --- | --- |
| `query` | The search query. |
| `provider` | Optional. `duckduckgo` (default) or `searxng`. |
| `searxng_url` | The base URL of your SearXNG instance. It must have JSON output on. |

Each result has `title`, `url` and `snippet`.

### Errors

A failed call returns an `error` object with a `code` the agent can act on:

| Code | Meaning |
| --- | --- |
| `challenge_required` | The site answered with a bot challenge that needs a browser. |
| `http_error` | The site refused the request. `status` has the HTTP code. |
| `rate_limited` | The site asked to slow down. `retry_after` has the wait, when given. |
| `unsupported_content` | The URL is not text (an image or a PDF, for example). |
| `extraction_failed` | The page has no readable content. |

## Use a proxy

Some sites refuse cloud server addresses whatever the request looks like. If your agent runs on a server, send Fetchroam through a residential proxy. Put one line in `~/.config/fetchroam/proxy`:

```
host:port:username:password
```

Fetchroam then sends every search and fetch through that HTTP proxy. The file holds the password in plain text, so keep it private: `chmod 600 ~/.config/fetchroam/proxy`. Delete the file to go direct again.

## Cloudflare checks

Some sites, Stack Overflow for one, answer every first request with a JavaScript check. Fetchroam handles it in two steps:

1. Your own Firefox passes the check once. It runs hidden, with a throwaway profile, for a few seconds.
2. Fetchroam saves the clearance for that site and goes back to plain requests. On Stack Overflow, one pass is good for a year.

In our test, the first Stack Overflow page took about 6 seconds and the next one under 2 seconds.

This needs Firefox, or a Firefox-based browser, on the machine. Without one, nothing changes: the fetch returns `challenge_required`.

| Setting | Effect |
| --- | --- |
| `FETCHROAM_BROWSER=off` | Never start a browser. |
| `FETCHROAM_BROWSER=/path/to/browser` | Use this Firefox-based browser. |

For an MCP server, set the variable in the server entry. In Claude Code:

```sh
claude mcp add --scope user fetchroam -e FETCHROAM_BROWSER=off -- fetchroam mcp
```

Notes:

- Tested on Linux. On Windows, and for a snap-packaged Firefox (the Ubuntu default), the browser is used only when `FETCHROAM_BROWSER` names it.
- A clearance belongs to the address that earned it, so the browser is not used together with a proxy.
- Saved clearances are in `~/.config/fetchroam/clearance/`. Delete a file to forget a site.

## What it does not do

Fetchroam itself does not run JavaScript, solve CAPTCHAs or log in. Pages behind a CAPTCHA, a paywall or a sign-in wall stay closed, and Fetchroam says so with an error code.

## Build from source

You need Rust 1.98, `cmake` and `clang` (the TLS library is BoringSSL). On Windows you also need NASM.

```sh
cargo install --git https://github.com/dtheDavid/fetchroam
```

## License

Apache-2.0
