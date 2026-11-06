#!/bin/bash
# Builds the Win16 benchmark's game (testdata/novel16/novel16.c, a 16-bit Windows 3.1 program)
# into DIR/novel16.exe with Open Watcom v2 (its 2024-05-02 build, pinned by its hash; the Sybase
# Open Watcom Public License), fetched into DIR/watcom once.
#
#   tests/bench/build-novel16.sh DIR
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
URL=https://github.com/open-watcom/open-watcom-v2/releases/download/2024-05-02-Build/ow-snapshot.tar.xz
SHA=d43e5064ff06ed32f3937cf20405566d4c52779661c6a975044ac130c02b4e3c
export WATCOM=$DIR/watcom
if [ ! -x "$WATCOM/binl64/wcl" ]; then
  for i in 1 2 3 4; do curl -fsSL --retry 3 -o "$DIR/ow.tar.xz" "$URL" && break; sleep $((i * 2)); done
  echo "$SHA  $DIR/ow.tar.xz" | sha256sum -c --quiet
  # (only what a 16-bit Windows program needs: the Linux tools, the headers, the 286 libraries)
  rm -rf "$WATCOM" && mkdir -p "$WATCOM"
  tar -C "$WATCOM" -xJf "$DIR/ow.tar.xz" binl64 h lib286 license.txt
  rm "$DIR/ow.tar.xz"
fi
export PATH=$WATCOM/binl64:$PATH INCLUDE=$WATCOM/h:$WATCOM/h/win
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
cd "$T"
# (large model; warnings are errors here too)
wcl -q -bt=windows -ml -ox -we -wx -l=windows -fe="$DIR/novel16.exe" "$ROOT/testdata/novel16/novel16.c" mmsystem.lib >/dev/null || \
  wcl -q -bt=windows -ml -ox -we -wx -l=windows -fe="$DIR/novel16.exe" "$ROOT/testdata/novel16/novel16.c" mmsystem.lib
cp "$WATCOM/license.txt" "$DIR/LICENSE.watcom"
