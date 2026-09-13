#!/usr/bin/env python3
"""Cross-check measured per-session RSS against the view-pool budget.

ARCHITECTURE.md §6.1: view pool budget (--pool-budget-mb, derived from
single-view ~250 MB resident). Consumption (评审 #70 Major-2): the per-session
value MUST come from a real `rss_sample.py` run (RSS_AVG_MB line), not a
literal constant — a constant-vs-constant comparison is always PASS and
therefore meaningless.

Usage:
    python3 scripts/rss_sample.py ... | python3 scripts/rss_budget_crosscheck.py \
        --sessions 4 --pool-budget-mb 1250 [--per-session-mb <fallback>]
"""
import argparse, re, sys

ap = argparse.ArgumentParser()
ap.add_argument("--sessions", type=int, default=4,
                help="max_concurrent_sessions (agent.toml)")
ap.add_argument("--pool-budget-mb", type=float, default=1250.0)
ap.add_argument("--per-session-mb", type=float, default=None,
                help="fallback only; prefer measured RSS_AVG_MB on stdin")
args = ap.parse_args()

measured = None
for line in sys.stdin.read().splitlines():
    m = re.match(r"RSS_AVG_MB=([0-9.]+)", line.strip())
    if m:
        measured = float(m.group(1))

if measured is None and args.per_session_mb is None:
    print("FAIL: no measured RSS_AVG_MB on stdin and no --per-session-mb fallback")
    sys.exit(2)

per_session = measured if measured is not None else args.per_session_mb
projected = per_session * args.sessions
ok = projected <= args.pool_budget_mb
src = "measured" if measured is not None else "fallback-constant"
print(f"PER_SESSION_MB={per_session:.1f} ({src}) PROJECTED_MB={projected:.0f} "
      f"POOL_BUDGET_MB={args.pool_budget_mb:.0f} SESSIONS={args.sessions} "
      f"{'PASS' if ok else 'FAIL'}")
sys.exit(0 if ok else 1)
