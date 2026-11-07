#!/bin/bash
# rust-src and the wasm32-unknown-emscripten standard library for the stable toolchain, unless
# the cached ones are there (actions/cache of their lib/rustlib directories, key RUST_KEY).
set -euo pipefail
lib=$(rustc --print sysroot)/lib/rustlib
if [ -d "$lib/src/rust/library" ] && [ -d "$lib/wasm32-unknown-emscripten/lib" ] && rustup component list --installed | grep -q '^rust-src' && rustup component list --installed | grep -q '^rust-std-wasm32-unknown-emscripten'; then
  echo "rust-wasm: cached"
else
  rustup component add rust-src
  rustup target add wasm32-unknown-emscripten
fi
