"""Scrapling MCP tools (get / stealthy_fetch) called through ScraplingMCPServer, default options, timeout 90 s."""
import asyncio, pathlib, sys, time
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, SITES
from scrapling.core.ai import ScraplingMCPServer
H = pathlib.Path(__file__).parent
mode = sys.argv[1]  # http | stealth
name = {"http": "scrapling-http", "stealth": "scrapling-stealth"}[mode]
(H / "raw" / name).mkdir(parents=True, exist_ok=True)
srv = ScraplingMCPServer()
limit = int(sys.argv[2]) if len(sys.argv) > 2 else len(SITES)
sem = asyncio.Semaphore(8 if mode == "http" else 3)

def clean(s): return str(s).replace("\n", " ").replace("\t", " ")

async def one(i, url):
    async with sem:
        t = time.time(); status = ""
        try:
            if mode == "http":
                r = await asyncio.wait_for(srv.make_request(url, timeout=90), 100)
            else:
                r = await asyncio.wait_for(srv.stealthy_fetch(url, timeout=90000), 100)
            text = "\n".join(r.content); status = r.status
            (H / "raw" / name / f"{i}.txt").write_text(text)
            ok, reason = judge(text)
            judged = (ok, reason)
            if isinstance(status, int) and status >= 400:
                ok, reason = False, f"http {status} ({reason} by judge)"
        except Exception as e:
            ok, reason = False, "error " + clean(repr(e))[:150]
            judged = (False, reason)
            (H / "raw" / name / f"{i}.txt").write_text("")
        dt = time.time() - t
        print(i, url, ok, reason, f"{dt:.1f}", flush=True)
        return (f"{url}\t{'read' if ok else 'not read'}\t{reason}\t{dt:.2f}", f"{url}\t{status}\t{'read' if judged[0] else 'not read'}\t{judged[1]}")

async def main():
    rows = await asyncio.gather(*[one(i + 1, u) for i, u in enumerate(SITES[:limit])])
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(r[0] for r in rows) + "\n")
    (H / "raw" / f"{name}.status.tsv").write_text("url\thttp_status\tjudge_alone\tjudge_reason\n" + "\n".join(r[1] for r in rows) + "\n")
    print("READ", sum(r[0].split("\t")[1] == "read" for r in rows), "of", len(rows))
asyncio.run(main())
