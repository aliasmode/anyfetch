"""Playwright MCP (@playwright/mcp 0.0.83) via stdio client: browser_navigate, then browser_snapshot text. Fresh server per URL, 3 at a time."""
import asyncio, os, pathlib, sys, time
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, SITES
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
H = pathlib.Path(__file__).parent
name = "playwright-mcp"
(H / "raw" / name).mkdir(parents=True, exist_ok=True)
limit = int(sys.argv[1]) if len(sys.argv) > 1 else len(SITES)
sem = asyncio.Semaphore(3)
def clean(s): return str(s).replace("\n", " ").replace("\t", " ")
def txt(r): return "\n".join(getattr(c, "text", "") or "" for c in r.content)

async def session_run(url):
    p = StdioServerParameters(command="npx", args=["@playwright/mcp@latest", "--headless", "--isolated", "--browser", "chromium", "--timeout-navigation", "90000"],
                              env=dict(os.environ), cwd=str(H / "raw"))
    async with stdio_client(p) as (r, w):
        async with ClientSession(r, w) as s:
            await s.initialize()
            nav = await s.call_tool("browser_navigate", {"url": url})
            if nav.is_error:
                return txt(nav), None
            snap = await s.call_tool("browser_snapshot", {})
            return txt(nav), txt(snap)

async def one(i, url):
    async with sem:
        t = time.time()
        try:
            nav, snap = await asyncio.wait_for(session_run(url), 120)
            (H / "raw" / name / f"{i}.nav.txt").write_text(nav)
            if snap is None:
                (H / "raw" / name / f"{i}.txt").write_text("")
                ok, reason = False, "navigate error: " + clean(nav)[:150]; judged = (False, reason)
            else:
                (H / "raw" / name / f"{i}.txt").write_text(snap)
                ok, reason = judged = judge(snap)
        except BaseException as e:
            ok, reason = False, "error " + clean(repr(e))[:150]; judged = (False, reason)
            (H / "raw" / name / f"{i}.txt").write_text("")
        dt = time.time() - t
        print(i, url, ok, reason, f"{dt:.1f}", flush=True)
        return (f"{url}\t{'read' if ok else 'not read'}\t{reason}\t{dt:.2f}", f"{url}\t\t{'read' if judged[0] else 'not read'}\t{judged[1]}")

async def main():
    rows = await asyncio.gather(*[one(i + 1, u) for i, u in enumerate(SITES[:limit])])
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(r[0] for r in rows) + "\n")
    (H / "raw" / f"{name}.status.tsv").write_text("url\thttp_status\tjudge_alone\tjudge_reason\n" + "\n".join(r[1] for r in rows) + "\n")
    print("READ", sum(r[0].split("\t")[1] == "read" for r in rows), "of", len(rows))
asyncio.run(main())
