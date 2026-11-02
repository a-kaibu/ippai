#!/bin/bash
# Builds vnwin.wasm: the from-scratch Win32 layer (crates/vnwin) + vnhost's x86 run loop, JIT,
# file system, display and audio, without Wine. Outputs:
#   $OUT/vnwin.js                 node (NODERAWFS): node vnwin.js /path/to/game.exe
#   web/public/vnwin/vnwin.js     the page (ES module factory createVnwin, like wine.js)
set -e
EMSDK4=${EMSDK4:-/home/user/emsdk4}
OUT=${OUT:-/home/user/work/vnwin-out}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
HO=$ROOT/hangover
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1
mkdir -p "$OUT"
cd "$ROOT"
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals -C relocation-model=pic -C panic=abort" \
  cargo build --release -p vnwin --target wasm32-unknown-emscripten -Zbuild-std=std,panic_abort
LIB=$ROOT/target/wasm32-unknown-emscripten/release/libvnwin.a
# vkd3d-shader for Direct3D 11 shaders (vnhost's d3d11, the library hangover/build.sh builds)
VKD3D=${VKD3D_OUT:-/home/user/work/vkd3d-wasm}/libvkd3d-shader.a
[ -f "$VKD3D" ] || { echo "no $VKD3D: run hangover/build.sh (or hangover/build-vkd3d.sh) first"; exit 1; }
emcc -O2 -pthread -fPIC -c "$HERE/c/main.c" -o "$OUT/main.o"
emcc -O2 -pthread -fPIC -c "$ROOT/crates/vnhost/c/shim.c" -o "$OUT/shim.o"
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
KEEP=$("$EMSDK/upstream/bin/llvm-nm" --defined-only -j "$LIB" 2>/dev/null | grep -E '^(vnhost_|vnwin_)' | sort -u)
EXPORTS=$(echo "$KEEP" | sed 's/^/_/' | paste -sd, -)
link() {  # <output .js> <emcc flags...>
  local out=$1
  shift
  emcc -O2 ${VNWIN_LDEXTRA:-} -pthread -fwasm-exceptions -sSUPPORT_LONGJMP=wasm -sMAIN_MODULE=2 -Wno-undefined -o "$out" "$OUT/main.o" "$OUT/shim.o" "$LIB" "$VKD3D" \
    $WRAPFLAGS --pre-js "$HO/pre.js" --js-library "$HO/vnfs.js" --js-library "$HO/vnjit.js" --js-library "$HO/vndl.js" --js-library "$HO/vnwasm.js" \
    -sPROXY_TO_PTHREAD -sEXIT_RUNTIME \
    -sGLOBAL_BASE=2147483648 -sINITIAL_MEMORY=2214592512 -sMAXIMUM_MEMORY=4GB -sALLOW_MEMORY_GROWTH=1 \
    -sSTACK_SIZE=1MB -sDEFAULT_PTHREAD_STACK_SIZE=1MB -sPTHREAD_POOL_SIZE=16 -sPTHREAD_POOL_SIZE_STRICT=0 \
    -sERROR_ON_UNDEFINED_SYMBOLS=0 -sDEFAULT_LIBRARY_FUNCS_TO_INCLUDE=__assert_fail "$@" \
    2> "$OUT/link.log" || { grep -i "undefined\|error" "$OUT/link.log" | sort -u | head -80; exit 1; }
  grep -i "undefined symbol" "$OUT/link.log" | sort -u | head -40 || true
}
link "$OUT/vnwin.js" -sNODERAWFS=1 -sENVIRONMENT=node -sEXPORTED_FUNCTIONS=_main,$EXPORTS
WEB_OUT=${WEB_OUT:-$ROOT/web/public/vnwin}
mkdir -p "$WEB_OUT"
link "$WEB_OUT/vnwin.js" -sENVIRONMENT=web,worker -sMODULARIZE -sEXPORT_ES6 -sEXPORT_NAME=createVnwin \
  -sFORCE_FILESYSTEM -sEXPORTED_RUNTIME_METHODS=FS,ENV,HEAPU8,HEAPU32,HEAPF32,HEAPF64,wasmMemory \
  -sEXPORTED_FUNCTIONS=_main,_malloc,$EXPORTS
(cd "$ROOT" && find crates/vnx86/src crates/vnx86/Cargo.toml crates/vnhost/src/jit.rs crates/vnhost/src/cpu.rs hangover/vnjit.js -type f | sort | xargs cat) |
  sha256sum | cut -c1-8 > "$WEB_OUT/jit-version.txt"
echo "built $OUT/vnwin.js and $WEB_OUT/vnwin.js"
