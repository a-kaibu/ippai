#!/bin/bash
# Builds sake.wasm: the from-scratch Win32 layer (crates/sake) + kura's x86 run loop, JIT,
# file system, display and audio, without Wine. Outputs:
#   $OUT/sake.js                 node (NODERAWFS): node sake.js /path/to/game.exe
#   web/public/sake/sake.js     the page (ES module factory createSake, like wine.js)
#   sake-grow.js / .wasm         both again with a memory that starts at 2.06 GB and grows to 4 GB
#                                (the page's fallback where 4 GB at once is refused: Android
#                                devices with little RAM; koji::jit::wasm::GROW_MIN_PAGES)
set -e
EMSDK4=${EMSDK4:-/home/user/emsdk4}
OUT=${OUT:-/home/user/work/sake-out}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
HO=$ROOT/host
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1
mkdir -p "$OUT"
cd "$ROOT"
RUSTC_BOOTSTRAP=1 RUSTFLAGS="${RUSTFLAGS:-} -D warnings -C target-feature=+atomics,+bulk-memory,+mutable-globals -C panic=abort" \
  cargo build --release -p sake --target wasm32-unknown-emscripten -Zbuild-std=std,panic_abort
LIB=$ROOT/target/wasm32-unknown-emscripten/release/libsake.a
emcc -O2 -pthread -c "$HERE/c/main.c" -o "$OUT/main.o"
emcc -O2 -pthread -c "$ROOT/crates/kura/c/shim.c" -o "$OUT/shim.o"
# the libc entry points kura takes over (crates/kura/src/wrap.rs, c/shim.c)
WRAP="write writev clock_gettime sched_yield emscripten_futex_wait"
WRAPFLAGS=$(for w in $WRAP; do printf -- "-Wl,--wrap=%s -Wl,-u,%s " "$w" "$w"; done)
KEEP=$("$EMSDK/upstream/bin/llvm-nm" --defined-only -j "$LIB" 2>/dev/null | grep -E '^(vnhost_|sake_)' | sort -u)
EXPORTS=$(echo "$KEEP" | sed 's/^/_/' | paste -sd, -)
# the page's: only what its scripts, host/*.js and the tests call (fewer exports, and the rest
# can go as dead code)
WEB_EXPORTS=$( (grep -rhoE '_(vnhost|sake)_[a-z0-9_]+' "$ROOT/web/src" "$ROOT/tests" --include='*.ts' --include='*.tsx' --include='*.mjs';
  grep -hoE "(wasmExports\.|call\(')(vnhost|sake)_[a-z0-9_]+" "$HO"/*.js | sed -E "s/^(wasmExports\.|call\(')/_/") |
  sort -u | grep -xF -f <(echo "$KEEP" | sed 's/^/_/') | paste -sd, -)
# sake.wasm's memory: the whole 4 GB from the start, never growing (V8 then checks accesses
# against a size it loads once per function: docs/translator.md); sake-grow.wasm's: from
# koji::jit::wasm::GROW_MIN_PAGES pages
FIXED="-sINITIAL_MEMORY=4294901760 -sALLOW_MEMORY_GROWTH=0"
GROW_PAGES=$(grep -oP 'GROW_MIN_PAGES: u32 = \K[0-9]+' "$ROOT/crates/koji/src/jit/wasm.rs")
GROW="-sINITIAL_MEMORY=$((GROW_PAGES * 65536)) -sALLOW_MEMORY_GROWTH=1 -sMAXIMUM_MEMORY=4294901760"
link() {  # <output .js> <memory flags> <emcc flags...>
  local out=$1 memory=$2
  shift 2
  local log="$OUT/link-$(basename "$(dirname "$out")")-$(basename "$out").log"
  emcc -O2 ${SAKE_LDEXTRA:-} -pthread -fwasm-exceptions -sSUPPORT_LONGJMP=wasm -Wno-undefined -o "$out" "$OUT/main.o" "$OUT/shim.o" "$LIB" \
    $WRAPFLAGS --pre-js "$HO/fetch-bridge.js" --pre-js "$HO/pre.js" --pre-js "$HO/tls-worker.js" --js-library "$HO/vnfs.js" --js-library "$HO/vnjit.js" --js-library "$HO/vnkern.js" --js-library "$HO/tls-bridge.js" --js-library "$HO/account-bridge.js" \
    -sPROXY_TO_PTHREAD -sEXIT_RUNTIME \
    -sGLOBAL_BASE=2147483648 $memory \
    -sSTACK_SIZE=1MB -sDEFAULT_PTHREAD_STACK_SIZE=1MB -sPTHREAD_POOL_SIZE=16 -sPTHREAD_POOL_SIZE_STRICT=0 \
    -sERROR_ON_UNDEFINED_SYMBOLS=0 -sDEFAULT_LIBRARY_FUNCS_TO_INCLUDE=__assert_fail "$@" \
    2> "$log" || { grep -i "undefined\|error" "$log" | sort -u | head -80; return 1; }
  grep -i "undefined symbol" "$log" | sort -u | head -40 || true
}
# (node: a factory too, run by host/node-main.js, so that the processes a program starts run as
# instances of their own in the same node; host/snap.js: save states under node)
NODE="-sNODERAWFS=1 -sENVIRONMENT=node -sMODULARIZE -sEXPORT_NAME=createSake --pre-js $HO/node-stdio.js --pre-js $HO/snap.js --extern-post-js $HO/node-main.js -sEXPORTED_FUNCTIONS=_main,$EXPORTS"
# the page's zstd for save states (crates/sake/snapz/snapz.c): Facebook's release, checked; built
# while the links run
snapz() {
  ZV=1.5.7
  ZSUM=eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3
  if [ ! -d "$OUT/zstd-$ZV" ]; then
    curl -sSfL --retry 4 -o "$OUT/zstd.tar.gz" "https://github.com/facebook/zstd/releases/download/v$ZV/zstd-$ZV.tar.gz"
    echo "$ZSUM  $OUT/zstd.tar.gz" | sha256sum -c --quiet
    tar xzf "$OUT/zstd.tar.gz" -C "$OUT"
  fi
  Z=$OUT/zstd-$ZV/lib
  emcc -O3 -DZSTD_DISABLE_ASM -DZSTD_MULTITHREAD=0 -DZSTD_LEGACY_SUPPORT=0 -I"$Z" -I"$Z/common" "$HERE/snapz/snapz.c" \
    "$Z"/common/*.c "$Z"/compress/*.c "$Z"/decompress/*.c -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createSnapz \
    -sENVIRONMENT=worker,web,node -sALLOW_MEMORY_GROWTH -sFILESYSTEM=0 -sEXPORTED_FUNCTIONS=_malloc,_free \
    -sEXPORTED_RUNTIME_METHODS=HEAPU8 -o "$WEB_OUT/snapz.mjs"
}
# (the four links at once: each takes a core for a minute)
# (SAKE_NODE=0: the page's two only, as the release builds: .github/workflows/release.yml)
pids=()
if [ "${SAKE_NODE:-1}" != 0 ]; then
  link "$OUT/sake.js" "$FIXED" $NODE & pids+=($!)
  link "$OUT/sake-grow.js" "$GROW" $NODE & pids+=($!)
fi
WEB_OUT=${WEB_OUT:-$ROOT/web/public/sake}
mkdir -p "$WEB_OUT"
snapz & pids+=($!)
# (no emscripten file system: the page mounts a tree at "/", crates/kura/src/vfs.rs)
WEB="-sENVIRONMENT=web,worker -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createSake --js-library $HO/vngpu.js -sFILESYSTEM=0
  -sEXPORTED_RUNTIME_METHODS=HEAPU8,HEAPU32,HEAPF32,HEAPF64,wasmMemory -sEXPORTED_FUNCTIONS=_main,_malloc,_free,$WEB_EXPORTS"
link "$WEB_OUT/sake.js" "$FIXED" $WEB & pids+=($!)
link "$WEB_OUT/sake-grow.js" "$GROW" $WEB & pids+=($!)
for p in "${pids[@]}"; do wait "$p"; done
# each build's id: a state (docs/savestate.md) is of one build (host/snap.js computes the same)
for b in sake sake-grow; do echo "$b $(sha256sum "$WEB_OUT/$b.wasm" | cut -c1-16)"; done > "$WEB_OUT/build-id.txt"
(cd "$ROOT" && find crates/koji/src crates/koji/Cargo.toml crates/kura/src/jit.rs crates/kura/src/cpu.rs host/vnjit.js -type f | sort | xargs cat) |
  sha256sum | cut -c1-8 > "$WEB_OUT/jit-version.txt"
echo "built $OUT/sake.js and $WEB_OUT/sake.js"
