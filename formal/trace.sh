#!/bin/bash
# Trace checks: the trace tests (cargo test ... _trace) drive kura's (and sake's) graphics API
# code with random calls and write each run as a TLA+ module, with the name of the spec it
# follows in SPEC; TLC checks every one against formal/tla/<spec>.tla through <spec>Trace.tla:
#   ComRefs   D3D9/D3D11 object lifetimes: results, objects handed back, which are alive
#   Locks     D3D9 surface locks and copies: results, what locks and page readbacks show
#   Maps      D3D11 maps and copies: results, what maps and page readbacks show
#   GlNames   GL names: glGen/glDelete/glBind/glIs, attachments, binding queries, glGetError
#   StateBlocks D3D9 state blocks: types' masks, recording, Capture/Apply: results, what Get reads
#   D2dDraw   sake's D2D BeginDraw/EndDraw/Flush, drawing calls and clips: what each returns
#
#   formal/trace.sh       TLC=path/to/tlc; IPPAI_TRACE_SEEDS (default 20 per test);
#                         KEEP=dir copies each failing trace (and TLC's output) there
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TLC=${TLC:-tlc}
dir=$(mktemp -d)
IPPAI_TRACE_DIR=$dir cargo test -q --manifest-path "$HERE/../Cargo.toml" -p kura -p sake --lib _trace || [ -n "${KEEP_GOING:-}" ] || exit 1
fail=0; n=0
for t in "$dir"/*/; do
  spec=$(cat "$t/SPEC")
  cp "$HERE/tla/$spec.tla" "$HERE/tla/${spec}Trace.tla" "$HERE/tla/${spec}Trace.cfg" "$t"
  data=$(ls "$t" | grep -v "^$spec\|^SPEC" | head -1)
  out=$(cd "$t" && "$TLC" -workers 1 -cleanup -config "${spec}Trace.cfg" "${spec}Trace.tla" 2>&1)
  if [ $? -ne 0 ]; then
    i=$(echo "$out" | grep -o '^/\\ i = [0-9]*' | tail -1 | grep -o '[0-9]*$')
    echo "FAIL $(basename "$t"), call $i:"
    if [ -n "$i" ]; then
      start=$(grep -n '^Trace == <<' "$t/$data" | cut -d: -f1)
      sed -n "$((start + i))p" "$t/$data"
      echo "$spec says:"; echo "$out" | awk '/^State /{s=""} {s=s"\n"$0} END{print s}' | grep -E '^ *(ret|got|alive|ok) \|->'
    else
      echo "$out" | grep -v '^Parsing\|^Semantic\|^Starting\|^Implied\|^Computing\|^Progress\|^Linting' | tail -40
    fi
    if [ -n "${KEEP:-}" ]; then
      mkdir -p "$KEEP" && cp -r "$t" "$KEEP/" && echo "$out" > "$KEEP/$(basename "$t")/tlc.out"
    fi
    fail=1
  fi
  n=$((n + 1))
done
echo "$n traces checked"
rm -rf "$dir"
exit $fail
