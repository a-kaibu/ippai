#!/bin/bash
# The workspace's tests under a sanitizer (nightly): ASan (out of bounds, use after free,
# leaks) or TSan (data races between the guest threads, the JIT's compiler thread, movie and
# audio workers). The unsafe code these catch: guest memory through raw pointers, MAP_32BIT
# arenas, ring buffers, the C runtime's own heap.
#   tools/ci/sanitize.sh address|thread [cargo test args...]
# (default packages: everything with native tests, the lock words (waits), wmcodec's SIMD and the TLS
# client too; TSan builds std instrumented, -Zbuild-std)
set -euo pipefail
cd "$(dirname "$0")/../.."
kind=${1:-address}
shift || true
pkgs=("$@")
[ ${#pkgs[@]} -gt 0 ] || pkgs=(-p koji -p kura -p sake -p crt -p vorbis -p hlslc -p dxwgsl -p glwgsl -p shaderconf -p ippai -p waits -p wmcodec -p ippai-tls)
export RUSTFLAGS="-Zsanitizer=$kind" RUSTDOCFLAGS="-Zsanitizer=$kind"
if [ "$kind" = address ]; then
  # Guest test arenas are mmap'd and hold pointers into the normal heap. Their
  # dlsym-based root registration needs this symbol in .dynsym, not just .symtab.
  export RUSTFLAGS="$RUSTFLAGS -Clink-arg=-Wl,--export-dynamic-symbol=__lsan_register_root_region"
  export RUSTDOCFLAGS="$RUSTFLAGS" IPPAI_REQUIRE_LSAN_ROOTS=1
fi
export LSAN_OPTIONS="suppressions=$PWD/tools/ci/lsan.supp:print_suppressions=0"
# (no fake stacks: the d3d tests run on a stack mapped below 4 GiB, trace_tests on_low_stack,
# because the host code hands addresses of its locals to the guest side as 32-bit pointers, as
# it does in wasm; ASan's fake frames for stack-use-after-return live above 4 GiB)
export ASAN_OPTIONS="detect_leaks=1:allocator_may_return_null=1:detect_stack_use_after_return=0"
# The bundled C libraries (SQLite, libzstd) stay uninstrumented: cc would build them with GCC's
# ASan against rustc's LLVM runtime, which reports SQLite globals as ODR violations and a
# stack-use-after-scope on a global in libzstd.
export CFLAGS="-fno-sanitize=address" CXXFLAGS="-fno-sanitize=address"
# A symbolizer is a must: LeakSanitizer matches lsan.supp against function names, so without
# one iced-x86's tables (552 bytes in 12 allocations) are reported as leaks in every binary that
# decodes, and reports show bare addresses. Nightly's own (llvm-tools) first, then the system's.
sym=$(rustc +nightly --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-symbolizer
[ -x "$sym" ] || sym=$(command -v llvm-symbolizer || ls /usr/bin/llvm-symbolizer-* /usr/lib/llvm-*/bin/llvm-symbolizer 2>/dev/null | sort -V | tail -1 || true)
[ -x "$sym" ] || { echo "no llvm-symbolizer: rustup +nightly component add llvm-tools" >&2; exit 1; }
export ASAN_SYMBOLIZER_PATH=$sym TSAN_SYMBOLIZER_PATH=$sym
echo "symbolizer: $sym"
export TSAN_OPTIONS="halt_on_error=1:second_deadlock_stack=1"
extra=()
[ "$kind" = thread ] && extra=(-Zbuild-std)
CARGO_TARGET_DIR=target/sanitize-$kind cargo +nightly test "${extra[@]}" --release --target x86_64-unknown-linux-gnu --no-fail-fast "${pkgs[@]}"
