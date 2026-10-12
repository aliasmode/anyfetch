"""curl and the reference MCP fetch server with a Chrome user agent, on the same 97 sites."""
import asyncio, pathlib, re, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, strip_html, SITES
CHROME = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"


def curl_chrome(url):
    done = subprocess.run(["curl", "-sSL", "--compressed", "-m", "30", "-A", CHROME, "-w", "\n%{http_code}", url], capture_output=True, timeout=60)
    body, _, status = done.stdout.decode("utf-8", "replace").rpartition("\n")
    if not status.startswith("2"):
        return False, f"http {status or 'error'}"
    return judge(strip_html(body))


def reference_chrome(url):
    """The reference server started with --user-agent <Chrome> --ignore-robots-txt."""
    from mcp_server_fetch import server

    async def run():
        return await server.fetch_url(url, CHROME)

    try:
        content, _ = asyncio.run(asyncio.wait_for(run(), 60))
    except Exception as error:
        return False, re.sub(r"\s+", " ", str(error))[:60]
    return judge(content)


TOOLS = {"curl-chrome-user-agent": curl_chrome, "reference-chrome-user-agent": reference_chrome}
for name in sys.argv[1:]:
    def one(url):
        started = time.time()
        try:
            read, reason = TOOLS[name](url)
        except Exception as error:
            read, reason = False, f"tool error: {str(error)[:50]}"
        return f"{url}\t{'read' if read else 'not read'}\t{reason}\t{time.time() - started:.2f}"
    with ThreadPoolExecutor(8) as pool:
        rows = list(pool.map(one, SITES))
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(rows) + "\n")
    print(name, sum(r.split("\t")[1] == "read" for r in rows), "of", len(rows))
