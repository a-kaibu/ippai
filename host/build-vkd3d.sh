#!/bin/bash
# vkd3d-shader (upstream vkd3d, the release Wine 11 carries) as a static library for vnhost's
# Direct3D 11: shader bytecode -> SPIR-V (host/vkd3d/vnshader.c), then naga makes WGSL.
# Downloads the release tarball, generates its parsers with bison/flex. With NATIVE=1 it builds
# for this machine (tests) instead of wasm.
#
# needs: bison, flex, SPIR-V and Vulkan headers (apt spirv-headers libvulkan-dev, or
# SPIRV_INCLUDE / VULKAN_INCLUDE)
# env: VKD3D_OUT (output directory), VKD3D_CACHE (downloads), SPIRV_INCLUDE, VULKAN_INCLUDE
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
VERSION=2.1
SHA256=7510146aff2adfb4ae07ab890701a607e5ff7c66e57100cfc9f630ec92eeda6a
CACHE=${VKD3D_CACHE:-$HOME/.cache/vnvm}
SPIRV_INCLUDE=${SPIRV_INCLUDE:-/usr/include}
VULKAN_INCLUDE=${VULKAN_INCLUDE:-/usr/include}
if [ -n "$NATIVE" ]; then
  OUT=${VKD3D_OUT:-/home/user/work/vkd3d-native}
  CC="gcc -O1 -fPIC -w"
  AR=ar
else
  OUT=${VKD3D_OUT:-/home/user/work/vkd3d-wasm}
  . "${EMSDK4:-/home/user/emsdk4}/emsdk_env.sh" >/dev/null 2>&1
  CC="emcc -O2 -pthread -fPIC -w"
  AR=emar
fi
[ -f "$SPIRV_INCLUDE/spirv/unified1/spirv.h" ] || { echo "no SPIR-V headers in $SPIRV_INCLUDE (apt install spirv-headers)"; exit 1; }
[ -f "$VULKAN_INCLUDE/vulkan/vulkan.h" ] || { echo "no Vulkan headers in $VULKAN_INCLUDE (apt install libvulkan-dev)"; exit 1; }

# the source
V=$CACHE/vkd3d-$VERSION
if [ ! -d "$V" ]; then
  mkdir -p "$CACHE"
  curl -fsSL "https://dl.winehq.org/vkd3d/source/vkd3d-$VERSION.tar.xz" -o "$CACHE/vkd3d.tar.xz"
  echo "$SHA256  $CACHE/vkd3d.tar.xz" | sha256sum -c --quiet
  tar xJf "$CACHE/vkd3d.tar.xz" -C "$CACHE"
  rm "$CACHE/vkd3d.tar.xz"
fi

# generated files: parsers, config.h, vkd3d_version.h
G=$OUT/gen
mkdir -p "$G"
S=$V/libs/vkd3d-shader
for p in hlsl preproc; do
  [ "$G/$p.tab.c" -nt "$S/$p.y" ] || bison -d -o "$G/$p.tab.c" "$S/$p.y"
  [ "$G/$p.yy.c" -nt "$S/$p.l" ] || flex -o "$G/$p.yy.c" "$S/$p.l"
done
cat > "$G/config.h" <<EOF
#define PACKAGE_NAME "vkd3d"
#define PACKAGE_STRING "vkd3d $VERSION"
#define PACKAGE_VERSION "$VERSION"
#define HAVE_SPIRV_UNIFIED1_SPIRV_H 1
#define HAVE_SPIRV_UNIFIED1_GLSL_STD_450_H 1
#define HAVE_BUILTIN_CLZ 1
#define HAVE_BUILTIN_CTZ 1
#define HAVE_BUILTIN_POPCOUNT 1
#define HAVE_SYNC_ADD_AND_FETCH 1
#define HAVE_SYNC_SUB_AND_FETCH 1
#define HAVE_SYNC_BOOL_COMPARE_AND_SWAP 1
#define HAVE_ATOMIC_EXCHANGE_N 1
EOF
echo "#define VKD3D_VCS_ID \"\"" > "$G/vkd3d_version.h"

# only the spirv/ and vulkan/ headers: -I/usr/include would put the system libc in front of
# Emscripten's
mkdir -p "$G/inc"
for d in "$SPIRV_INCLUDE/spirv" "$VULKAN_INCLUDE/vulkan" "$VULKAN_INCLUDE/vk_video"; do
  [ -d "$d" ] && ln -sfn "$d" "$G/inc/$(basename "$d")"
done

cd "$OUT"
FLAGS="-DLIBVKD3D_SHADER_SOURCE -I$G -I$V/include -I$V/include/private -I$S -I$G/inc"
for f in $V/libs/vkd3d-common/{blob,debug,error,memory,utf8}.c \
  $S/{checksum,d3d_asm,d3dbc,dxbc,dxil,fx,glsl,hlsl,hlsl_codegen,hlsl_constant_ops,ir,msl,spirv,tpf,vkd3d_shader_main}.c \
  $G/{hlsl.tab,hlsl.yy,preproc.tab,preproc.yy}.c; do
  o=$(basename "$f" .c).o
  [ "$o" -nt "$f" ] || $CC $FLAGS -include "$G/config.h" -c "$f" -o "$o"
done
$CC -I"$V/include" -c "$HERE/vkd3d/vnshader.c" -o vnshader.o
rm -f libvkd3d-shader.a
$AR rcs libvkd3d-shader.a *.o
echo "built $OUT/libvkd3d-shader.a"
