#!/bin/bash
# sake.wasm's own decoders in wasm under node (V8, as the page runs them): crates/decbench.
#
#   tests/decbench/run.sh test               the SIMD128 code against its scalar code, byte for
#                                            byte (crates/decbench/tests, crates/mpeg1's tests)
#   tests/decbench/run.sh bench [REPS] [ONLY] decbench built with and without SIMD128, plain and
#                                            as on Android (--wasm-enforce-bounds-checks), on
#                                            inputs made with ffmpeg (a 1280x720 PNG and JPEG,
#                                            5 s of 640x480 MPEG-1)
#
# WORK=dir keeps the builds (default: a temporary directory). Emscripten from tools/ci/emsdk.sh.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-$(mktemp -d)}
mkdir -p "$WORK"
. "$ROOT/tools/ci/emsdk.sh"
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUNNER=node
LINK="-C link-arg=-sALLOW_MEMORY_GROWTH=1 -C link-arg=-sNODERAWFS=1 -C link-arg=-sERROR_ON_UNDEFINED_SYMBOLS=0"
cd "$ROOT"
case "${1:-test}" in
  test)
    RUSTFLAGS="-C target-feature=+simd128 $LINK" cargo test --release -p decbench -p mpeg1 \
      --target wasm32-unknown-emscripten --target-dir "$WORK/simd" -- --test-threads=1
    ;;
  bench)
    REPS=${2:-10}
    ONLY=${3:-}
    in=$WORK/in
    mkdir -p "$in"
    [ -f "$in/pic.png" ] || ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1280x720 -frames:v 1 "$in/pic.png"
    [ -f "$in/pic.jpg" ] || ffmpeg -loglevel error -y -i "$in/pic.png" -q:v 3 "$in/pic.jpg"
    [ -f "$in/movie.mpg" ] || ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=640x480:rate=30 -t 5 -c:v mpeg1video -b:v 2M -f mpeg "$in/movie.mpg"
    for v in scalar simd; do
      f=""
      [ $v = simd ] && f="-C target-feature=+simd128"
      RUSTFLAGS="$f $LINK" cargo build -q --release -p decbench --target wasm32-unknown-emscripten --target-dir "$WORK/$v"
    done
    for mode in desktop android; do
      flags=""
      [ $mode = android ] && flags="--wasm-enforce-bounds-checks"
      for v in scalar simd; do
        node $flags "$WORK/$v/wasm32-unknown-emscripten/release/decbench.js" "$in" "$REPS" "$ONLY" | sed -n "s/^BENCH \([^ ]*\) \([^ ]*\).*/$mode $v \1 \2/p"
      done
    done | sort -k3,3 -k1,1 -k2,2 | awk '{ printf "%-8s %-7s %-26s %8.3f ms\n", $1, $2, $3, $4 }'
    ;;
  *)
    echo "usage: $0 test | bench [REPS] [ONLY]" >&2
    exit 2
    ;;
esac
