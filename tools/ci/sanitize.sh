#!/bin/bash
# The workspace's tests under a sanitizer (nightly): ASan (out of bounds, use after free,
# leaks) or TSan (data races between the guest threads, the JIT's compiler thread, movie and
# audio workers). The unsafe code these catch: guest memory through raw pointers, MAP_32BIT
# arenas, ring buffers, the C runtime's own heap.
#   tools/ci/sanitize.sh address|thread [cargo test args...]
# (default packages: everything with native tests; TSan builds std instrumented, -Zbuild-std)
set -euo pipefail
cd "$(dirname "$0")/../.."
kind=${1:-address}
shift || true
pkgs=("$@")
[ ${#pkgs[@]} -gt 0 ] || pkgs=(-p koji -p kura -p sake -p crt -p vorbis -p hlslc -p dxwgsl -p glwgsl -p shaderconf -p ippai)
export RUSTFLAGS="-Zsanitizer=$kind" RUSTDOCFLAGS="-Zsanitizer=$kind"
export LSAN_OPTIONS="suppressions=$PWD/tools/ci/lsan.supp:print_suppressions=0"
export ASAN_OPTIONS="detect_leaks=1:allocator_may_return_null=1"
# The bundled C libraries (SQLite, libzstd) stay uninstrumented: cc would build them with GCC's
# ASan against rustc's LLVM runtime, which reports SQLite globals as ODR violations and a
# stack-use-after-scope on a global in libzstd.
export CFLAGS="-fno-sanitize=address" CXXFLAGS="-fno-sanitize=address"
export TSAN_OPTIONS="halt_on_error=1:second_deadlock_stack=1"
extra=()
[ "$kind" = thread ] && extra=(-Zbuild-std)
CARGO_TARGET_DIR=target/sanitize-$kind cargo +nightly test "${extra[@]}" --release --target x86_64-unknown-linux-gnu "${pkgs[@]}" --no-fail-fast
