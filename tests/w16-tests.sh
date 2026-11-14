#!/bin/bash
# Win16 programs (tests/w16/*.c, 16-bit Windows 3.1 programs built with Open Watcom: a .rc
# beside one is its resources, "dll: name" lines build tests/w16/<name>.c as a 16-bit DLL beside
# it) on sake's node build, with the 16-bit JIT and without (MASU_JIT16=0). They print what
# they find; both runs must print tests/w16/expected/<name>.txt. Windows 3.1 itself is not at
# hand (64-bit Windows runs no 16-bit programs): the expected files are what the programs'
# own checks and Windows' documented behaviour say. UPDATE=1 rewrites them.
#
#   tests/w16-tests.sh [name ...]   SAKE=path/to/sake.js, WATCOM_DIR (default ~/.cache/masu-watcom)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SAKE=${SAKE:-/home/user/work/sake-out/sake.js}
WD=${WATCOM_DIR:-$HOME/.cache/masu-watcom}
OUT=${OUT:-$(mktemp -d)}
# (Open Watcom, fetched and checked by the Win16 benchmark's build; its game is built too)
"$ROOT/tests/bench/build-novel16.sh" "$WD" > /dev/null || exit 1
export WATCOM=$WD/watcom PATH=$WD/watcom/binl64:$PATH INCLUDE=$WD/watcom/h:$WD/watcom/h/win
TESTS=${*:-$(cd "$ROOT/tests/w16" && grep -L '^/\* *16-bit DLL' *.c | sed 's/\.c$//')}
fail=0
for t in $TESTS; do
  d=$OUT/$t
  rm -rf "$d" && mkdir -p "$d"
  src=$ROOT/tests/w16/$t.c
  (
    cd "$d" || exit 1
    python3 "$ROOT/tests/w16/gen-bmp.py" "$d"
    for dll in $(sed -n 's/.*dll: *\([a-z0-9_ ]*\).*/\1/p' "$src"); do
      dlibs=$(sed -n 's/.*libs: *\([a-z0-9_. ]*\).*/\1/p' "$ROOT/tests/w16/$dll.c")
      wcl -q -bt=windows -bd -ml -zu -we -l=windows_dll -fe="$d/$dll.dll" "$ROOT/tests/w16/$dll.c" $dlibs || exit 1
      wlib -q -n "$d/$dll.lib" +"$d/$dll.dll" || exit 1
    done
    libs=$(sed -n 's/.*libs: *\([a-z0-9_. ]*\).*/\1/p' "$src")
    wcl -q -bt=windows -ml -we -l=windows -fe="$d/$t.exe" "$src" $libs || exit 1
    if [ -f "$ROOT/tests/w16/$t.rc" ]; then
      wrc -q -r -bt=windows -i="$ROOT/tests/w16" -i="$d" "$ROOT/tests/w16/$t.rc" -fo="$d/$t.res" && wrc -q -bt=windows "$d/$t.res" -fe="$d/$t.exe" || exit 1
    fi
  ) > "$d/build.log" 2>&1 || { echo "FAIL $t (build)"; cat "$d/build.log"; fail=1; continue; }
  for mode in jit interp; do
    env=; [ $mode = interp ] && env=MASU_JIT16=0
    mkdir -p "$d/c-$mode"
    # (sake writes the console to stderr, with its own "sake:" lines)
    (cd "$d" && timeout 120 env $env SAKE_C="$d/c-$mode" node "$SAKE" "$d/$t.exe" > "$d/$mode.raw" 2>&1)
    grep -Ev '^(sake|kura)[: ]' "$d/$mode.raw" | tr -d '\r' > "$d/$mode.txt"
    grep -E '^(sake|kura)[: ]' "$d/$mode.raw" > "$d/$mode.err"
    want=$ROOT/tests/w16/expected/$t.txt
    if [ "${UPDATE:-}" = 1 ] && [ $mode = jit ]; then cp "$d/$mode.txt" "$want"; fi
    if cmp -s "$d/$mode.txt" "$want"; then
      echo "ok   $t ($mode)"
    else
      echo "FAIL $t ($mode)"; diff "$want" "$d/$mode.txt" | head -20; tail -5 "$d/$mode.err"; fail=1
    fi
  done
done
exit $fail
