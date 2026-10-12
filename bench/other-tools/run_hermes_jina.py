"""Hermes Agent web_extract and Jina Reader on the same 97 sites, with the judge of bench.py.

Usage: run_hermes_jina.py jina-reader
       HERMES_AGENT=~/.hermes/hermes-agent <its venv>/bin/python run_hermes_jina.py hermes-web-extract
"""
import asyncio, json, os, pathlib, re, subprocess, sys, time
BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, SITES
OUT = BENCH / "results"


def hermes(url):
    sys.path.insert(0, os.environ["HERMES_AGENT"])  # the folder of the Hermes Agent source
    from tools.web_tools import web_extract_tool
    data = json.loads(asyncio.run(web_extract_tool([url], char_limit=200000)))
    results = data.get("results") or [data]
    page = results[0]
    if page.get("error"):
        return False, str(page["error"])[:120].replace("\n", " ").replace("\t", " ")
    return judge(f"{page.get('title') or ''}\n{page.get('content') or ''}")


def jina(url):
    done = subprocess.run(["curl", "-sS", "-m", "60", "-w", "\n%{http_code}", "https://r.jina.ai/" + url],
                          capture_output=True, timeout=90)
    body, _, status = done.stdout.decode("utf-8", "replace").rpartition("\n")
    time.sleep(3.2)  # 20 requests per minute without a key
    if not status.startswith("2"):
        return False, f"reader http {status or 'error'}: {body[:80]}".replace("\n", " ").replace("\t", " ")
    warning = re.search(r"Warning: Target URL returned error (\d+)", body[:2000])
    if warning:
        return False, f"http {warning.group(1)}"
    text = body.split("Markdown Content:", 1)[-1]
    title = re.search(r"^Title: (.*)$", body, re.M)
    return judge(f"{title.group(1) if title else ''}\n{text}")


TOOLS = {"hermes-web-extract": hermes, "jina-reader": jina}
for name in sys.argv[1:]:
    rows, read = [], 0
    for url in SITES:
        start = time.time()
        try:
            ok, reason = TOOLS[name](url)
        except Exception as error:
            ok, reason = False, f"error {str(error)[:100]}".replace("\n", " ").replace("\t", " ")
        read += ok
        rows.append(f"{url}\t{'read' if ok else 'not read'}\t{reason}\t{time.time() - start:.2f}")
        print(rows[-1], flush=True)
    (OUT / f"{name}.tsv").write_text("\n".join(rows) + "\n")
    print(f"== {name}: {read} of {len(SITES)}", flush=True)
