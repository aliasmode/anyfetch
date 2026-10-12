"""Crawl4AI AsyncWebCrawler, default BrowserConfig (headless), arun with page_timeout 90 s, result Markdown."""
import asyncio, pathlib, sys, time
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, SITES
from crawl4ai import AsyncWebCrawler, CrawlerRunConfig
H = pathlib.Path(__file__).parent
name = "crawl4ai"
(H / "raw" / name).mkdir(parents=True, exist_ok=True)
limit = int(sys.argv[1]) if len(sys.argv) > 1 else len(SITES)
sem = asyncio.Semaphore(3)
def clean(s): return str(s).replace("\n", " ").replace("\t", " ")

async def one(crawler, i, url):
    async with sem:
        t = time.time(); status = ""
        try:
            r = await asyncio.wait_for(crawler.arun(url=url, config=CrawlerRunConfig(page_timeout=90000)), 100)
            status = r.status_code
            text = str(r.markdown) if r.markdown else ""
            (H / "raw" / name / f"{i}.txt").write_text(text)
            judged = judge(text)
            if not r.success:
                ok, reason = False, "crawl failed: " + clean(r.error_message)[:150]
            elif isinstance(status, int) and status >= 400:
                ok, reason = False, f"http {status} ({judged[1]} by judge)"
            else:
                ok, reason = judged
        except Exception as e:
            ok, reason = False, "error " + clean(repr(e))[:150]
            judged = (False, reason)
            (H / "raw" / name / f"{i}.txt").write_text("")
        dt = time.time() - t
        print(i, url, ok, reason, f"{dt:.1f}", flush=True)
        return (f"{url}\t{'read' if ok else 'not read'}\t{reason}\t{dt:.2f}", f"{url}\t{status}\t{'read' if judged[0] else 'not read'}\t{judged[1]}")

async def main():
    async with AsyncWebCrawler() as crawler:
        rows = await asyncio.gather(*[one(crawler, i + 1, u) for i, u in enumerate(SITES[:limit])])
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(r[0] for r in rows) + "\n")
    (H / "raw" / f"{name}.status.tsv").write_text("url\thttp_status\tjudge_alone\tjudge_reason\n" + "\n".join(r[1] for r in rows) + "\n")
    print("READ", sum(r[0].split("\t")[1] == "read" for r in rows), "of", len(rows))
asyncio.run(main())
