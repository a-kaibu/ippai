#!/bin/bash
# Model checks of formal/tla with TLC (tlc-static: one static binary, no JVM). Each line of
# formal/tla/models.txt: <module> <config> <expect>; expect "ok" (no violation) or "violation"
# (a config that shows a known trade-off: TLC must find its counterexample).
#
#   formal/run.sh [module ...]       TLC=path/to/tlc (default: tlc on PATH); WORKERS (default auto)
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TLC=${TLC:-tlc}
WORKERS=${WORKERS:-auto}
fail=0
while read -r mod cfg expect; do
  [ -z "$mod" ] || [ "${mod#\#}" != "$mod" ] && continue
  if [ $# -gt 0 ] && ! printf '%s\n' "$@" | grep -qx "$mod"; then continue; fi
  w=$(mktemp -d)
  cp "$HERE"/tla/*.tla "$HERE/tla/$cfg" "$w"/
  t0=$(date +%s)
  out=$(cd "$w" && "$TLC" -workers "$WORKERS" -deadlock -cleanup -config "$cfg" "$mod.tla" 2>&1)
  rc=$?
  t=$(( $(date +%s) - t0 ))
  states=$(echo "$out" | grep -o '[0-9,]* distinct states found' | tail -1)
  if [ "$expect" = ok ] && [ $rc -eq 0 ]; then
    echo "ok   $mod ($cfg): $states, ${t}s"
  elif [ "$expect" = violation ] && [ $rc -ne 0 ] && echo "$out" | grep -Eq '(is|was) violated'; then
    echo "ok   $mod ($cfg): counterexample found as expected ($(echo "$out" | grep -Em1 '(is|was) violated' | sed 's/^Error: //')), ${t}s"
  else
    echo "FAIL $mod ($cfg), expected $expect, TLC exit $rc:"
    echo "$out" | grep -v '^Parsing\|^Semantic\|^Starting\|^Implied\|^Computing\|^Progress' | tail -60
    fail=1
  fi
  rm -rf "$w"
done < "$HERE/tla/models.txt"
exit $fail
