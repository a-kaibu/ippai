#!/bin/bash
# Builds sake.wasm: the from-scratch Win32 layer (crates/sake) + vnhost's x86 run loop, JIT,
# file system, display and audio, without Wine. Outputs:
#   $OUT/sake.js                 node (NODERAWFS): node sake.js /path/to/game.exe
#   web/public/sake/sake.js     the page (ES module factory createSake, like wine.js)
set -e
EMSDK4=${EMSDK4:-/home/user/emsdk4}
OUT=${OUT:-/home/user/work/sake-out}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
HO=$ROOT/host
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1
mkdir -p "$OUT"
cd "$ROOT"
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals -C panic=abort" \
  cargo build --release -p sake --target wasm32-unknown-emscripten -Zbuild-std=std,panic_abort
LIB=$ROOT/target/wasm32-unknown-emscripten/release/libsake.a
emcc -O2 -pthread -c "$HERE/c/main.c" -o "$OUT/main.o"
emcc -O2 -pthread -c "$ROOT/crates/vnhost/c/shim.c" -o "$OUT/shim.o"
# the libc entry points vnhost routes to its fd layer and the page's file system (not mmap:
# guest memory is ours, the host's mmap stays emscripten's)
WRAP="socket socketpair pipe pipe2 bind listen accept accept4 connect shutdown setsockopt getsockopt
  getsockname getpeername sendmsg recvmsg send recv sendto recvfrom read write readv writev close dup dup2
  poll select fstat lseek fcntl ioctl dlopen dlsym dlclose dlerror dladdr kill pthread_kill isatty getpid exit _exit
  getenv setenv unsetenv putenv pthread_create open openat creat stat lstat fstatat access faccessat readlink mkdir
  rmdir unlink rename symlink link chmod utimensat chdir fchdir getcwd realpath opendir fdopendir readdir closedir
  rewinddir dirfd pread pwrite ftruncate fsync fchmod futimens fstatfs fstatvfs posix_fallocate fgetxattr fsetxattr
  fremovexattr getxattr fopen fdopen clock_gettime sched_yield emscripten_futex_wait"
WRAPFLAGS=$(for w in $WRAP; do printf -- "-Wl,--wrap=%s -Wl,-u,%s " "$w" "$w"; done)
KEEP=$("$EMSDK/upstream/bin/llvm-nm" --defined-only -j "$LIB" 2>/dev/null | grep -E '^(vnhost_|sake_)' | sort -u)
EXPORTS=$(echo "$KEEP" | sed 's/^/_/' | paste -sd, -)
link() {  # <output .js> <emcc flags...>
  local out=$1
  shift
  emcc -O2 ${SAKE_LDEXTRA:-} -pthread -fwasm-exceptions -sSUPPORT_LONGJMP=wasm -Wno-undefined -o "$out" "$OUT/main.o" "$OUT/shim.o" "$LIB" \
    $WRAPFLAGS --pre-js "$HO/pre.js" --js-library "$HO/vnfs.js" --js-library "$HO/vnjit.js" \
    -sPROXY_TO_PTHREAD -sEXIT_RUNTIME \
    -sGLOBAL_BASE=2147483648 -sINITIAL_MEMORY=4294901760 -sALLOW_MEMORY_GROWTH=0 \
    -sSTACK_SIZE=1MB -sDEFAULT_PTHREAD_STACK_SIZE=1MB -sPTHREAD_POOL_SIZE=16 -sPTHREAD_POOL_SIZE_STRICT=0 \
    -sERROR_ON_UNDEFINED_SYMBOLS=0 -sDEFAULT_LIBRARY_FUNCS_TO_INCLUDE=__assert_fail "$@" \
    2> "$OUT/link.log" || { grep -i "undefined\|error" "$OUT/link.log" | sort -u | head -80; exit 1; }
  grep -i "undefined symbol" "$OUT/link.log" | sort -u | head -40 || true
}
link "$OUT/sake.js" -sNODERAWFS=1 -sENVIRONMENT=node -sEXPORTED_FUNCTIONS=_main,$EXPORTS
WEB_OUT=${WEB_OUT:-$ROOT/web/public/sake}
mkdir -p "$WEB_OUT"
# (no emscripten file system: the page mounts a tree at "/", crates/vnhost/src/vfs.rs)
link "$WEB_OUT/sake.js" -sENVIRONMENT=web,worker -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createSake \
  --js-library "$HO/vngpu.js" -sFILESYSTEM=0 -sEXPORTED_RUNTIME_METHODS=HEAPU8,HEAPU32,HEAPF32,HEAPF64,wasmMemory \
  -sEXPORTED_FUNCTIONS=_main,_malloc,_free,$EXPORTS
(cd "$ROOT" && find crates/vnx86/src crates/vnx86/Cargo.toml crates/vnhost/src/jit.rs crates/vnhost/src/cpu.rs host/vnjit.js -type f | sort | xargs cat) |
  sha256sum | cut -c1-8 > "$WEB_OUT/jit-version.txt"
echo "built $OUT/sake.js and $WEB_OUT/sake.js"
