#!/usr/bin/env python3
"""Cross-check the per-session RSS budget against the view-pool budget.

ARCHITECTURE.md §6.1: single WebKit view ~250 MB resident; PRODUCT-DESIGN.md
§6 M-6: per-session gate < 300 MB. With `max_concurrent_sessions = 4` the
projected pool must stay within --pool-budget-mb, else exit 1.
"""
import argparse, sys

ap = argparse.ArgumentParser()
ap.add_argument("--per-session-mb", type=float, default=300.0)
ap.add_argument("--sessions", type=int, default=4)
ap.add_argument("--pool-budget-mb", type=float, default=1250.0)
args = ap.parse_args()

projected = args.per_session_mb * args.sessions
ok = projected <= args.pool_budget_mb
print(f"PROJECTED_MB={projected:.0f} POOL_BUDGET_MB={args.pool_budget_mb:.0f} "
      f"SESSIONS={args.sessions} {'PASS' if ok else 'FAIL'}")
sys.exit(0 if ok else 1)
