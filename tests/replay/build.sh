#!/bin/bash
# Builds into $1 (default target/replay):
#   replay-32.exe, replay-64.exe   play a stream file given on their command line (games' traces)
#   replay_<trace>.exe             one per traces/<trace>.trace (<probe>-32 / -64), the stream
#                                  inside, of the trace's width: winref probes named
#                                  replay_<probe>-<bits>-<bits> (tests/winref/build.sh calls this
#                                  with its exe dir; they draw, so they are listed in browser.txt)
#                                  and one per trial game's trace in testdata/apitrace (games.py; recorded by
#                                  .github/workflows/gametrace.yml), the same, showing one frame
#                                  in GAME_EVERY (default 900: 15 s at 60 fps), its stream at most
#                                  GAME_MAX bytes (default 256 MiB: the calls up to there)
# The streams come from the apitrace crate (`cargo run -p apitrace`, or APITRACE=its binary).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/replay}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
(cd "$ROOT/tests/winref" && i686-w64-mingw32-windres ja.rc -O coff -o "$OUT/replay32.res" && x86_64-w64-mingw32-windres ja.rc -O coff -o "$OUT/replay64.res")
SRC=("$HERE/replay.c" "$HERE/d3d9img.c" "$HERE/d3d8img.c" "$HERE/d3d11img.c")
cc() {
  local bits=$1; shift
  if [ "$bits" = 64 ]; then
    x86_64-w64-mingw32-gcc -O2 -Wall -Wno-format -Wno-unused "$@" "${SRC[@]}" "$OUT/replay64.res" -ldxguid -luuid -lgdi32
  else
    i686-w64-mingw32-gcc -O2 -msse2 -Wall -Wno-format -Wno-unused "$@" "${SRC[@]}" "$OUT/replay32.res" -ldxguid -luuid -lgdi32
  fi
}
# (not among the probes: they need a stream on their command line)
if [ ! -f "$OUT/browser.txt" ]; then
  cc 32 -o "$OUT/replay-32.exe"
  cc 64 -o "$OUT/replay-64.exe"
fi
shopt -s nullglob
traces=("$HERE"/traces/*.trace)
games=($(python3 "$HERE/games.py" names))
# (NO_TRACES=1: the replayers alone)
[ -n "${NO_TRACES:-}" ] && traces=() games=()
if [ $(( ${#traces[@]} + ${#games[@]} )) -gt 0 ]; then
  if [ -z "${APITRACE:-}" ]; then
    cargo build -q --release -p apitrace --manifest-path "$ROOT/Cargo.toml"
    APITRACE=$ROOT/target/release/apitrace
  fi
  for t in "${traces[@]}"; do
    n=$(basename "$t" .trace)
    bits=${n##*-}
    st=0; "$APITRACE" -strict "-$bits" "$t" "$OUT/$n.rpl" || st=$?
    if [ $st = 3 ]; then echo "$n: not replayed yet"; continue; fi
    [ $st = 0 ] || exit $st
    cc "$bits" -DSTREAM="\"$OUT/$n.rpl\"" -o "$OUT/replay_$n-$bits.exe"
    rm "$OUT/$n.rpl"
    [ -f "$OUT/browser.txt" ] && echo "replay_$n-$bits" >> "$OUT/browser.txt"
  done
  # (a game's trace ends where the game was killed: not -strict. Its stream goes into the exe:
  # at most GAME_MAX bytes (default 256 MiB), the calls up to there; a stream of gigabytes made
  # i686-w64-mingw32-ld crash, and a 32-bit process could not map such an image)
  for n in "${games[@]}"; do
    bits=${n##*-}
    python3 "$HERE/games.py" unpack "$ROOT/testdata/apitrace/$n" "$OUT/$n.trace"
    st=0; "$APITRACE" "-$bits" "-max=${GAME_MAX:-268435456}" "$OUT/$n.trace" "$OUT/$n.rpl" || st=$?
    rm "$OUT/$n.trace"
    if [ $st = 3 ]; then echo "$n: not replayed yet"; continue; fi
    [ $st = 0 ] || exit $st
    cc "$bits" -DSTREAM="\"$OUT/$n.rpl\"" -DEVERY="${GAME_EVERY:-900}" -o "$OUT/replay_$n-$bits.exe"
    rm "$OUT/$n.rpl"
    [ -f "$OUT/browser.txt" ] && echo "replay_$n-$bits" >> "$OUT/browser.txt"
  done
fi
rm "$OUT/replay32.res" "$OUT/replay64.res"
