#!/bin/bash
# loom (https://github.com/tokio-rs/loom): every interleaving of the lock-word protocols of
# crates/waits (critical sections, SRW locks, condition variables, INIT_ONCE, WaitOnAddress),
# with the weak-memory outcomes loom models. LOOM_MAX_PREEMPTIONS (default 3) bounds the
# preemptions per execution: more finds more and takes longer.
set -euo pipefail
cd "$(dirname "$0")/.."
RUSTFLAGS="--cfg loom ${RUSTFLAGS:-}" CARGO_TARGET_DIR=target/loom cargo test -p waits --release --test loom "$@"
