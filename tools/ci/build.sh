#!/bin/bash
# Builds ippai from a fresh checkout: sake.wasm, the page (web/dist), and the ippai binary
# with the page built in, then packs them into $DIST/ippai-<version>.tar.zst (+ .sha256) for the
# corpus runs (tests/corpus/ci.sh: they serve web/ from it). The releases are GoReleaser's
# (.goreleaser.yaml, .github/workflows/release.yml), from the web/dist this makes.
# Used by .github/workflows/ci.yml and runnable anywhere with the same tools:
#   apt: mingw-w64 zstd; rustup (rust-src,
#   wasm32-unknown-emscripten); node 20+. Emscripten is downloaded into $EMSDK4.
#
#   tools/ci/build.sh [version]          (default: git describe)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=${1:-$(git -C "$ROOT" describe --tags --always)}
export EMSDK4=${EMSDK4:-$HOME/emsdk4}
export OUT=${OUT:-$ROOT/target/sake-out}
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
rm -rf "$DIST"  # (a cached target/ would carry older builds into the artifact)
WEB_OUT=$ROOT/web/public/sake bash crates/sake/build.sh
bash host/tests/bench-wasm.sh  # (web/public/benchwasm: bench.html's direct-wasm numbers; not in releases)
(cd web && npm ci --no-audit --no-fund && npx vp build)
cargo build --release -p ippai

mkdir -p "$DIST"
STAGE=$(mktemp -d)
P=$STAGE/ippai-$VERSION
mkdir -p "$P"
cp -r web/dist "$P/web"
cp target/release/ippai "$P/"
cp "$EMSDK4/upstream/bin/wasm-opt" "$P/"  # (the server optimizes saved translations with it)
echo "$VERSION $(git rev-parse HEAD)" > "$P/VERSION"
tar -C "$STAGE" -cf - "ippai-$VERSION" | zstd -19 -T0 -q -o "$DIST/ippai-$VERSION.tar.zst" -f
(cd "$DIST" && sha256sum "ippai-$VERSION.tar.zst" > "ippai-$VERSION.tar.zst.sha256")
rm -rf "$STAGE"
echo "built $DIST/ippai-$VERSION.tar.zst"
