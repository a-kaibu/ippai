#!/bin/bash
# One shard of the trial-game corpus on sake, as .github/workflows/corpus.yml runs it (and
# runnable anywhere with the same tools): the titles of tests/corpus/titles.tsv whose row
# number modulo SHARDS is SHARD are downloaded from their original sites, run in headless
# Chromium (tests/corpus-run.py, tests/corpus.mjs), and titles that fell below their expected
# level get one more try (a busy runner can miss a timing). Grading: tests/corpus/check.py.
#
#   tests/corpus/ci.sh DIST FONTS OUT
#     DIST: an unpacked release (ippai, web/: tools/ci/build.sh)
#     FONTS: sake's C:\windows\Fonts (tools/ci/sake-fonts.py). That is all of C:\: no Wine
#     DLLs, so a title that needs a DLL sake does not implement yet fails here, and
#     check.py lists the DLLs each title missed.
#   env: SET=smoke|full (default smoke: rows with smoke=1), SHARD=0 SHARDS=1, ONLY="id ..."
#        (just those), WORK (default $RUNNER_TEMP or /tmp), STORY (story seconds, default 30),
#        LANES (titles run at once, default 2), PREP_JOBS (downloads ahead per lane, default 1: most trials come from one
#        home-run mirror, suezou.dyndns.org, which 60 parallel downloads knocked over)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DIST=$(realpath "$1") FONTS=$(realpath "$2") OUT=$3
WORK=${WORK:-${RUNNER_TEMP:-/tmp}/corpus}
TITLES=$ROOT/tests/corpus/titles.tsv
mkdir -p "$WORK" "$OUT"
OUT=$(realpath "$OUT")

# sake's C:\ as a store with the tree "prefix32" (what play.html mounts); each title's store is a
# hard-linked copy of it plus the game's tree
BASE=$WORK/base-store
if [ ! -d "$BASE" ]; then
  set -e
  rm -rf "$WORK/prefix" "$BASE.part" && mkdir -p "$WORK/prefix/drive_c/windows/temp"
  cp -r "$FONTS" "$WORK/prefix/drive_c/windows/Fonts"
  "$DIST/ippai" --store "$BASE.part" pack "$WORK/prefix" prefix32 >/dev/null
  rm -rf "$WORK/prefix"
  mv "$BASE.part" "$BASE"
  set +e
fi

IDS=$WORK/ids.txt
python3 -I - "$TITLES" "${SET:-smoke}" "${SHARD:-0}" "${SHARDS:-1}" "${ONLY:-}" > "$IDS" <<'EOF'
import csv, sys
path, kind, k, n, only = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5].split()
rows = [r for r in csv.DictReader(open(path, encoding='utf-8'), delimiter='\t')
        if ((r['id'] in only) if only else (kind == 'full' or r.get('smoke') == '1'))]
for i, r in enumerate(rows):
    if i % n == k: print(r['id'])
EOF
echo "shard ${SHARD:-0}/${SHARDS:-1}: $(wc -l < "$IDS") titles"

run1() {  # ids file, port
  IPPAI_DIST=$DIST BASE_STORE=$BASE TRIALS=$WORK/trials STORES=$WORK/stores JIT_DIR=$WORK/jit \
    DL_TRIES=${DL_TRIES:-3} DL_MAX_TIME=${DL_MAX_TIME:-3600} PREP_TIMEOUT=${PREP_TIMEOUT:-11000} SKIP_X64=0 CORPUS_QS='&env=SAKE_TRACE_LOAD%3D1&env=IPPAI_API_COUNTS%3D1' STORY=${STORY:-30} PREP_JOBS=${PREP_JOBS:-1} PORT=$2 \
    python3 "$ROOT/tests/corpus-run.py" "$TITLES" "$1" "$OUT"
}
# LANES (default 2) queues side by side, each with its own Chromium, server and downloads: the
# trial hosts are slow per connection and a 4-core runner has room for two games
run() {
  local n=${LANES:-2} i
  for i in $(seq 0 $((n - 1))); do awk -v n="$n" -v i="$i" 'NR % n == i' "$1" > "$1.$i"; done
  local -a pids=()
  for i in $(seq 0 $((n - 1))); do
    run1 "$1.$i" $((8092 + i)) &
    pids+=("$!")
  done
  local failed=0 pid
  for pid in "${pids[@]}"; do
    wait "$pid" || failed=1
  done
  return "$failed"
}
fail=0
run "$IDS" || fail=1
# one more try for the titles below their expected level
python3 "$ROOT/tests/corpus/check.py" "$TITLES" "$OUT" --regressions "$WORK/again.txt" >/dev/null
if [ -s "$WORK/again.txt" ]; then
  echo "again: $(tr '\n' ' ' < "$WORK/again.txt")"
  while read -r id; do rm -rf "${OUT:?}/$id"; done < "$WORK/again.txt"
  run "$WORK/again.txt" || fail=1
fi
# Missing results must not be silently ignored by check.py.
while IFS= read -r id; do
  if [ ! -f "$OUT/$id/result.json" ]; then
    echo "FAIL $id: missing result.json" >&2
    fail=1
  fi
done < "$IDS"
python3 "$ROOT/tests/corpus/check.py" "$TITLES" "$OUT" >/dev/null || fail=1
exit "$fail"
