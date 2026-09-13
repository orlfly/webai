#!/usr/bin/env bash
# TUI smoke: drive the interactive TUI under a pty and assert a full
# round-trip (prompt -> step -> done) renders without panicking.
# Usage: scripts/tui_smoke.sh [path-to-webai-bin]  (default target/debug/webai)
set -euo pipefail
bin="${1:-target/debug/webai}"
out="$(mktemp /tmp/webai-tui-smoke.XXXX.log)"
log() { printf '%s\n' "$*" >&2; }

{
  sleep 2                 # let the UI reach the event loop
  printf '打开百度\r'      # navigate-intent prompt -> [navigate] step
  sleep 4                 # let the agent runner produce steps + done
  printf '界面上有什么\r'   # read-intent prompt -> [get_text] (page content)
  sleep 4
  printf '\x03'           # Ctrl+C exit
} | script -qec "stty rows 48 cols 80; timeout 24 $bin" "$out" >/dev/null

body=$(python3 - "$out" <<'PY'
import re,sys
d=open(sys.argv[1],'rb').read().decode('utf-8','replace')
print(re.sub(r'\x1b\[[0-9;?]*[a-zA-Z]','',d))
PY
)
if echo "$body" | grep -aq "panicked"; then
  log "FAIL: panic"; tail -c 600 "$out"; rm "$out"; exit 1
fi
# Model output is honest-stub: navigate-intent -> [navigate], then
# read-intent -> [get_text] (page content summary), then done.
if echo "$body" | grep -aqF "[navigate]" \
   && echo "$body" | grep -aqF "[get_text]" \
   && echo "$body" | grep -aq "AI:" \
   && echo "$body" | grep -aq "TUI session loop finished"; then
  log "TUI-SMOKE-OK: navigate+read-intent round-trip rendered + clean exit"
  rm "$out"; exit 0
fi
log "FAIL: round-trip missing (navigate/get_text/done not rendered)"; tail -c 800 "$out"; rm "$out"; exit 1
