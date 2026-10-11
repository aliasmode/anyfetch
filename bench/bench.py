#!/usr/bin/env python3
"""Reads every site in sites.txt once with each tool and counts the pages that came back.

A page counts as read when the tool returns at least 500 characters of text and the text does not
look like a bot check. The same rule is applied to every tool.

Usage:
  python3 bench/bench.py anyfetch curl            # tools to run, in order
  python3 bench/bench.py reference                 # needs: pip install mcp-server-fetch
Results go to bench/results/<tool>.tsv and a summary is printed. BENCH_THREADS sets how many
sites are read at the same time (default 8).
"""
import asyncio, html, json, os, pathlib, re, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

HERE = pathlib.Path(__file__).parent
SITES = [line.strip() for line in (HERE / "sites.txt").read_text().splitlines() if line.strip()]
BOT_CHECK = re.compile(
    r"just a moment|access denied|attention required|pardon our interruption|prove your humanity|"
    r"verify you are human|are you a robot|security check|security verification|client challenge|"
    r"not a bot|please enable (js|javascript)|enable javascript and cookies|unusual traffic|"
    r"request unsuccessful|403 forbidden|please hold a moment|authenticating\.\.\.",
    re.I,
)


def judge(text):
    """(read?, reason) for the text a tool returned."""
    text = text or ""
    if BOT_CHECK.search(text[:3000]):
        return False, "bot check"
    if len(text) < 500:
        return False, f"only {len(text)} chars"
    return True, f"{len(text)} chars"


def strip_html(markup):
    markup = re.sub(r"(?is)<(script|style|noscript|svg|template)\b.*?</\1>", " ", markup)
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"(?s)<[^>]+>", " ", markup))).strip()


def anyfetch(url):
    done = subprocess.run(["anyfetch", "fetch", "--fresh", "--max-chars", "200000", url],
                          capture_output=True, text=True, timeout=120)
    page = json.loads(done.stdout or "{}")
    if "error" in page:
        return False, f"{page['error']['code']} {page['error'].get('status', '')}".strip()
    return judge(f"{page.get('title') or ''}\n{page.get('description') or ''}\n{page['content']}")


def curl(url):
    done = subprocess.run(["curl", "-sSL", "--compressed", "-m", "30", "-w", "\n%{http_code}", url],
                          capture_output=True, timeout=60)
    body, _, status = done.stdout.decode("utf-8", "replace").rpartition("\n")
    if not status.startswith("2"):
        return False, f"http {status or 'error'}"
    return judge(strip_html(body))


def reference(url, ignore_robots=False):
    """The reference MCP fetch server, as an agent calls it."""
    from mcp_server_fetch import server

    async def run():
        agent = server.DEFAULT_USER_AGENT_AUTONOMOUS
        if not ignore_robots:
            await server.check_may_autonomously_fetch_url(url, agent)
        return await server.fetch_url(url, agent)

    try:
        content, _ = asyncio.run(asyncio.wait_for(run(), 60))
    except Exception as error:  # the server reports every failure as an exception
        return False, re.sub(r"\s+", " ", str(error))[:60]
    return judge(content)


TOOLS = {
    "anyfetch": anyfetch,
    "curl": curl,
    "reference": reference,
    "reference-ignore-robots": lambda url: reference(url, ignore_robots=True),
}


def main():
    (HERE / "results").mkdir(exist_ok=True)
    for name in sys.argv[1:]:
        tool = TOOLS[name]

        def one(url):
            started = time.time()
            try:
                read, reason = tool(url)
            except Exception as error:
                read, reason = False, f"tool error: {str(error)[:50]}"
            return url, read, reason, time.time() - started

        with ThreadPoolExecutor(int(os.environ.get("BENCH_THREADS", "8"))) as pool:
            rows = list(pool.map(one, SITES))
        lines = [f"{url}\t{'read' if read else 'not read'}\t{reason}\t{seconds:.2f}" for url, read, reason, seconds in rows]
        (HERE / "results" / f"{name}.tsv").write_text("\n".join(lines) + "\n")
        times = sorted(seconds for _, read, _, seconds in rows if read)
        median = times[len(times) // 2] if times else 0
        print(f"{name}: read {sum(read for _, read, _, _ in rows)} of {len(rows)}, median {median:.2f} s per page read")


if __name__ == "__main__":
    main()
