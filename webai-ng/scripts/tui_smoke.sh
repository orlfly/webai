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
  printf 'hello tui\r'    # submit a prompt
  sleep 4                 # let the agent runner produce steps + done
  printf '\x03'           # Ctrl+C exit
} | script -qec "stty rows 24 cols 80; timeout 20 $bin" "$out" >/dev/null

body=$(python3 - "$out" <<'PY'
import re,sys
d=open(sys.argv[1],'rb').read().decode('utf-8','replace')
print(re.sub(r'\x1b\[[0-9;?]*[a-zA-Z]','',d))
PY
)
if echo "$body" | grep -aq "panicked"; then
  log "FAIL: panic"; tail -c 600 "$out"; rm "$out"; exit 1
fi
# Model output is honest-stub: steps [navigate] page loaded, then done.
if echo "$body" | grep -aq "AI:" && echo "$body" | grep -aq "donewebai: TUI session loop finished"; then
  log "TUI-SMOKE-OK: prompt round-trip rendered + clean exit"
  rm "$out"; exit 0
fi
log "FAIL: round-trip missing (prompt/done not rendered)"; tail -c 600 "$out"; rm "$out"; exit 1
