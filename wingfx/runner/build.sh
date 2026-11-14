#!/bin/bash
# Builds the executor once per suite (cases/<suite>.txt compiled in) for x86 and x86-64 with
# mingw-w64: <out>/wingfx_<suite>-32.exe and -64.exe. Suites that need a GPU (their first line
# says "# gpu") are listed in <out>/browser.txt, appended to if it exists (a host that runs GPU
# programs elsewhere, e.g. sake in a browser page, reads it).
#
#   wingfx/runner/build.sh <out dir>
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?out dir}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
for cases in "$HERE"/../cases/*.txt; do
  suite=$(basename "$cases" .txt)
  { echo "static const char wingfx_cases[] = {"; od -An -v -tx1 "$cases" | sed 's/ \([0-9a-f][0-9a-f]\)/0x\1,/g'; echo "0};"; } > "$TMP/cases_$suite.h"
  for arch in 32 64; do
    cc=i686-w64-mingw32-gcc; [ "$arch" = 64 ] && cc=x86_64-w64-mingw32-gcc
    $cc -O2 -Wall -Wno-format -Wno-unused -I"$TMP" -DWINGFX_CASES="\"cases_$suite.h\"" -DWINGFX_SUITE="\"$suite\"" -o "$OUT/wingfx_$suite-$arch.exe" "$HERE/wingfx.c" -lgdi32 -luser32
  done
  if head -1 "$cases" | grep -q '^# gpu'; then
    echo "wingfx_$suite-32" >> "$OUT/browser.txt"
    echo "wingfx_$suite-64" >> "$OUT/browser.txt"
  fi
done
