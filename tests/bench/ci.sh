#!/bin/bash
# One benchmark group as .github/workflows/bench.yml runs it (and runnable anywhere with the same
# tools): builds the programs, serves each build of ippai from its own store (fonts as C:\, the
# programs as the tree "bench"), and runs tests/bench/run.mjs on them, alternating.
#
#   tests/bench/ci.sh OUT.json FONTS GROUP...
#   env: NEW=unpacked build (ippai, web/: tools/ci/build.sh), OLD=the build to compare with
#        (optional), WORK (default $RUNNER_TEMP or /tmp/bench), ROUNDS, PLAYWRIGHT
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=$(realpath -m "$1") FONTS=$(realpath "$2")
shift 2
WORK=${WORK:-${RUNNER_TEMP:-/tmp}/bench}
rm -rf "$WORK" && mkdir -p "$WORK/prefix/drive_c/windows/temp" "$(dirname "$OUT")"
cp -r "$FONTS" "$WORK/prefix/drive_c/windows/Fonts"
"$ROOT/tests/bench/build.sh" "$WORK/exe" >/dev/null

BUILDS="" PIDS=()
trap 'kill "${PIDS[@]}" 2>/dev/null || true' EXIT
port=8091
for name in new old; do
  var=${name^^}
  dist=${!var:-}
  [ -n "$dist" ] || continue
  dist=$(realpath "$dist")
  store=$WORK/store-$name
  "$dist/ippai" --store "$store" pack "$WORK/prefix" prefix32 >/dev/null
  "$dist/ippai" --store "$store" pack "$WORK/exe" bench >/dev/null
  mkdir -p "$WORK/jit-$name"
  "$dist/ippai" serve --foreground --web "$dist/web" --store "$store" --jit "$WORK/jit-$name" \
    --listen "127.0.0.1:$port" --no-prerun > "$WORK/server-$name.log" 2>&1 &
  PIDS+=("$!")
  for _ in $(seq 60); do curl -fs "http://127.0.0.1:$port/play.html" >/dev/null && break; sleep 1; done
  curl -fs "http://127.0.0.1:$port/play.html" >/dev/null || { cat "$WORK/server-$name.log"; exit 1; }
  BUILDS="$BUILDS $name=http://127.0.0.1:$port"
  port=$((port + 10)) # (ippai serve also takes the next port, for web games)
done
BUILDS=$BUILDS node "$ROOT/tests/bench/run.mjs" "$OUT" "$@"
