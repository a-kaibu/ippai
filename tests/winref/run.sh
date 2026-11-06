#!/bin/bash
# Runs every exe in <exe dir> and keeps <out dir>/<name>/stdout.txt and exit code. On Windows
# (Git Bash on the runner) the exes run natively: that output is the reference. With SAKE=path
# to sake.js (its node build) they run on sake; IPPAI_JIT=0 for the interpreter.
#
#   tests/winref/run.sh <exe dir> <out dir> [name-filter]
set -uo pipefail
EXE=$(cd "$1" && pwd)
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
FILTER=${3:-}
for exe in "$EXE"/*.exe; do
  n=$(basename "$exe" .exe)
  [[ -n "$FILTER" && "$n" != *$FILTER* ]] && continue
  d=$OUT/$n
  rm -rf "$d"; mkdir -p "$d/cwd"
  start=$(date +%s)
  if [ -n "${SAKE:-}" ]; then
    # (sake writes the console to stderr, with its own "sake:" lines)
    (cd "$d/cwd" && timeout 300 node "$SAKE" "$exe" > ../stdout.raw 2>&1; echo $? > ../exit)
    grep -v '^sake:' "$d/stdout.raw" > "$d/stdout.txt"; grep '^sake:' "$d/stdout.raw" > "$d/stderr.txt"; rm "$d/stdout.raw"
  else
    (cd "$d/cwd" && timeout 300 "$exe" > ../stdout.txt 2> ../stderr.txt; echo $? > ../exit)
  fi
  echo "$n exit $(cat "$d/exit") $(( $(date +%s) - start ))s $(wc -l < "$d/stdout.txt") lines"
done
