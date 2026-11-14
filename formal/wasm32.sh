#!/bin/bash
# The parsers of guest-supplied data (shaders, traces, movies, installers' tables, the CRT's
# number formatting) are what sake runs on wasm32: usize is 32 bits there, and the tests on the
# 64-bit host never see a length or an offset wrap. Their whole test suites (proptest included)
# on wasm32-wasip1 under node's WASI, with overflow checks on in release, so any wrap is a
# failure; a panic aborts the run as it would abort the VM (panic=abort there).
#   needs: rustup target wasm32-wasip1, node 24+ (node 22 segfaults at times after the deep tests)
#   formal/wasm32.sh [crate ...]   (default: all of them)
set -euo pipefail
cd "$(dirname "$0")/.."
crates=("$@")
[ ${#crates[@]} -gt 0 ] || crates=(asf mpeg1 msidb apitrace dxwgsl hlslc crt)
# (deep but bounded recursion, e.g. hlslc's nesting limits, takes more of the native stack in
# V8 than the 1 MiB of the guest's own linear-memory stack; node's default overflows first)
ulimit -s unlimited 2>/dev/null || ulimit -s 65536
export CARGO_TARGET_WASM32_WASIP1_RUNNER="node --no-warnings --stack-size=30000 $PWD/formal/wasi-run.mjs"
export CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target/wasm32-tests}
for c in "${crates[@]}"; do
  echo "== $c"
  cargo test --release --target wasm32-wasip1 -p "$c" --lib
done
