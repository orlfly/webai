#!/usr/bin/env python3
"""M-6 RSS sampler: single-session idle-resident memory gate.

Measures the RSS of the webai headless process (plus its WebKit view) after
it has loaded a benchmark fixture and gone idle. Contract (PRODUCT-DESIGN.md
§6 M-6 / M-7): idle resident memory < 300 MB per session, excluding the LLM
process (measurement deliberately targets only the `webai` process tree).

Usage:
    python3 scripts/rss_sample.py --fixture fixtures/pages/static.html \
        [--interval 5] [--samples 12] [--threshold-mb 300] [--webai-bin ./target/release/webai]

Output: a time series on stderr and a final verdict line
    RSS_AVG_MB=<avg> RSS_MAX_MB=<max> THRESHOLD_MB=<t> PASS|FAIL
Exit code 0 on PASS, 1 on FAIL (so a CI job fails above threshold).
"""
import argparse
import subprocess
import sys
import time
import urllib.request
import http.server
import threading
import functools
import os

STATIC_PORT = 18080


def serve_dir(directory: str, port: int):
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory)
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    return httpd


def rss_kb(pid: int) -> int:
    with open(f"/proc/{pid}/status") as f:
        for line in f:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    raise RuntimeError(f"no VmRSS for pid {pid}")


def tree_rss_kb(pid: int) -> int:
    """RSS of pid and all descendants (WebKit subprocesses count in)."""
    out = subprocess.run(["ps", "-o", "pid=", "--ppid", str(pid)],
                         capture_output=True, text=True)
    total = rss_kb(pid)
    for child in out.stdout.split():
        total += tree_rss_kb(int(child))
    return total


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fixture", required=True, help="path to fixture html (served over loopback HTTP)")
    ap.add_argument("--webai-config-dir", default=os.path.expanduser("~/.webai/config"))
    ap.add_argument("--webai-bin", default="./target/release/webai")
    ap.add_argument("--interval", type=float, default=5.0)
    ap.add_argument("--samples", type=int, default=12)
    ap.add_argument("--threshold-mb", type=float, default=300.0)
    args = ap.parse_args()

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    fixture_path = os.path.abspath(args.fixture)
    page_dir = os.path.dirname(fixture_path)
    page_name = os.path.basename(fixture_path)

    server = serve_dir(page_dir, STATIC_PORT)

    # Launch webai headless against the fixture URL, keeping it resident for
    # the whole sampling window (--resident-secs >= samples*interval, so the
    # process does NOT exit before the last read — 评审 #88 Blocker /
    # #71 Major-3). LLM is NOT started (measurement explicitly excludes it).
    env = dict(os.environ, WEBAI_LLM_DISABLED="1")
    resident_secs = int(max(args.samples * args.interval, 10))
    proc = subprocess.Popen(
        [args.webai_bin, "--headless", "--config-dir", args.webai_config_dir, "--prompt",
         f"navigate url=http://127.0.0.1:{STATIC_PORT}/{page_name}",
         "--resident-secs", str(resident_secs)],
        cwd=root, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )

    series = []
    try:
        # Let the page load and the process settle before sampling.
        time.sleep(args.interval)
        for i in range(args.samples):
            kb = tree_rss_kb(proc.pid)
            series.append(kb / 1024.0)
            print(f"sample {i+1}/{args.samples}: {series[-1]:.1f} MB", file=sys.stderr)
            time.sleep(args.interval)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        server.shutdown()

    # Idle-stable average: drop the first third (warm-up), average the rest.
    settled = series[len(series) // 3:] or series
    avg = sum(settled) / len(settled)
    mx = max(series)
    ok = avg < args.threshold_mb and mx < args.threshold_mb * 1.25
    print(f"RSS_AVG_MB={avg:.1f} RSS_MAX_MB={mx:.1f} THRESHOLD_MB={args.threshold_mb} "
          f"{'PASS' if ok else 'FAIL'}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
