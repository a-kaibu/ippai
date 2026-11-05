#!/bin/bash
# Builds the benchmark programs (tests/bench/src, and host/tests/bench.c) with mingw-w64 into
# OUT (default target/bench-exe): the tree run.mjs mounts as "bench" (`ippai pack OUT bench`).
# Each source's "Build:" line has its libraries.
#
#   tests/bench/build.sh [OUT]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=$(mkdir -p "${1:-$ROOT/target/bench-exe}" && cd "${1:-$ROOT/target/bench-exe}" && pwd)
cd "$ROOT/tests/bench/src"
for src in *.c *.cpp; do
  cmd=$(sed -n 's|^ \* Build: \(.*\) \*/$|\1|p; s|^ \* Build: \(.*\)$|\1|p' "$src" | head -1)
  [ -n "$cmd" ] || { echo "no Build: line in $src"; exit 1; }
  # (warnings are errors here: a benchmark that computes something else measures nothing)
  eval "${cmd/ -o / -Wall -Werror -o $OUT/}"
done
i686-w64-mingw32-gcc -O2 "$ROOT/host/tests/bench.c" -o "$OUT/bench.exe"
ls "$OUT"
