#!/bin/bash
# The tests around unsafe code that hands memory between threads or reads guest memory through
# raw pointers, under Miri (out of bounds, use after free, data races): texture format
# conversion on exactly sized buffers, the movie slots, padded GPU uploads.
#   needs: rustup toolchain nightly with the miri component
set -euo pipefail
cd "$(dirname "$0")/../crates/kura"
cargo +nightly miri test --lib -- every_format_on_exact_buffers dxt_partial_blocks video:: padded_tests::
