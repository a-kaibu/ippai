#!/bin/bash
# Smoke test of a release binary (GoReleaser's post-build hook): it prints its version, starts
# in the background with a fresh home, serves its built-in page (with its token only), answers
# status, and stops.
# Targets this machine cannot run are skipped.
#   tools/ci/smoke.sh BINARY [RUST_TARGET]
set -euo pipefail
BIN=$(realpath "$1")
case "${2:-}" in
  "" | "$(uname -m)"-*) ;;
  *) echo "smoke: skip $2"; exit 0 ;;
esac
export HOME=$(mktemp -d) XDG_RUNTIME_DIR= 
trap '"$BIN" stop >/dev/null 2>&1 || true; rm -rf "$HOME"' EXIT
PORT=$((20000 + RANDOM % 20000))
"$BIN" --version
"$BIN" serve --listen 127.0.0.1:$PORT --no-prerun
"$BIN" status
# (nothing without the token, from this machine either: auth.rs)
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/games")
[ "$code" = 401 ] || { echo "smoke: /games without the token: HTTP $code"; exit 1; }
AUTH="Authorization: Bearer $("$BIN" token 2>/dev/null)"
curl -fsS -H "$AUTH" "http://127.0.0.1:$PORT/" | grep -qi '<html' || { echo "smoke: no page"; exit 1; }
curl -fsS -H "$AUTH" "http://127.0.0.1:$PORT/games" | grep -q '^\[' || { echo "smoke: no game list"; exit 1; }
"$BIN" stop
echo "smoke: ok"
