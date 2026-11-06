#!/bin/bash
# The "js" benchmark group: cpu.c's computing cases and host/tests/bench.c compiled straight to
# wasm (emcc, SIMD on), run in V8 (node) and JavaScriptCore (WebKitGTK's build of its shell) by
# tests/bench/js.mjs, with dyn.js (wasm generated at run time, as koji's). Emscripten comes from
# tools/ci/emsdk.sh. The JavaScriptCore build is pinned (IPPAI_JSC_REV + IPPAI_JSC_SHA256 below: change them
# on purpose, the report's 計測条件 shows the one each run used).
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
# The JavaScriptCore shell: WebKitGTK's nightly bundle of revision IPPAI_JSC_REV (the build jsvu
# would fetch), checked against IPPAI_JSC_SHA256 and kept in IPPAI_JSC_CACHE (bench.yml caches it:
# webkitgtk.org keeps only the last ~40 revisions). Not JSC_*: the shell reads those as its options.
# To move the pin: pick a revision from https://webkitgtk.org/jsc-built-products/x86_64/release/
# and put its zip's sha256 here.
IPPAI_JSC_REV=${IPPAI_JSC_REV:-325822}
IPPAI_JSC_SHA256=${IPPAI_JSC_SHA256:-7f8f80916b7dc0cf7700d8f62deede95cb3bc8e4f36c94334fb01aaa91e62b8f}
JSC_VERSION=unknown
if [ -z "${JSC:-}" ]; then
  IPPAI_JSC_CACHE=${IPPAI_JSC_CACHE:-$HOME/.cache/ippai-jsc}
  dir=$IPPAI_JSC_CACHE/$IPPAI_JSC_REV zip=$IPPAI_JSC_CACHE/$IPPAI_JSC_REV@main.zip
  if [ ! -x "$dir/jsc" ]; then
    mkdir -p "$IPPAI_JSC_CACHE"
    if [ ! -f "$zip" ]; then
      curl -sSfL --retry 5 -o "$zip.part" "https://webkitgtk.org/jsc-built-products/x86_64/release/$IPPAI_JSC_REV@main.zip" || {
        echo "js.sh: JavaScriptCore $IPPAI_JSC_REV@main is not on webkitgtk.org any more (and not cached): move IPPAI_JSC_REV/IPPAI_JSC_SHA256" >&2
        exit 1
      }
      mv "$zip.part" "$zip"
    fi
    echo "$IPPAI_JSC_SHA256  $zip" | sha256sum -c --quiet - || { rm -f "$zip"; echo "js.sh: $zip: wrong sha256" >&2; exit 1; }
    rm -rf "$dir" && mkdir -p "$dir" && unzip -q "$zip" -d "$dir"
  fi
  # (the bundle's own wrapper: it runs bin/jsc with the bundle's loader and libraries)
  JSC=$dir/jsc
  "$JSC" -e 'print("jsc ok")' >/dev/null
  JSC_VERSION=$IPPAI_JSC_REV@main
fi
EMCC_VERSION=$(emcc --version | head -1 | sed 's/.*) //; s/ .*//')
VERSIONS="emscripten=$EMCC_VERSION javascriptcore=$JSC_VERSION" \
  node "$ROOT/tests/bench/js.mjs" "$OUT" "$WORK" v8=node "jsc=$JSC"
