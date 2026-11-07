#!/bin/bash
# Save states under node (docs/savestate.md): host/tests/snaptest.c (and other host tests) run
# whole, and in pieces cut by states. For each cut time T:
#   run A: a state at T, the process ends there (IPPAI_SNAP_EXIT)
#   run B: from that state to the end
# A's output followed by B's must be the whole run's. Also: a live state (the program goes on)
# changes nothing, B twice prints the same, a state taken from a restored run (A -> B' -> C)
# restores too. With the JIT and the interpreter (IPPAI_JIT=0), x86 and x86-64.
#
#   tests/snap.sh          SAKE=path/to/sake.js (default /home/user/work/sake-out/sake.js); node 24+
#                          (EXES="snaptest snaptest-64", JITS="1 0", TIMES: fewer)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SAKE=${SAKE:-/home/user/work/sake-out/sake.js}
OUT=${OUT:-$(mktemp -d)}
TIMES=${TIMES:-0.2 0.7 1.3 2.1 2.8}
EXES=${EXES:-snaptest snaptest-64}
JITS=${JITS:-1 0}
fail=0
keep=
i686-w64-mingw32-gcc -O2 "$ROOT/host/tests/snaptest.c" -o "$OUT/snaptest.exe" || exit 1
x86_64-w64-mingw32-gcc -O2 "$ROOT/host/tests/snaptest.c" -o "$OUT/snaptest-64.exe" || exit 1
# the program's output (sake's and the states' own lines left out: they may come in the middle
# of one of its lines, both go to stderr; a last line without its newline stays so)
LOG=$OUT/last.log
run() {
  (cd "$(dirname "$SAKE")" && timeout 120 env "$@" node "$SAKE" "$EXE" 2>&1 | tee "$LOG" |
    perl -0pe 's/snap: (stopped in \d+ ms, \d+ ranges|state of [^\n]*? stood still \d+ ms|saved \S+|restoring \d+ threads|no state: [^\n]*|stirred: [^\n]*)\n//g; s/^(sake|snap):[^\n]*\n//mg')
  # (a thread that ran while a state was copied: snap.js drops that state, the test fails)
  if grep -q "snap: stirred" "$LOG"; then echo "FAIL $*: a thread ran while the state was copied" >&2; fail=1; fi
}
check() {  # <name> <file got> <file want>
  if ! cmp -s "$2" "$3"; then
    echo "FAIL $1"; diff "$3" "$2" | head -8; fail=1
    # (what sake said, and the files kept)
    cp "$LOG" "$2.log" 2>/dev/null; echo "  kept: $2 $2.log $s"; keep=1
  else
    echo "ok   $1"
  fi
}
for exe in $EXES; do
  EXE=$OUT/$exe.exe
  for jit in $JITS; do
    tag="$exe jit=$jit"
    o=$OUT/$exe-$jit
    run IPPAI_JIT=$jit > $o.whole
    if [ "$(tail -1 $o.whole)" != end ] || ! grep -qx 'clocks: ok' $o.whole; then
      echo "FAIL $tag: the whole run"; tail -5 $o.whole; fail=1; continue
    fi
    for t in $TIMES; do
      s=$o-$t.snap
      # (a state may come in the middle of a line: the pieces are put together as they are)
      run IPPAI_JIT=$jit IPPAI_SNAP_SAVE="$s@$t" IPPAI_SNAP_EXIT=1 > $o.a
      if [ ! -s "$s" ]; then
        echo "FAIL $tag @$t: no state"; fail=1; continue
      fi
      run IPPAI_JIT=$jit IPPAI_SNAP_LOAD="$s" > $o.b
      cat $o.a $o.b > $o.ab
      check "$tag @$t: before + after = whole" $o.ab $o.whole
      run IPPAI_JIT=$jit IPPAI_SNAP_LOAD="$s" > $o.b2
      check "$tag @$t: restored twice" $o.b2 $o.b
    done
    # a state from a restored run
    run IPPAI_JIT=$jit IPPAI_SNAP_SAVE="$o.c1@0.9" IPPAI_SNAP_EXIT=1 > $o.a
    run IPPAI_JIT=$jit IPPAI_SNAP_LOAD="$o.c1" IPPAI_SNAP_SAVE="$o.c2@0.9" IPPAI_SNAP_EXIT=1 > $o.b
    run IPPAI_JIT=$jit IPPAI_SNAP_LOAD="$o.c2" > $o.c
    cat $o.a $o.b $o.c > $o.abc
    check "$tag: state of a restored run" $o.abc $o.whole
    # live states: the program goes on as if none were taken
    run IPPAI_JIT=$jit IPPAI_SNAP_SAVE="$o.live@0.5,1.5,2.5" > $o.l
    check "$tag: live states" $o.l $o.whole
  done
done
# a state asked for while the program starts (its TLS callback sleeps, called by sake before
# the entry): taken once the program runs on its own, it restores
for bits in 32 64; do
  cc=i686-w64-mingw32-gcc; [ $bits = 64 ] && cc=x86_64-w64-mingw32-gcc
  $cc -O2 "$ROOT/host/tests/snapinit.c" -o "$OUT/snapinit-$bits.exe" || exit 1
  EXE=$OUT/snapinit-$bits.exe
  o=$OUT/snapinit-$bits
  run > $o.whole
  run IPPAI_SNAP_SAVE="$o.snap@1.2" IPPAI_SNAP_EXIT=1 > $o.a
  run IPPAI_SNAP_LOAD="$o.snap" > $o.b
  cat $o.a $o.b > $o.ab
  if [ "$(tail -1 $o.whole)" = end ] && grep -qx callback $o.a; then
    check "snapinit $bits: asked for in the TLS callback" $o.ab $o.whole
  else
    echo "FAIL snapinit $bits: the whole run, or the state not asked for in the callback"; tail -3 $o.whole $o.a; fail=1
  fi
done
# a program with another process: no state (the kernel they share is outside the memory), the
# program goes on
i686-w64-mingw32-gcc -O2 "$ROOT/host/tests/snapproc.c" -o "$OUT/snapproc.exe" &&
  i686-w64-mingw32-gcc -O2 -DCHILD "$ROOT/host/tests/snapproc.c" -o "$OUT/snapchild.exe" || exit 1
EXE=$OUT/snapproc.exe
run IPPAI_SNAP_SAVE="$OUT/proc.snap@1" > "$OUT/proc.out"
if grep -q "snap: no state: other processes run with this one" "$LOG" && [ ! -e "$OUT/proc.snap" ] &&
  [ "$(cat "$OUT/proc.out")" = "$(printf 'started\nchild: 7\nend')" ]; then
  echo "ok   another process: no state, the program goes on"
else
  echo "FAIL another process: a state, or the program did not go on"; cat "$OUT/proc.out"; grep snap: "$LOG"; fail=1
fi
[ -n "$keep" ] || rm -f "$OUT"/snaptest*-[01]*
exit $fail
