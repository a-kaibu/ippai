#!/bin/bash
# The part of a cargo cache key that says which compiler built it (actions/cache keys: a target
# directory from another rustc is rebuilt from scratch, so restoring it is only wasted time), into
# $GITHUB_ENV as RUST_KEY. Also turns incremental builds off (CI builds once; their state is big).
#   tools/ci/rust-key.sh [toolchain]   (e.g. nightly; default: the one in use)
set -euo pipefail
v=$(rustc ${1:++$1} -vV | sed -n 's/^release: //p; s/^commit-hash: \(.\{9\}\).*/\1/p' | tr '\n' '-')
echo "RUST_KEY=rust-${v%-}" >> "${GITHUB_ENV:-/dev/stdout}"
echo "CARGO_INCREMENTAL=0" >> "${GITHUB_ENV:-/dev/stdout}"
