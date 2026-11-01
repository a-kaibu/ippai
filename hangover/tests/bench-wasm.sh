#!/bin/bash
# bench.c compiled straight to wasm (no x86, no Wine) into web/public/benchwasm/, which
# web/bench.html?wasm runs: what the device itself does with the same kernels
set -e
cd "$(dirname "$0")"
. "${EMSDK4:-/home/user/emsdk4}/emsdk_env.sh" >/dev/null 2>&1
mkdir -p ../../web/public/benchwasm
emcc -O2 -Iwasm bench.c -o ../../web/public/benchwasm/bench.js \
  -sMODULARIZE -sEXPORT_ES6 -sINVOKE_RUN=0 -sALLOW_MEMORY_GROWTH -sSTACK_SIZE=1048576 -sEXIT_RUNTIME=0 -sEXPORTED_RUNTIME_METHODS=callMain
