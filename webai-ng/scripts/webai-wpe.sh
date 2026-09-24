#!/usr/bin/env bash
# One-command real-WPE launcher: build (cached) and run webai with the real
# WPE browser on this host, without clobbering the default pure-Rust build
# (the real backend lives in a separate CARGO_TARGET_DIR).
#
# Usage: scripts/webai-wpe.sh [webai args...]   (e.g. scripts/webai-wpe.sh)
#        scripts/webai-wpe.sh --headless --prompt "打开 <url> 然后读取页面内容 最后总结"
#
# Env: WEBAI_CARGO_TARGET overrides the target dir (default target-wpe, which
#      is gitignored); WPE_BACKEND defaults to fdo (offscreen render).
set -euo pipefail
cd "$(dirname "$0")/.."

target_dir="${WEBAI_CARGO_TARGET:-target-wpe}"
export WPE_BACKEND="${WPE_BACKEND:-fdo}"

echo "webai-wpe: building real_backend into ${target_dir}/ (cached after first run)…" >&2
CARGO_TARGET_DIR="$target_dir" cargo build -p webai --features real_backend >&2

exec "$target_dir/debug/webai" "$@"