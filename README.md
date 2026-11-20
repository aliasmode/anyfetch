# Anyfetch (fork)

For the full upstream documentation, see [README-OG.md](README-OG.md).

This fork adds the features below on top of upstream Anyfetch. Everything else works as upstream describes: [install](README-OG.md#install), [connect your agent](README-OG.md#connect-your-agent-by-hand), [tools](README-OG.md#tools), [Cloudflare checks](README-OG.md#cloudflare-checks), [proxy](README-OG.md#use-a-proxy).

| Feature | Section |
| --- | --- |
| A real browser for pages that need scripts or refuse plain requests | [Browser fallback](#browser-fallback) |
| Output formats, CSS selectors and jq programs | [New inputs](#new-inputs) |
| Choose when page scripts run | [Pages that need their scripts](#pages-that-need-their-scripts) |
| Save fetched content to a file and get a preview | [Saving to a file](#saving-to-a-file) |
| YouTube transcripts | [YouTube](#youtube) |
| No fetches of local, private or cloud metadata addresses | [Local addresses](#local-addresses) |
| Tool schemas that strict clients accept | [Tool schemas](#tool-schemas) |
| Limits and new error codes | [Limits](#limits), [Errors](#errors) |

## Browser fallback

When plain requests are not enough, Anyfetch loads the page in a Chromium-based browser (Chrome, Brave, Edge or Chromium). It drives the browser itself over the DevTools protocol, so it needs no Node, Bun or Playwright install. It does this in two cases:

- A page that holds nothing without its scripts, when Lightpanda is not installed or returns nothing.
- A page that refuses plain requests: a bot check, or HTTP 401, 403 or 503 (Amazon refuses with 503). A browser with your profile often gets through, since it has that profile's cookies. If the browser is refused too, the error says so: `... The browser fallback failed too: HTTP 403`.

So a page that only shows its content after its scripts run is no longer limited to its title and description, as [upstream](README-OG.md#what-it-does-not-do) says, when a browser is available.

Anyfetch picks the browser in this order:

1. A browser already open for remote debugging: `browser.cdpEndpoint` in the [config file](#optional-browser-settings-in-a-config-file), a `DevToolsActivePort` file in the profile folder, or port 9222.
2. A new browser on `PLAYWRIGHT_USERDATA_DIR`, else on `browser.userDataDir` of the config file. It uses `launchOptions.executablePath`, `headless` (default `true`) and `args` from that file.
3. A new browser on its own default profile, only when that profile is not open. Chrome and Edge 136 and later refuse automation of their default profile, so for them Anyfetch asks for a copy straight away.

| Setting | Effect |
| --- | --- |
| `PLAYWRIGHT_USERDATA_DIR=/path/to/profile` | Start the browser on this user data folder. A profile folder inside one, such as `.../User Data/Profile 1`, also works. |
| `PLAYWRIGHT_MCP_CONFIG=/path/to/playwright.json` | Read `browser.userDataDir`, `browser.cdpEndpoint` and `browser.launchOptions` (`executablePath`, `headless`, `args`) from this file, in the Playwright MCP config format. Optional. |
| `ANYFETCH_CHROMIUM=off` | Never use the browser fallback. |

When the browser cannot start, the fetch returns `browser_unavailable` and the message lists the steps to fix it. Anyfetch writes the same text to stderr, which MCP clients keep in their server log. A failed start is not tried again for 5 minutes. The browser Anyfetch started closes after 5 minutes without a fetch, and when Anyfetch exits.

### Use a copy of your profile

A browser holds its profile folder while it runs, and Chrome and Edge refuse automation of their default profile. So give Anyfetch a copy:

1. Close the browser, so that its files are complete.
2. Copy its user data folder to a new folder. See the table below.
3. Set `PLAYWRIGHT_USERDATA_DIR` to the copy and restart the MCP server.

| Browser | Windows user data folder |
| --- | --- |
| Chrome | `%LOCALAPPDATA%\Google\Chrome\User Data` |
| Brave | `%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data` |
| Edge | `%LOCALAPPDATA%\Microsoft\Edge\User Data` |

On macOS they are under `~/Library/Application Support/`, on Linux under `~/.config/`.

```json
{
  "mcpServers": {
    "anyfetch": {
      "command": "anyfetch",
      "args": ["mcp"],
      "env": {
        "PLAYWRIGHT_USERDATA_DIR": "/path/to/profile-copy"
      }
    }
  }
}
```

### Optional: browser settings in a config file

To pick the browser program, show its window or add flags, put them in a JSON file and set `PLAYWRIGHT_MCP_CONFIG` to it. The file uses the format of the [Playwright MCP](https://github.com/microsoft/playwright-mcp) config, so an existing one can be reused, but Anyfetch needs neither Playwright nor that server.

```json
{
  "browser": {
    "userDataDir": "/path/to/profile-copy",
    "launchOptions": {
      "executablePath": "/path/to/chrome",
      "headless": true,
      "args": ["--profile-directory=Default"]
    }
  }
}
```

Notes:

- One browser at a time can open a profile folder. While another browser has it open, Anyfetch reports it as in use.
- To share a running browser, start it with `--remote-debugging-port=9222 --user-data-dir=/path/to/profile-copy`. Anyfetch attaches to port 9222, or to `cdpEndpoint` in the config file.
- The browser opens pages with that profile's cookies, so a page sees you as signed in. Use a profile without accounts you would not want an agent's fetches to use.
- Like Lightpanda, the browser opens only public sites, and is not used together with a proxy.
- Some sites refuse headless browsers. Set `headless` to `false` in the config to show the window.

## Use it from the command line

On top of the [upstream commands](README-OG.md#use-it-from-the-command-line):

```sh
anyfetch fetch https://news.ycombinator.com/ --content html --selector ".titleline > a"
anyfetch fetch https://api.github.com/repos/rust-lang/rust --selector "{name, stars: .stargazers_count}"
anyfetch fetch "https://www.youtube.com/watch?v=<id>" --lang es
anyfetch fetch https://docs.python.org/3.15/whatsnew/3.15.html --out true --preview 40:20
```

`--content`, `--selector`, `--render`, `--lang`, `--out` and `--preview` are the `format`, `selector`, `render`, `lang`, `out` and `preview` inputs of `web_fetch`. The flag is `--content`, not `--format`, because `--format` already picks the output. `search --read` takes `--content`, `--selector`, `--render`, `--out` and `--preview` too.

## New inputs

### `web_fetch`

Added to the [upstream inputs](README-OG.md#web_fetch):

| Input | Meaning |
| --- | --- |
| `format` | Optional. `readable` (default): the article as Markdown. `markdown`: the whole page as Markdown. `text`: plain text, a line per block, no links. `html`: the HTML as served, or as rendered when `render` applies. `json`: the body as JSON, from any content type, run through `jq .`; `not_json` if it does not parse. |
| `selector` | Optional. Keep only part of the page. On HTML, a CSS selector: all matches in page order (`readable` then gives Markdown). On JSON, a jq program run by [jaq](https://github.com/01mf02/jaq): one result is returned as is, several as an array. Without `env`, `input` or `debug`. Limits: 15 seconds, 1 GB of memory, input nesting depth 256. Ignored on other pages that are not HTML: they come back whole. An empty string means not set. |
| `render` | Optional. `"auto"` (default), `"true"` or `"false"`. See [Pages that need their scripts](#pages-that-need-their-scripts). |
| `lang` | Optional. Caption language for YouTube transcripts (yt-dlp is required), for example `en` (default) or `es`. An empty string means `en`. |
| `out` | Optional. Save the content to a file and get a preview: a path, or `"true"`. See [Saving to a file](#saving-to-a-file). |
| `preview` | Optional, only with `out`. Which lines of the saved content come back: `"true"` (default, the first 20), `"false"`, `"N"`, `"-N"` or `"START:COUNT"`. |

The result's `content` is Markdown unless `format` says otherwise. The cache key includes `format`, `selector` and `render`, and `lang` for YouTube URLs.

### `web_fetch_many`

Added to the [upstream inputs](README-OG.md#web_fetch_many):

| Input | Meaning |
| --- | --- |
| `format`, `selector`, `render`, `lang` | Optional. As for `web_fetch`, for each page. |
| `out`, `preview` | Optional. As for `web_fetch`. A path gets all pages in order; `"true"` gives each URL its own file. |

### `web_search`

Added to the [upstream inputs](README-OG.md#web_search):

| Input | Meaning |
| --- | --- |
| `format`, `selector`, `render` | Optional. As for `web_fetch`, for the pages that `read` returns. |
| `out`, `preview` | Optional. Save the results and read pages as Markdown (`"true"`: `.anyfetch/search/<query>.md`). The answer keeps the results without their `content`; the preview is in `content`, and `saved` says where the file is. |
| `searxng_url` | Changed: it may be on this machine or your network, such as `http://localhost:8888`, also without `ANYFETCH_ALLOW_PRIVATE=1`. |

## Pages that need their scripts

[Upstream](README-OG.md#pages-that-need-their-scripts) runs page scripts in Lightpanda only for pages that are just scripts. The `render` input (`--render` on the command line) sets when scripts run. The renderer is Lightpanda, then the [browser fallback](#browser-fallback).

| `render` | Effect |
| --- | --- |
| `"auto"` (default) | Run the page's scripts only when the page is just scripts and a renderer is available. Pages that already work are not rendered, so they stay fast. |
| `"true"` | Render every HTML page, for every `format`. The error is `renderer_missing` if no renderer is available, `blocked_address` if the page is not public or requests go through a proxy, and `render_failed` if the renderer returns no page. |
| `"false"` | Never render. |

With `"false"`, or when no renderer is available, Anyfetch also skips the article extraction on the formats other than `readable`, since it is only needed to spot a page that is just scripts.

## Saving to a file

Set `out` (`--out` on the command line) to write the content to a file. You can then view, share or grep it later, and the model context gets only a preview.

- A path is taken from the server's working directory. It must be in a subfolder of it, not the folder itself, also after links are followed, with no `..`. It must end in `.md`, `.txt`, `.html`, `.json` or `.jsonl`. `git check-ignore` must say it is ignored; outside a git repository, use `"true"`.
- `"true"` picks `.anyfetch/<host>/<path>.<ext>`, with the extension from `format`. `.anyfetch/` gets a `.gitignore` of `*`. If the working directory is not writable, the file goes to the temporary directory.
- An existing file is appended to, never cut. Each Markdown, text or HTML entry starts with `<!-- anyfetch: <url> <retrieved_at> -->`.
- JSON content is saved as JSON Lines: each entry is one compact line with no comment line. This holds for `.jsonl` (the automatic name for JSON content) and for an explicit `.json` path. The preview of JSON shows the pretty-printed content, so `preview_lines` counts the lines of that text, not of the file.
- The answer keeps its other fields. `content` holds the preview, or is left out with `preview: "false"`. `saved` has `path`, `bytes_written`, `appended` and `preview_lines` (such as `1-20 of 340`). A preview stops at 50,000 characters.
- `preview` is a string: `"true"` (the first 20 lines), `"false"`, `"N"` (the first N lines), `"-N"` (the last N), or `"START:COUNT"`: COUNT lines from line START, counting from 1 (`"40:20"` is lines 40-59). JSON `true`, `false`, numbers and `[start, count]` still work.
- A path that breaks these rules gives `invalid_request`; a failed write gives `write_failed`.

## YouTube

A YouTube video URL (`watch?v=`, `youtu.be/`, `/shorts/`, also `m.` and `music.`) returns the transcript as Markdown: the title, the channel and length, then lines such as `[01:05] text`, each about 20 seconds long. Manual captions come before automatic ones. Set `lang` (`--lang` on the command line) for another language; use the full code, such as `pt-BR`, if the video has only that form.

[yt-dlp](https://github.com/yt-dlp/yt-dlp) is required for YouTube transcripts. Put `yt-dlp` on your `PATH`. Without it, or if it fails, the error is `transcript_unavailable`. That code also means the video has no captions in that language.

## Local addresses

By default, `web_fetch` reads only public sites. It refuses this machine (`localhost`, `127.0.0.1`) and private networks (`10.x`, `192.168.x` and similar), also when a public name or a redirect leads there. It always refuses link-local and cloud metadata addresses, such as `169.254.169.254`. The Cloudflare browser, the JavaScript renderer and the browser fallback open only public sites.

| Setting | Effect |
| --- | --- |
| `ANYFETCH_ALLOW_PRIVATE=1` | Also allows this machine and private networks. Link-local and cloud metadata addresses stay blocked. A SearXNG instance may be local or private without it. |

## Tool schemas

Every tool parameter has exactly one JSON schema `type`: no type arrays, no `anyOf`, `oneOf`, `allOf` or `$ref`. Enums are strings. Strict clients, such as those that take only an OpenAPI subset, accept the schemas, and agent harnesses no longer show params as "unknown".

- `render`, `out` and `preview` are strings. `out: "true"` means `.anyfetch/`, not a file named `true`.
- The JSON forms still work: `true`, `false`, numbers and `[start, count]`.
- `provider` and `time` list their values in the schema.

## Limits

| Limit | Value |
| --- | --- |
| Response body | 20 MB, then `too_large`. |
| One fetch, overall | 90 seconds. |
| `web_fetch_many` | At most 20 URLs, 8 fetched at a time. |
| `web_search` `page` | At most 100, then `invalid_request`. |

## Errors

Codes added by this fork, on top of the [upstream codes](README-OG.md#errors):

| Code | Meaning |
| --- | --- |
| `blocked_address` | The URL, a redirect or the name's address leads to a link-local or cloud metadata address, or to a local or private network without `ANYFETCH_ALLOW_PRIVATE=1`. |
| `browser_unavailable` | The browser fallback cannot start. The message lists the steps. See [Browser fallback](#browser-fallback). |
| `render_failed` | The renderer or the browser could not load the page. |
| `renderer_missing` | `render` is `"true"` and no renderer is available. |
| `no_match` | The `selector` matched nothing. |
| `not_json` | `format` is `json` and the body does not parse. |
| `jq_unavailable` | The jq process could not start. |
| `transcript_unavailable` | yt-dlp is missing or failed, or the video has no captions in that language. |
| `write_failed` | `out` could not write the file. |
| `too_large` | The response body, or the jq output, is over 20 MB. |
| `invalid_request` | A bad input, such as a bad selector or an `out` path that breaks the rules. |

## Build from source

You need Rust 1.98, `cmake` and `clang` (the TLS library is BoringSSL). On Windows you also need NASM.
Use whichever suited for your scenario: 
- `cargo install --git https://github.com/aliasmode/anyfetch`
- ` cargo build --release`
- `sudo cargo install --path .`
