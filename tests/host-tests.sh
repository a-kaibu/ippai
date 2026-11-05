#!/bin/bash
# Console checks of host/tests on sake (its node build): each is built with mingw-w64, then run
# with the JIT and with the interpreter (VNVM_JIT=0). Both outputs must equal
# host/tests/expected/<name>.txt (checksums of integer, MMX/SSE, atomics, self-modifying code
# results; they match Windows). UPDATE=1 rewrites the expected files. Exits 1 on a failure.
#
#   tests/host-tests.sh [name ...]      SAKE=path/to/sake.js (default /home/user/work/sake-out/sake.js)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SAKE=${SAKE:-/home/user/work/sake-out/sake.js}
OUT=${OUT:-$(mktemp -d)}
TESTS=${*:-nullcall intops simd atomics smc cmp64 ssecvt dinputtest textbounds crtstdio fileseek crtfiles}
fail=0
for t in $TESTS; do
  # a "libs:" line in the source names its import libraries
  libs=$(sed -n 's/.*libs: *\([-a-z0-9 ]*\).*/\1/p' "$ROOT/host/tests/$t.c" | head -1)
  if ! i686-w64-mingw32-gcc -O2 -mmmx -msse2 -mssse3 "$ROOT/host/tests/$t.c" -o "$OUT/$t.exe" $libs; then
    echo "FAIL $t: build"; fail=1; continue
  fi
  run() { (cd "$(dirname "$SAKE")" && timeout 300 env "$@" node "$SAKE" "$OUT/$t.exe" 2>&1 | grep -v '^sake:'); }
  if ! jit=$(run VNVM_JIT=1); then
    echo "FAIL $t: JIT process failed or timed out"
    fail=1
    continue
  fi
  if ! interp=$(run VNVM_JIT=0); then
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
done
exit $fail
