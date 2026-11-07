#!/bin/bash
# Builds host/tests' msvcp140 checks into $1 (default target/winref/exe) as host_<name>-64.exe:
# MSVC-ABI x86-64 C++ programs (clang + lld-link, build-msvc64.sh; msvcstream against
# Microsoft's STL headers). Apart from build.sh so the probes' answers do not depend on it.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/winref/exe}
mkdir -p "$OUT"
for t in msvcpsync msvcstream; do
  "$ROOT/host/tests/build-msvc64.sh" "$t" "$OUT"
  mv "$OUT/$t.exe" "$OUT/host_$t-64.exe"
done
