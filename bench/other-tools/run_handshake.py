"""The handshake test: 2 handshakes x 2 header sets on the 97 sites, one request each, no retry.

Holds the header set fixed and changes only the TLS and HTTP/2 handshake, and the reverse. Uses
`examples/handshake.rs`: build it first with `cargo build --example handshake`. A page counts as
read by the rule that bench.py uses for curl: a 2xx status, then `judge` on the text of the HTML.

Usage: python3 run_handshake.py
"""
import json, pathlib, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

BENCH = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BENCH))
from bench import judge, strip_html, SITES

PROGRAM = BENCH.parent / "target/debug/examples/handshake"
CELLS = [("safari", "safari"), ("plain", "safari"), ("safari", "curl"), ("plain", "curl")]


def read(handshake, headers, url):
    done = subprocess.run([str(PROGRAM), handshake, headers, url], capture_output=True, text=True, timeout=90)
    answer = json.loads(done.stdout or '{"error": "no output"}')
    if "error" in answer:
        return False, "error " + answer["error"][:80].replace("\t", " ").replace("\n", " ")
    if not 200 <= answer["status"] < 300:
        return False, f"http {answer['status']}"
    return judge(strip_html(answer["body"]))


for handshake, headers in CELLS:
    def one(url):
        started = time.time()
        try:
            ok, reason = read(handshake, headers, url)
        except Exception as error:
            ok, reason = False, f"tool error: {str(error)[:60]}"
        return f"{url}\t{'read' if ok else 'not read'}\t{reason}\t{time.time() - started:.2f}"
    with ThreadPoolExecutor(8) as pool:
        rows = list(pool.map(one, SITES))
    name = f"handshake-{handshake}-tls-{headers}-headers"
    (BENCH / "results" / f"{name}.tsv").write_text("\n".join(rows) + "\n")
    print(name, sum(row.split("\t")[1] == "read" for row in rows), "of", len(rows), flush=True)
