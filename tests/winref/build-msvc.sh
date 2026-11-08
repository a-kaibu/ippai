#!/bin/bash
# Builds host/tests' msvcp140 and C++ runtime checks (and x64seh) into $1 (default target/winref/exe) as host_<name>-64.exe:
# MSVC-ABI x86-64 C++ programs (clang + lld-link, build-msvc64.sh; msvcstream against
# Microsoft's STL headers). Apart from build.sh so the probes' answers do not depend on it.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/winref/exe}
mkdir -p "$OUT"
for t in msvcpsync msvcstream msvcrtti msvcseh; do
  "$ROOT/host/tests/build-msvc64.sh" "$t" "$OUT"
  mv "$OUT/$t.exe" "$OUT/host_$t-64.exe"
done
# (x86 on msvcrt.dll: build-msvc32.sh)
for t in msvcseh; do
  "$ROOT/host/tests/build-msvc32.sh" "$t" "$OUT"
  mv "$OUT/$t.exe" "$OUT/host_$t-32.exe"
done
# x64seh: SEH on x86-64 (unwinding through handlers, longjmp, vectored handlers, and
# STATUS_UNWIND_CONSOLIDATE as C++ catch blocks use it) as clang's mingw target builds it
clang --target=x86_64-w64-windows-gnu -fms-extensions -fuse-ld=lld -O2 "$ROOT/host/tests/x64seh.c" -o "$OUT/host_x64seh-64.exe"
