#!/bin/bash
# Trace checks: the trace tests drive kura's real Direct3D 9/11 object tables with random calls
# and write each run as formal/tla ComTrace.tla; TLC checks every one against ComRefs.tla
# (ComRefsTrace: each call's result, the object it handed back, and which objects are alive).
#
#   formal/trace.sh       TLC=path/to/tlc; IPPAI_TRACE_SEEDS (default 20 per API)
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TLC=${TLC:-tlc}
dir=$(mktemp -d)
IPPAI_TRACE_DIR=$dir cargo test -q --manifest-path "$HERE/../Cargo.toml" -p kura --lib object_lifetimes_trace || [ -n "${KEEP_GOING:-}" ] || exit 1
fail=0; n=0
for t in "$dir"/*/; do
  cp "$HERE"/tla/ComRefs.tla "$HERE"/tla/ComRefsTrace.tla "$HERE"/tla/ComRefsTrace.cfg "$t"
  out=$(cd "$t" && "$TLC" -workers 1 -cleanup -config ComRefsTrace.cfg ComRefsTrace.tla 2>&1)
  if [ $? -ne 0 ]; then
    i=$(echo "$out" | grep -o '^/\\ i = [0-9]*' | tail -1 | grep -o '[0-9]*$')
    echo "FAIL $(basename "$t"), call $i:"
    if [ -n "$i" ]; then
      sed -n "$((i + 3))p" "$t/ComTrace.tla"
      echo "ComRefs says:"; echo "$out" | awk '/^State /{s=""} {s=s"\n"$0} END{print s}' | grep -E '^ *(ret|got|alive) \|->'
    else
      echo "$out" | grep -v '^Parsing\|^Semantic\|^Starting\|^Implied\|^Computing\|^Progress\|^Linting' | tail -40
    fi
    fail=1
  fi
  n=$((n + 1))
done
echo "$n traces checked against ComRefs"
rm -rf "$dir"
exit $fail
