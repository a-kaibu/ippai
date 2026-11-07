#!/bin/bash
# GoReleaser's "cargo" in release.yml (.goreleaser.yaml builds.tool): the binaries are built
# before it, one runner per target (release.yml bin jobs: cargo zigbuild), and downloaded to
# target/prebuilt/<target>/; this puts the one GoReleaser asks for (`zigbuild --target=T ...`)
# where it looks: $CARGO_TARGET_DIR/<target>/release/ (GoReleaser OSS has no prebuilt builder).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
t=
for a in "$@"; do
  case $a in --target=*) t=${a#--target=} ;; esac
done
[ -n "$t" ] || { echo "prebuilt-cargo: no --target in: $*" >&2; exit 1; }
src=$ROOT/target/prebuilt/$t
dst=${CARGO_TARGET_DIR:-target}/$t/release
mkdir -p "$dst"
cp -v "$src"/* "$dst"/
