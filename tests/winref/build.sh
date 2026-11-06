#!/bin/bash
# Builds the winref probes (probes/*.c) for x86 and x86-64, and host/tests' console checks for
# x86, into $1 (default target/winref/exe). A "libs:" line in a source names its import
# libraries; "arch: 32" builds it for x86 only; "manifest: ja" adds a code page 932 build.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/winref/exe}
mkdir -p "$OUT"
libs() { sed -n 's/.*libs: *\([-a-z0-9_ ]*\).*/\1/p' "$1" | head -1; }
for src in "$HERE"/probes/*.c; do
  n=$(basename "$src" .c)
  l=$(libs "$src")
  i686-w64-mingw32-gcc -O2 -msse2 -Wall -Wno-format -Wno-unused -o "$OUT/$n-32.exe" "$src" $l
  # "manifest: ja": also a "-ja" build whose manifest asks for code page 932 (activeCodePage)
  if grep -q 'manifest: ja' "$src"; then
    (cd "$HERE" && i686-w64-mingw32-windres ja.rc -O coff -o "$OUT/ja.res")
    i686-w64-mingw32-gcc -O2 -msse2 -Wall -Wno-format -Wno-unused -o "$OUT/${n}_ja-32.exe" "$src" "$OUT/ja.res" $l
    rm "$OUT/ja.res"
  fi
  if ! grep -q 'arch: 32' "$src"; then
    x86_64-w64-mingw32-gcc -O2 -Wall -Wno-format -Wno-unused -o "$OUT/$n-64.exe" "$src" $l
  fi
done
# host/tests checks that print results (tests/host-tests.sh compares them with expected/*.txt;
# here they are compared with Windows' own output)
for t in nullcall intops simd atomics smc cmp64 ssecvt textbounds crtstdio fileseek crtfiles struct64; do
  src=$ROOT/host/tests/$t.c
  i686-w64-mingw32-gcc -O2 -mmmx -msse2 -mssse3 -o "$OUT/host_$t-32.exe" "$src" $(libs "$src")
done
x86_64-w64-mingw32-gcc -O2 -o "$OUT/host_struct64-64.exe" "$ROOT/host/tests/struct64.c" $(libs "$ROOT/host/tests/struct64.c")
ls "$OUT" | wc -l
