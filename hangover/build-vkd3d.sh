#!/bin/bash
# vkd3d-shader (Wine's copy in libs/vkd3d) as a static library for vnhost's Direct3D 11:
# shader bytecode -> SPIR-V (hangover/vkd3d/vnshader.c), then naga makes WGSL.
# With NATIVE=1 it builds for this machine (tests) instead of wasm.
#
# env: WINE_SRC (patched Wine source), BUILD (the wasm Wine build: generated headers),
#      WINE_BUILD (a Wine build that generated vkd3d's parsers: hlsl.tab.c, ...),
#      VKD3D_OUT (output directory)
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
WINE_SRC=${WINE_SRC:-/home/user/work/wine-src}
BUILD=${BUILD:-/home/user/work/wine-wb4}
WINE_BUILD=${WINE_BUILD:-/home/user/img/buildroot/tmp/vnbox-build/wine-build-src/build}
V=$WINE_SRC/libs/vkd3d
G=$WINE_BUILD/libs/vkd3d/libs/vkd3d-shader
if [ -n "$NATIVE" ]; then
  OUT=${VKD3D_OUT:-/home/user/work/vkd3d-native}
  CC="gcc -O1 -fPIC -w"
  AR=ar
  ARCH=
else
  OUT=${VKD3D_OUT:-/home/user/work/vkd3d-wasm}
  . "${EMSDK4:-/home/user/emsdk4}/emsdk_env.sh" >/dev/null 2>&1
  CC="emcc -O2 -pthread -fPIC"
  AR=emar
  ARCH=-D__i386__
fi
mkdir -p "$OUT/cfg"
grep -v STRTOF "$V/config.h" > "$OUT/cfg/config.h"  # (no _strtof_l outside Windows)
cd "$OUT"
FLAGS="$ARCH -DWINE_UNIX_LIB -DWINE_NO_NAMELESS_EXTENSION -DLIBVKD3D_SHADER_SOURCE -DHAVE_SYNC_ADD_AND_FETCH=1
  -DHAVE_SYNC_BOOL_COMPARE_AND_SWAP=1 -DHAVE_ATOMIC_EXCHANGE_N=1 -DHAVE_BUILTIN_CLZ=1 -DHAVE_BUILTIN_CTZ=1
  -I$OUT/cfg -I$V -I$V/include -I$V/include/private -I$V/libs/vkd3d-shader -I$G -I$BUILD/include -I$WINE_SRC/include
  -idirafter $WINE_BUILD/include"
for f in $V/libs/vkd3d-common/{blob,debug,error,memory,utf8}.c \
  $V/libs/vkd3d-shader/{checksum,d3d_asm,d3dbc,dxbc,dxil,fx,glsl,hlsl,hlsl_codegen,hlsl_constant_ops,ir,msl,spirv,tpf,vkd3d_shader_main}.c \
  $G/{hlsl.tab,hlsl.yy,preproc.tab,preproc.yy}.c; do
  o=$(basename "$f" .c).o
  [ "$o" -nt "$f" ] || $CC $FLAGS -c "$f" -o "$o"
done
$CC -I"$V/include" -c "$HERE/vkd3d/vnshader.c" -o vnshader.o
rm -f libvkd3d-shader.a
$AR rcs libvkd3d-shader.a *.o
echo "built $OUT/libvkd3d-shader.a"
