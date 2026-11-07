#!/bin/bash
# The nightly toolchain with these components, unless the cached one (actions/cache of
# ~/.rustup/toolchains/nightly-*, key CI_WEEK from tools/ci/cache-keys.sh) already has them;
# then RUST_KEY for it (tools/ci/rust-key.sh nightly).
#   tools/ci/nightly.sh [component...]
set -euo pipefail
if rustup run nightly rustc -V >/dev/null 2>&1; then
  have=$(rustup +nightly component list --installed 2>/dev/null)
  missing=()
  for c in "$@"; do
    grep -q "^$c" <<<"$have" || missing+=("$c")
  done
  [ ${#missing[@]} -eq 0 ] || rustup +nightly component add "${missing[@]}"
else
  rustup toolchain install nightly --profile minimal ${1:+--component "$(IFS=,; echo "$*")"}
fi
rustc +nightly -V
"$(dirname "$0")/rust-key.sh" nightly
