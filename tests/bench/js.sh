#!/bin/bash
# The "js" benchmark group: cpu.c's computing cases and host/tests/bench.c compiled straight to
# wasm (emcc, SIMD on), run in V8 (node) and JavaScriptCore (jsvu's build of its shell) by
# tests/bench/js.mjs, with dyn.js (wasm generated at run time, as koji's). Emscripten comes from
# tools/ci/emsdk.sh. The JavaScriptCore build is jsvu's newest unless JSC_REV pins one (the
# report's 計測条件 shows the revision each run used).
#
#   tests/bench/js.sh OUT.json [WORK]      (JSC=path to the jsc shell skips installing it)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=$(realpath -m "$1")
mkdir -p "$(dirname "$OUT")"
WORK=${2:-$(mktemp -d)}
mkdir -p "$WORK"
. "$ROOT/tools/ci/emsdk.sh"
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1
FLAGS="-O2 -msimd128 -msse2 -sENVIRONMENT=node,shell -sSINGLE_FILE=1 -sALLOW_MEMORY_GROWTH=1"
emcc $FLAGS "$ROOT/tests/bench/src/cpu.c" -o "$WORK/cpu.js"
emcc $FLAGS -I"$ROOT/host/tests/wasm" "$ROOT/host/tests/bench.c" -o "$WORK/bench.js"
cp "$ROOT/tests/bench/dyn.js" "$WORK/"
JSC_VERSION=unknown
if [ -z "${JSC:-}" ]; then
  npx -y jsvu@3.0.6 --os=linux64 --engines="javascriptcore${JSC_REV:+@$JSC_REV}" >/dev/null
  JSC=$HOME/.jsvu/bin/javascriptcore
  JSC_VERSION=$(python3 -c 'import json, os, sys; print(json.load(open(os.path.expanduser("~/.jsvu/status.json"))).get("javascriptcore", "unknown"))' 2>/dev/null || echo unknown)
fi
EMCC_VERSION=$(emcc --version | head -1 | sed 's/.*) //; s/ .*//')
VERSIONS="emscripten=$EMCC_VERSION javascriptcore=$JSC_VERSION jsvu=3.0.6" \
  node "$ROOT/tests/bench/js.mjs" "$OUT" "$WORK" v8=node "jsc=$JSC"
