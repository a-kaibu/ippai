#!/bin/bash
# rust-src and the wasm32-unknown-emscripten standard library for the stable toolchain, unless
# the cached ones are there (actions/cache of their lib/rustlib files, key RUST_KEY; rustup's own
# record of them, components and multirust-config.toml, is in the cache too).
set -euo pipefail
lib=$(rustc --print sysroot)/lib/rustlib
if [ -d "$lib/src/rust/library/core" ] && [ -d "$lib/wasm32-unknown-emscripten/lib" ]; then
  echo "rust-wasm: cached"
else
  rustup component add rust-src
  rustup target add wasm32-unknown-emscripten
fi
