#!/bin/sh
# Downloads the SingleStepTests suites (single-instruction tests recorded on real chips, MOO
# format) to target/sst/<cpu>/ for crates/koji/tests/singlestep.rs and tools/jit16test (sst).
# Pinned commits; sparse fetches of the binary directories only; nothing of them is committed
# here. `tests/sst/fetch.sh 80386 8088` fetches only those.
#   80386  v1_ex_real_mode   386EX real mode, 32-bit prefixes        ~430 MB
#   80286  v1_real_mode      286 real mode
#   8086   v1_binary         8086                                    ~95 MB
#   8088   v2_binary         8088 (NMOS), v2
#   V20    v1_native_binary  NEC V20 native mode
set -eu
cd "$(dirname "$0")/../.."
ROOT=target/sst
want() { [ $# -eq 0 ] || echo " $* " | grep -q " $CPU "; }
fetch() { # cpu rev dir [extra files...]
  CPU=$1 REV=$2 DIR=$3
  shift 3
  OUT=$ROOT/$CPU
  if [ "$(cat "$OUT/REV" 2>/dev/null || true)" = "$REV" ] && [ -d "$OUT/$DIR" ]; then
    return 0
  fi
  rm -rf "$OUT"
  git init -q "$OUT"
  git -C "$OUT" -c gc.auto=0 fetch -q --depth 1 --filter=blob:none "https://github.com/SingleStepTests/$CPU" "$REV"
  git -C "$OUT" -c gc.auto=0 checkout -q FETCH_HEAD -- "$DIR" "$@"
  # (the files only: git's copy of them would double the CI cache)
  rm -rf "$OUT/.git"
  echo "$REV" > "$OUT/REV"
  echo "SingleStepTests/$CPU $REV: $(ls "$OUT/$DIR" | wc -l) files in $OUT/$DIR"
}
# (an older layout: the 386 suite directly in target/sst)
[ -d "$ROOT/v1_ex_real_mode" ] && rm -rf "$ROOT"
# (the five suites are separate repositories: fetched at once, so a cache miss is bounded by the
# slowest suite instead of their sum; the downloads are most of a cold run)
pids=
for c in 80386 80286 8086 8088 V20; do
  CPU=$c
  want "$@" || continue
  (
    case $c in
      80386) fetch 80386 459d49fbe6280e9ed46fee887b58dacd9cb880ab v1_ex_real_mode revocation_list.txt ;;
      80286) fetch 80286 37c73caf53dcd22d3dd369ff09305d13d117a4fe v1_real_mode revocation_list.txt ;;
      8086) fetch 8086 e71c68d215a6bb8c356bd4cb3842de3bef345ca9 v1_binary ;;
      8088) fetch 8088 aea84484abc79d09639d855b7b0ab32bc9e4dbeb v2_binary ;;
      V20) fetch V20 6b3748f97533d606249382cad8d94b1c165ef93d v1_native_binary ;;
    esac
  ) &
  pids="$pids $!"
done
st=0
for p in $pids; do
  wait "$p" || st=1
done
exit $st
