"""Firecrawl keyless, Exa keyless, DuckDuckGo MCP on the 97 sites, judge of bench.py. Usage: hosted.py firecrawl exa ddg"""
import asyncio, json, pathlib, re, sys, time
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, SITES
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.client.streamable_http import streamablehttp_client
HERE = pathlib.Path(__file__).parent
RATE = re.compile(r"rate.?limit|\b429\b|too many requests", re.I)
NAMES = {"firecrawl": "firecrawl-mcp", "exa": "exa-mcp", "ddg": "duckduckgo-mcp"}


def sess(tool):
    if tool == "firecrawl": return streamablehttp_client("https://mcp.firecrawl.dev/v2/mcp")
    if tool == "exa": return streamablehttp_client("https://mcp.exa.ai/mcp")
    return stdio_client(StdioServerParameters(command="uvx", args=["duckduckgo-mcp-server"]))


def evaluate(tool, text):
    """(read, reason) from the raw text a tool returned."""
    if tool == "firecrawl":
        try:
            page = json.loads(text)
        except ValueError:
            return False, "error " + re.sub(r"\s+", " ", text)[:100]
        status = (page.get("metadata") or {}).get("statusCode")
        if isinstance(status, int) and status >= 400:
            return False, f"http {status}"
        return judge(f"{(page.get('metadata') or {}).get('title') or ''}\n{page.get('markdown') or ''}")
    if tool == "ddg":
        if text.startswith("Error"):
            return False, re.sub(r"\s+", " ", text)[:100]
        text = re.sub(r"\n\n---\n\[Content info:[^\]]*\]\s*$", "", text)
    if tool == "exa":
        m = re.search(r"\b(?:HTTP|status)[ :]*([45]\d\d)\b", text[:600], re.I)
        if m or text.lstrip().lower().startswith("error"):
            return False, re.sub(r"\s+", " ", text)[:100]
    return judge(text)


async def run(tool):
    from contextlib import AsyncExitStack
    name = NAMES[tool]
    (HERE / "raw" / name).mkdir(parents=True, exist_ok=True)
    rows, counter = [None] * len(SITES), {"limited": 0}
    queue = asyncio.Queue()
    for item in enumerate(SITES): queue.put_nowait(item)

    async def worker():
        stack, session = None, None
        async def close():
            nonlocal stack, session
            if stack:
                try: await stack.aclose()
                except BaseException: pass
            stack, session = None, None
        while not queue.empty():
            i, url = queue.get_nowait()
            start, ok, reason, text = time.time(), False, "", ""
            for attempt in range(4):
                try:
                    if session is None:
                        stack = AsyncExitStack()
                        streams = await stack.enter_async_context(sess(tool))
                        session = await stack.enter_async_context(ClientSession(streams[0], streams[1]))
                        await session.initialize()
                    args = {"firecrawl": ("firecrawl_scrape", {"url": url, "formats": ["markdown"]}),
                            "exa": ("web_fetch_exa", {"urls": [url]}),
                            "ddg": ("fetch_content", {"url": url})}[tool]
                    res = await asyncio.wait_for(session.call_tool(*args), 90)
                    text = "\n".join(c.text for c in res.content if hasattr(c, "text"))
                except asyncio.TimeoutError:
                    ok, reason, text = False, "timeout 90s", ""
                    await close(); break
                except BaseException as e:
                    if isinstance(e, asyncio.CancelledError): raise
                    text = "EXC " + repr(e)
                    await close()
                    if RATE.search(text) or "429" in text:
                        if attempt < 3:
                            await asyncio.sleep(20 * (attempt + 1)); continue
                        ok, reason = False, "rate limited"; counter["limited"] += 1; break
                    ok, reason = False, "error " + re.sub(r"\s+", " ", str(e))[:100]
                    break
                if RATE.search(text[:500]) and len(text) < 1500:
                    if attempt < 3:
                        await asyncio.sleep(20 * (attempt + 1)); continue
                    ok, reason = False, "rate limited"; counter["limited"] += 1
                    break
                ok, reason = evaluate(tool, text)
                break
            (HERE / "raw" / name / f"{i+1}.txt").write_text(text)
            rows[i] = f"{url}\t{'read' if ok else 'not read'}\t{reason.replace(chr(9), ' ')}\t{time.time()-start:.2f}"
            print(rows[i], flush=True)
        await close()

    await asyncio.gather(*(worker() for _ in range(4)))
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(rows) + "\n")
    print(f"== {name}: {sum(r.split(chr(9))[1]=='read' for r in rows)} of {len(SITES)}; rate limited {counter['limited']}", flush=True)

for t in sys.argv[1:]:
    asyncio.run(run(t))
