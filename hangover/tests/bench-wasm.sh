#!/bin/bash
# bench.c compiled straight to wasm (no x86, no Wine) into web/public/benchwasm/, which
# web/bench.html?wasm runs: what the device itself does with the same kernels. Variants for
# telling apart what costs the JIT: shared.js (shared memory, as Wine's threads need) and big.js
# (shared and as large as wine.wasm's: 2.1 GB initial, 4 GB maximum).
set -e
cd "$(dirname "$0")"
. "${EMSDK4:-/home/user/emsdk4}/emsdk_env.sh" >/dev/null 2>&1
OUT=../../web/public/benchwasm
mkdir -p $OUT
FLAGS="-O2 -Iwasm bench.c -sMODULARIZE -sEXPORT_ES6 -sINVOKE_RUN=0 -sSTACK_SIZE=1048576 -sEXIT_RUNTIME=0 -sEXPORTED_RUNTIME_METHODS=callMain"
emcc $FLAGS -sALLOW_MEMORY_GROWTH -o $OUT/bench.js
emcc $FLAGS -pthread -sINITIAL_MEMORY=64MB -o $OUT/shared.js
emcc $FLAGS -pthread -sINITIAL_MEMORY=2214592512 -sMAXIMUM_MEMORY=4GB -sALLOW_MEMORY_GROWTH -o $OUT/big.js
