#!/bin/bash
# Builds the benchmark programs (tests/bench/src, and host/tests/bench.c) with mingw-w64 into
# OUT (default target/bench-exe): the tree run.mjs mounts as "bench" (`ippai pack OUT bench`).
# Each source's "Build:" line has its libraries; scenario.dat is scenario.exe's archive, kirikiri/
# the KiriKiri game (testdata/kirikiri, pack-kirikiri.mjs), renpy/ the Ren'Py game (pack-renpy.py),
# onscripter/ and onscripter64/ the ONScripter game (pack-onscripter.py), novel16/ the Win16
# game (pack-novel16.py).
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
# (the scenario's game data)
node "$ROOT/tests/bench/gen-scenario.mjs" "$OUT" >/dev/null
# (the KiriKiri game: testdata/kirikiri packed, with 吉里吉里Z fetched into KRKRZ_CACHE once)
KRKRZ_CACHE=${KRKRZ_CACHE:-$ROOT/target/krkrz}
"$ROOT/tests/bench/fetch-krkrz.sh" "$KRKRZ_CACHE"
node "$ROOT/tests/bench/pack-kirikiri.mjs" "$OUT" "$KRKRZ_CACHE/krkrz" >/dev/null
# (the same KiriKiri game on 吉里吉里2, fetched into KRKR2_CACHE once)
KRKR2_CACHE=${KRKR2_CACHE:-$ROOT/target/krkr2}
"$ROOT/tests/bench/fetch-krkr2.sh" "$KRKR2_CACHE"
node "$ROOT/tests/bench/pack-kirikiri.mjs" "$OUT" "$KRKR2_CACHE/krkr2/kirikiri2" >/dev/null
# (the 64-bit C runtimes the x64 games ship app-local, fetched into CRT64_CACHE once)
CRT64_CACHE=${CRT64_CACHE:-$ROOT/target}
"$ROOT/tests/bench/fetch-crt64.sh" "$CRT64_CACHE"
# (the Ren'Py game: testdata/renpy compiled and packed, with Ren'Py's SDK fetched into RENPY_CACHE once)
RENPY_CACHE=${RENPY_CACHE:-$ROOT/target/renpy}
"$ROOT/tests/bench/fetch-renpy.sh" "$RENPY_CACHE"
python3 -I "$ROOT/tests/bench/pack-renpy.py" "$OUT" "$RENPY_CACHE/renpy-sdk" "$CRT64_CACHE/crt64" >/dev/null
# (the ONScripter game: the engine built from pinned sources into ONS_CACHE once)
ONS_CACHE=${ONS_CACHE:-$ROOT/target/onscripter}
"$ROOT/tests/bench/build-onscripter.sh" "$ONS_CACHE"
python3 -I "$ROOT/tests/bench/pack-onscripter.py" "$OUT" "$ONS_CACHE" "$CRT64_CACHE/crt64" >/dev/null
# (the Win16 game: testdata/novel16 built with Open Watcom, fetched into WATCOM_CACHE once)
WATCOM_CACHE=${WATCOM_CACHE:-$ROOT/target/watcom}
"$ROOT/tests/bench/build-novel16.sh" "$WATCOM_CACHE"
python3 -I "$ROOT/tests/bench/pack-novel16.py" "$OUT" "$WATCOM_CACHE" >/dev/null
ls "$OUT"
