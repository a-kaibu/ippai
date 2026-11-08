#!/bin/bash
# apitrace's DirectDraw wrapper (ddraw.dll), built from its sources with
# tests/replay/apitrace-ddraw.patch: after a surface's ReleaseDC the wrapper locks and unlocks the
# surface through itself, so the trace holds what GDI drew there (a Lock, the fake memcpy, an
# Unlock); apitrace's own build records nothing at ReleaseDC, and a replay of the calls alone
# would miss the text and pictures a game drew with GDI (docs/replay.md).
#   ddraw-wrapper.sh <out dir>     -> <out>/win32/ddraw.dll, <out>/win64/ddraw.dll
# (mingw-w64's g++, cmake, ninja; apitrace and its DirectX SDK headers at fixed commits)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: ddraw-wrapper.sh <out dir>}
APITRACE_REV=18cfd388ac47a9e00978b34d6c20341f3e2cdb56   # apitrace 14.0
DXSDK_REV=d964b66467aaa734edbc24326da8119f5f063dd3
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
fetch() {  # <repo> <rev> <dir>
  git init -q "$3"
  git -C "$3" fetch -q --depth 1 "https://github.com/$1" "$2"
  git -C "$3" checkout -q FETCH_HEAD
}
fetch apitrace/apitrace $APITRACE_REV "$W/apitrace"
git -C "$W/apitrace" submodule -q update --init --depth 1
fetch apitrace/dxsdk $DXSDK_REV "$W/apitrace/dxsdk-master"
git -C "$W/apitrace" apply "$HERE/apitrace-ddraw.patch"
for t in mingw32 mingw64; do
  (cd "$W/apitrace" && cmake -S . -B "build/$t" -G Ninja -DCMAKE_TOOLCHAIN_FILE="$PWD/cmake/toolchain/$t.cmake" \
     -C "dxsdk-master/Cache-$t.cmake" -DCMAKE_BUILD_TYPE=Release -DENABLE_GUI=NO > "$W/$t.log" \
   && cmake --build "build/$t" --target ddrawtrace >> "$W/$t.log") || { tail -30 "$W/$t.log"; exit 1; }
  d=$OUT/win${t#mingw}
  mkdir -p "$d"
  cp "$W/apitrace/build/$t/wrappers/ddraw.dll" "$d/"
done
ls -l "$OUT"/win*/ddraw.dll
