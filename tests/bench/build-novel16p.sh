#!/bin/bash
# Builds the second Win16 benchmark game (testdata/novel16p/novel16p.pas) into DIR/novel16p.exe
# with Free Pascal's i8086-win16 cross compiler, built once into DIR/fpc from Free Pascal's sources
# at a pinned commit (the target is only in its development version) with the distribution's fpc
# (3.2.2) and Open Watcom's linker (build-novel16.sh's WATCOM_DIR/watcom). The RTL's license
# (modified LGPL) goes with the game.
#
#   tests/bench/build-novel16p.sh DIR WATCOM_DIR      (needs fpc, nasm, git, make)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
export WATCOM=$(cd "$2" && pwd)/watcom
REPO=https://gitlab.com/freepascal.org/fpc/source.git
COMMIT=cda6e50f26f319d22fc0458ec99ba8fc369cac73
FPC=$DIR/fpc/lib/fpc/3.3.1
export PATH=$WATCOM/binl64:$PATH
if [ ! -x "$FPC/ppcross8086" ]; then
  S=$(mktemp -d)
  trap 'rm -rf "$S"' EXIT
  # (a commit, fetched by its hash: git checks what it gets against it)
  git -C "$S" init -q
  for i in 1 2 3 4; do git -C "$S" fetch -q --depth 1 "$REPO" "$COMMIT" && break; sleep $((i * 2)); done
  git -C "$S" checkout -q FETCH_HEAD
  # (large model: the game's data is far; nasm and wlink as they are, no target prefix)
  make -C "$S" -j"$(nproc)" crossinstall OS_TARGET=win16 CPU_TARGET=i8086 BINUTILSPREFIX= \
    CROSSOPT="-WmLarge -CX -XXs -O2" INSTALL_PREFIX="$DIR/fpc" >"$DIR/fpc-build.log" 2>&1 ||
    { tail -30 "$DIR/fpc-build.log"; exit 1; }
  cp "$S/rtl/COPYING.FPC" "$DIR/COPYING.fpc"
fi
T=$(mktemp -d)
"$FPC/ppcross8086" -Twin16 -WmLarge -O2 -Sew -Fu"$FPC/units/win16/rtl" -FU"$T" -o"$DIR/novel16p.exe" \
  "$ROOT/testdata/novel16p/novel16p.pas" >"$T/log" 2>&1 || { cat "$T/log"; exit 1; }
rm -rf "$T"
