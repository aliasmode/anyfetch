#!/usr/bin/env python3
"""Time and peak memory to read a page: anyfetch against a headless Chromium (Linux only).

Usage: python3 bench/speed.py /path/to/chromium-or-headless_shell
Each page is read once by each program, started fresh, as an agent tool call would.
"""
import os, subprocess, sys, tempfile, time

PAGES = [
    "https://en.wikipedia.org/wiki/Web_scraping",
    "https://www.rust-lang.org/",
    "https://docs.python.org/3/tutorial/controlflow.html",
    "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch",
    "https://react.dev/learn",
    "https://github.com/rust-lang/rust",
    "https://www.bbc.com/news",
    "https://www.theguardian.com/international",
    "https://arstechnica.com/",
    "https://news.ycombinator.com/",
]


def tree_rss_kb(root):
    """Resident memory of a process and all its descendants, from /proc."""
    children = {}
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
            children.setdefault(int(fields[1]), []).append(int(pid))
        except OSError:
            pass
    total, todo = 0, [root]
    while todo:
        pid = todo.pop()
        todo += children.get(pid, [])
        try:
            for line in open(f"/proc/{pid}/status"):
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
        except OSError:
            pass
    return total


def measure(command):
    with tempfile.TemporaryFile() as output:
        started = time.time()
        process = subprocess.Popen(command, stdout=output, stderr=subprocess.DEVNULL)
        peak = 0
        while process.poll() is None:
            peak = max(peak, tree_rss_kb(process.pid))
            if time.time() - started > 60:
                process.kill()
            time.sleep(0.02)
        return time.time() - started, peak / 1024, output.tell()


def median(values):
    return sorted(values)[len(values) // 2]


def main():
    chromium = sys.argv[1]
    programs = {
        "anyfetch": lambda url: ["anyfetch", "fetch", "--fresh", url],
        "headless Chromium": lambda url: [chromium, "--headless", "--no-sandbox", "--disable-gpu", "--dump-dom", url],
    }
    for name, command in programs.items():
        rows = [measure(command(url)) for url in PAGES]
        print(f"{name}: median {median([r[0] for r in rows]):.2f} s, median peak memory {median([r[1] for r in rows]):.0f} MB "
              f"({sum(1 for r in rows if r[2] > 2000)} of {len(rows)} pages returned)")


if __name__ == "__main__":
    main()
