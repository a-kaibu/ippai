#!/bin/bash
# Checks sake against Windows' answers kept in tests/winref/golden, locally (no Windows):
# builds the probes (mingw-w64), runs them on sake's node build and compares.
#
#   tests/winref/check.sh [name-filter]
#   SAKE=path/to/sake.js (default target/sake-out/sake.js, built if missing)
#   IPPAI_JIT=0 for the interpreter; PNG=dir for pictures of differing images; FONTS=dir: sake's
#   fonts (tools/ci/sake-fonts.py) for the probes that measure text
#   BROWSER=1: the GPU probes too, in headless Chromium (run-browser.mjs; needs ippai built,
#   web/dist built after sake, and Playwright: PLAYWRIGHT=module path)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
EXE=$ROOT/target/winref/exe
OUT=$ROOT/target/winref/sake
SAKE=${SAKE:-$ROOT/target/sake-out/sake.js}
if [ ! -f "$SAKE" ]; then
  . "$ROOT/tools/ci/emsdk.sh"
  OUT=$(dirname "$SAKE") bash "$ROOT/crates/sake/build.sh"
fi
"$HERE/build.sh" "$EXE" > /dev/null
"$HERE/build-msvc.sh" "$EXE" > /dev/null
rm -rf "$OUT"
SAKE=$SAKE "$HERE/run.sh" "$EXE" "$OUT" "${1:-}" > /dev/null
[ -n "${BROWSER:-}" ] && WEB=${WEB:-$ROOT/web/dist} node "$HERE/run-browser.mjs" "$EXE" "$OUT" "${1:-}" > /dev/null
s=$(python3 "$HERE/golden.py" stale "$EXE")
[ -n "$s" ] && echo "stale or missing Windows answers (the winref workflow refreshes them): $s"
python3 "$HERE/compare.py" "$HERE/golden" "$OUT" --known "$HERE/known.txt" ${PNG:+--png "$PNG"}
