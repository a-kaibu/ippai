#!/bin/bash
# Console checks of host/tests on sake (its node build): each is built with mingw-w64, then run
# with the JIT and with the interpreter (IPPAI_JIT=0). Both outputs must equal
# host/tests/expected/<name>.txt (checksums of integer, MMX/SSE, atomics, self-modifying code
# results; they match Windows). UPDATE=1 rewrites the expected files. Exits 1 on a failure.
# Tests in X64 are also built for x86-64 (mingw-w64: msvcrt) and must print the same: sake's
# C runtime has to behave alike in both. CLANG64 are x86-64 programs clang builds (SEH, and
# x64crt: an MSVC-ABI program on ucrtbase/vcruntime140), each with its own expected file.
#
#   tests/host-tests.sh [name ...]      SAKE=path/to/sake.js (default /home/user/work/sake-out/sake.js)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SAKE=${SAKE:-/home/user/work/sake-out/sake.js}
OUT=${OUT:-$(mktemp -d)}
TESTS=${*:-nullcall intops simd atomics smc cmp64 ssecvt dinputtest textbounds crtstdio fileseek crtfiles hlsltest d3dx9shader d3dx9effect struct64 guard sse4 xaudio2 winrt wtsapi fastpaths}
X64=" struct64 guard xaudio2 crtstdio fileseek crtfiles cmp64 winrt wtsapi atomics fastpaths "
CLANG64=${CLANG64-x64seh x64crt}
# C++ (g++, static libstdc++) built for x86 and x64, one expected output for both
GXX=${GXX-gxxunwind}
# Tests in VEX are also built with -mavx (the VEX forms of their SSE instructions)
VEX=" sse4 "
fail=0
for t in $TESTS; do
  # a "libs:" line in the source names its import libraries
  libs=$(sed -n 's/.*libs: *\([-a-z0-9_ ]*\).*/\1/p' "$ROOT/host/tests/$t.c" | head -1)
  if ! i686-w64-mingw32-gcc -O2 -mmmx -msse2 -mssse3 "$ROOT/host/tests/$t.c" -o "$OUT/$t.exe" $libs; then
    echo "FAIL $t: build"; fail=1; continue
  fi
  run() { (cd "$(dirname "$SAKE")" && timeout 300 env "$@" node "$SAKE" "$OUT/$t.exe" 2>&1 | grep -v '^sake:'); }
  if ! jit=$(run IPPAI_JIT=1); then
    echo "FAIL $t: JIT process failed or timed out"
    fail=1
    continue
  fi
  if ! interp=$(run IPPAI_JIT=0); then
    echo "FAIL $t: interpreter process failed or timed out"
    fail=1
    continue
  fi
  want=$ROOT/host/tests/expected/$t.txt
  if [ -n "${UPDATE:-}" ]; then
    echo "$jit" > "$want"
  fi
  if [ "$jit" != "$(cat "$want" 2>/dev/null)" ]; then
    echo "FAIL $t (JIT):"; diff <(echo "$jit") "$want" | head -10; fail=1
  elif [ "$interp" != "$jit" ]; then
    echo "FAIL $t (interpreter):"; diff <(echo "$interp") "$want" | head -10; fail=1
  else
    echo "ok   $t"
  fi
  if [[ "$VEX" == *" $t "* ]]; then
    if ! i686-w64-mingw32-gcc -O2 -mavx "$ROOT/host/tests/$t.c" -o "$OUT/$t-vex.exe" $libs; then
      echo "FAIL $t (VEX): build"; fail=1; continue
    fi
    tv=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t-vex.exe" 2>&1 | grep -v '^sake:') )
    if [ "$tv" != "$(cat "$want")" ]; then
      echo "FAIL $t (VEX):"; diff <(echo "$tv") "$want" | head -10; fail=1
    else
      echo "ok   $t (VEX)"
    fi
  fi
  if [[ "$X64" == *" $t "* ]]; then
    if ! x86_64-w64-mingw32-gcc -O2 "$ROOT/host/tests/$t.c" -o "$OUT/$t-64.exe" $libs; then
      echo "FAIL $t (x64): build"; fail=1; continue
    fi
    t64=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t-64.exe" 2>&1 | grep -v '^sake:') )
    if [ "$t64" != "$(cat "$want")" ]; then
      echo "FAIL $t (x64):"; diff <(echo "$t64") "$want" | head -10; fail=1
    else
      echo "ok   $t (x64)"
    fi
  fi
done
for t in $CLANG64; do
  case $t in
  x64crt) "$ROOT/host/tests/build-x64crt.sh" "$OUT" ;;
  *) clang --target=x86_64-w64-windows-gnu -fms-extensions -fuse-ld=lld -O2 "$ROOT/host/tests/$t.c" -o "$OUT/$t.exe" ;;
  esac || { echo "FAIL $t: build"; fail=1; continue; }
  got=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t.exe" 2>&1 | grep -v '^sake:') )
  want=$ROOT/host/tests/expected/$t.txt
  [ -n "${UPDATE:-}" ] && echo "$got" > "$want"
  if [ "$got" != "$(cat "$want" 2>/dev/null)" ]; then
    echo "FAIL $t:"; diff <(echo "$got") "$want" | head -10; fail=1
  else
    echo "ok   $t"
  fi
done
for t in $GXX; do
  want=$ROOT/host/tests/expected/$t.txt
  for arch in i686 x86_64; do
    if ! $arch-w64-mingw32-g++ -O2 -static "$ROOT/host/tests/$t.cpp" -o "$OUT/$t-$arch.exe"; then
      echo "FAIL $t ($arch): build"; fail=1; continue
    fi
    got=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t-$arch.exe" 2>&1 | grep -v '^sake:') )
    [ -n "${UPDATE:-}" ] && [ $arch = i686 ] && echo "$got" > "$want"
    if [ "$got" != "$(cat "$want" 2>/dev/null)" ]; then
      echo "FAIL $t ($arch):"; diff <(echo "$got") "$want" | head -10; fail=1
    else
      echo "ok   $t ($arch)"
    fi
  done
done
exit $fail
