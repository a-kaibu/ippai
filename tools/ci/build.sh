#!/bin/bash
# Builds a release of vnvm from a fresh checkout: sake.wasm (with vkd3d-shader), the page, and
# vnvm-server / vnvm-pack, then packs them into $DIST/vnvm-<version>.tar.zst (+ .sha256).
# Used by .github/workflows/ci.yml and runnable anywhere with the same tools:
#   apt: bison flex spirv-headers libvulkan-dev mingw-w64 zstd; rustup (rust-src,
#   wasm32-unknown-emscripten); node 20+. Emscripten is downloaded into $EMSDK4.
#
#   tools/ci/build.sh [version]          (default: git describe)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=${1:-$(git -C "$ROOT" describe --tags --always)}
export EMSDK4=${EMSDK4:-$HOME/emsdk4}
export OUT=${OUT:-$ROOT/target/sake-out}
export VKD3D_OUT=${VKD3D_OUT:-$ROOT/target/vkd3d-wasm}
DIST=${DIST:-$ROOT/target/dist}
# the emscripten release the project builds with (4.0.15)
EMSCRIPTEN_HASH=b412b6307e541b93dd93f01b61181e15c17302ec

if [ ! -f "$EMSDK4/emsdk_env.sh" ]; then
  mkdir -p "$EMSDK4"
  curl -fsSL "https://storage.googleapis.com/webassembly/emscripten-releases-builds/linux/$EMSCRIPTEN_HASH/wasm-binaries.tar.xz" | tar xJ -C "$EMSDK4"
  ln -sfn install "$EMSDK4/upstream"
  printf "LLVM_ROOT = '%s/upstream/bin'\nBINARYEN_ROOT = '%s/upstream'\nNODE_JS = '%s'\n" "$EMSDK4" "$EMSDK4" "$(command -v node)" > "$EMSDK4/.emscripten"
  printf 'export EMSDK=%s\nexport EM_CONFIG=%s/.emscripten\nexport PATH=%s/upstream/emscripten:%s/upstream/bin:$PATH\n' "$EMSDK4" "$EMSDK4" "$EMSDK4" "$EMSDK4" > "$EMSDK4/emsdk_env.sh"
fi

cd "$ROOT"
bash host/build-vkd3d.sh
WEB_OUT=$ROOT/web/public/sake bash crates/sake/build.sh
(cd web && npm ci --no-audit --no-fund && npx vp build)
cargo build --release -p vnvm-server -p vnvm-pack

mkdir -p "$DIST"
STAGE=$(mktemp -d)
P=$STAGE/vnvm-$VERSION
mkdir -p "$P"
cp -r web/dist "$P/web"
cp target/release/vnvm-server target/release/vnvm-pack "$P/"
cp "$EMSDK4/upstream/bin/wasm-opt" "$P/"  # (the server optimizes saved translations with it)
echo "$VERSION $(git rev-parse HEAD)" > "$P/VERSION"
tar -C "$STAGE" -cf - "vnvm-$VERSION" | zstd -19 -T0 -q -o "$DIST/vnvm-$VERSION.tar.zst" -f
(cd "$DIST" && sha256sum "vnvm-$VERSION.tar.zst" > "vnvm-$VERSION.tar.zst.sha256")
rm -rf "$STAGE"
echo "built $DIST/vnvm-$VERSION.tar.zst"
