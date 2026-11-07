#!/bin/bash
# Smoke test of a release binary (GoReleaser's post-build hook): it prints its version, starts
# in the background with a fresh home, serves its built-in page (to a proxy only with its token), answers
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
# (the release's fonts, as install.sh puts them: serve makes sake's C:\ of the store from them)
FONTS=${IPPAI_SMOKE_FONTS:-fonts}
if [ -d "$FONTS" ]; then
  mkdir -p "$HOME/.local/share/ippai"
  cp -R "$FONTS" "$HOME/.local/share/ippai/fonts"
fi
"$BIN" serve --listen 127.0.0.1:$PORT --no-prerun
"$BIN" status
if [ -d "$FONTS" ]; then
  grep -q 'drive_c/windows/Fonts/msgothic.ttf' "$HOME/.local/share/ippai/store/trees/prefix32.tsv" || { echo "smoke: no C:\ made from the fonts"; exit 1; }
fi
curl -fsS "http://127.0.0.1:$PORT/" | grep -qi '<html' || { echo "smoke: no page"; exit 1; }
curl -fsS "http://127.0.0.1:$PORT/games" | grep -q '^\[' || { echo "smoke: no game list"; exit 1; }
# (a proxy that says it forwards: the token; auth.rs)
code=$(curl -s -o /dev/null -w '%{http_code}' -H 'X-Forwarded-For: 192.0.2.1' "http://127.0.0.1:$PORT/games")
[ "$code" = 401 ] || { echo "smoke: forwarded /games without the token: HTTP $code"; exit 1; }
curl -fsS -H 'X-Forwarded-For: 192.0.2.1' -H "Authorization: Bearer $("$BIN" token 2>/dev/null)" "http://127.0.0.1:$PORT/games" | grep -q '^\[' || { echo "smoke: no game list with the token"; exit 1; }
"$BIN" stop
echo "smoke: ok"
