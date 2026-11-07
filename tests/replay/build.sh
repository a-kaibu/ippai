#!/bin/bash
# Builds into $1 (default target/replay):
#   d3d9replay.exe              plays a stream file given on its command line (games' traces)
#   replay_<trace>-32.exe       one per traces/<trace>.trace, the stream inside: winref probes
#                               (tests/winref/build.sh calls this with its exe dir; they draw, so
#                               they are listed in browser.txt)
# The streams come from the apitrace crate (`cargo run -p apitrace`, or APITRACE=its binary), the
# vtable indices from mingw-w64's d3d9.h (methods.sh).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/replay}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
"$HERE/methods.sh" > "$OUT/d3d9methods.h"
(cd "$ROOT/tests/winref" && i686-w64-mingw32-windres ja.rc -O coff -o "$OUT/replay32.res")
cc() { i686-w64-mingw32-gcc -O2 -msse2 -Wall -Wno-format -Wno-unused -I"$OUT" "$@" "$OUT/replay32.res" -ld3d9 -lgdi32; }
# (not among the probes: it needs a stream on its command line)
[ -f "$OUT/browser.txt" ] || cc -o "$OUT/d3d9replay.exe" "$HERE/d3d9replay.c"
shopt -s nullglob
traces=("$HERE"/traces/*.trace)
if [ ${#traces[@]} -gt 0 ]; then
  if [ -z "${APITRACE:-}" ]; then
    cargo build -q --release -p apitrace --manifest-path "$ROOT/Cargo.toml"
    APITRACE=$ROOT/target/release/apitrace
  fi
  for t in "${traces[@]}"; do
    n=$(basename "$t" .trace)
    st=0; "$APITRACE" "$t" "$OUT/$n.rpl" || st=$?
    if [ $st = 3 ]; then echo "$n: not replayed yet"; continue; fi
    [ $st = 0 ] || exit $st
    cc -DSTREAM="\"$OUT/$n.rpl\"" -o "$OUT/replay_$n-32.exe" "$HERE/d3d9replay.c"
    rm "$OUT/$n.rpl"
    [ -f "$OUT/browser.txt" ] && echo "replay_$n-32" >> "$OUT/browser.txt"
  done
fi
rm "$OUT/replay32.res" "$OUT/d3d9methods.h"
