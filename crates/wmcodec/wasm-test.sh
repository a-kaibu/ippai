#!/bin/sh
# wmcodec's tests built for wasm with SIMD128 and run under node: the simd module's kernels
# against the scalar code, and the rest as on the host. EMSDK: an emsdk (emsdk_env.sh), node in
# PATH. Args go to the test binary (a filter, --nocapture).
set -e
. "${EMSDK:-$HOME/emsdk4}/emsdk_env.sh" >/dev/null 2>&1
cd "$(dirname "$0")"
CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUNNER=node \
RUSTFLAGS="-C target-feature=+simd128,+bulk-memory -C link-arg=-sNODERAWFS=1 -C link-arg=-sALLOW_MEMORY_GROWTH=1 -C link-arg=-sSTACK_SIZE=8MB" \
  cargo test --release --target wasm32-unknown-emscripten --lib -- "$@"
