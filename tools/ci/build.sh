#!/bin/bash
# Builds ippai from a fresh checkout: sake.wasm, the page (web/dist), and the ippai binary
# with the page built in, then packs them into $DIST/ippai-<version>.tar.zst (+ .sha256) for the
# corpus runs (tests/corpus/ci.sh: they serve web/ from it). The releases are
# .github/workflows/release.yml's.
# Used by .github/workflows/ci.yml and runnable anywhere with the same tools:
#   apt: mingw-w64 zstd; rustup (rust-src,
#   wasm32-unknown-emscripten); node 26+. Emscripten is downloaded into $EMSDK4.
#
#   tools/ci/build.sh [version]          (default: git describe)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=${1:-$(git -C "$ROOT" describe --tags --always)}
DIST=${DIST:-$ROOT/target/dist}
export OUT=${OUT:-$ROOT/target/sake-out}
. "$ROOT/tools/ci/emsdk.sh"

cd "$ROOT"
rm -rf "$DIST"  # (a cached target/ would carry older builds into the artifact)
WEB_OUT=$ROOT/web/public/sake bash crates/sake/build.sh
bash host/tests/bench-wasm.sh  # (web/public/benchwasm: bench.html's direct-wasm numbers)
(cd web && pnpm install --frozen-lockfile && pnpm exec vp build)
cargo build --release -p ippai

mkdir -p "$DIST"
STAGE=$(mktemp -d)
P=$STAGE/ippai-$VERSION
mkdir -p "$P"
cp -r web/dist "$P/web"
cp target/release/ippai "$P/"
cp "$EMSDK4/upstream/bin/wasm-opt" "$P/"  # (the server optimizes saved translations with it)
# (sake for node: the corpus runs setup programs in it before the game, tests/corpus/install.py)
mkdir -p "$P/sake-node" && cp "$OUT/sake.js" "$OUT/sake.wasm" "$P/sake-node/"
echo "$VERSION $(git rev-parse HEAD)" > "$P/VERSION"
tar -C "$STAGE" -cf - "ippai-$VERSION" | zstd -19 -T0 -q -o "$DIST/ippai-$VERSION.tar.zst" -f
(cd "$DIST" && sha256sum "ippai-$VERSION.tar.zst" > "ippai-$VERSION.tar.zst.sha256")
rm -rf "$STAGE"
echo "built $DIST/ippai-$VERSION.tar.zst"
