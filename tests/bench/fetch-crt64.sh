#!/bin/bash
# Fetches the 64-bit C runtimes the benchmark's x64 programs link (sake has no 64-bit CRT of its
# own yet) into DIR/crt64, once: ucrtbase.dll (Ren'Py, MSVC programs) and msvcrt.dll (mingw-w64
# programs), from Ubuntu 24.04's Wine 9.0 package (LGPL), pinned by its hash. The games ship them
# app-local, next to the exe.
#
#   tests/bench/fetch-crt64.sh DIR
set -euo pipefail
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
URL=http://archive.ubuntu.com/ubuntu/pool/universe/w/wine/libwine_9.0~repack-4build3_amd64.deb
SHA=eb421a3660bfdc090d70ce16b06aadfde1c2bce2593292023e520988f45d009e
[ -f "$DIR/crt64/ucrtbase.dll" ] && [ -f "$DIR/crt64/msvcrt.dll" ] && exit 0
for i in 1 2 3 4; do curl -fsSL --retry 3 -o "$DIR/libwine.deb" "$URL" && break; sleep $((i * 2)); done
echo "$SHA  $DIR/libwine.deb" | sha256sum -c --quiet
rm -rf "$DIR/x" && mkdir "$DIR/x"
W=./usr/lib/x86_64-linux-gnu/wine/x86_64-windows
dpkg-deb --fsys-tarfile "$DIR/libwine.deb" | tar -C "$DIR/x" -x "$W/ucrtbase.dll" "$W/msvcrt.dll"
mkdir -p "$DIR/crt64" && mv "$DIR/x/$W/ucrtbase.dll" "$DIR/x/$W/msvcrt.dll" "$DIR/crt64/"
rm -rf "$DIR/x" "$DIR/libwine.deb"
