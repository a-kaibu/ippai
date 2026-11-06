#!/bin/bash
# Fetches the engine of the Ren'Py benchmark's game (testdata/renpy, packed by
# tests/bench/pack-renpy.py) into DIR/renpy-sdk, once: Ren'Py 8.5.3's SDK (Windows x64 runtime,
# the engine's Python, and the Linux runtime that compiles the scripts), pinned by its hash.
# (Ren'Py is MIT with LGPL libraries: games ship it; LICENSE.txt goes with the game.)
#
#   tests/bench/fetch-renpy.sh DIR
set -euo pipefail
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
RENPY_URL=https://www.renpy.org/dl/8.5.3/renpy-8.5.3-sdk.zip
RENPY_SHA=ff57648f9c04f27e381c48af6d8e3ee3cdec296bed4d3831f47f09b0a71b505e
get() {  # <url> <file>: with retries
  for i in 1 2 3 4; do curl -fsSL --retry 3 -o "$2" "$1" && return; sleep $((i * 2)); done
  return 1
}
if [ ! -f "$DIR/renpy-sdk/renpy.py" ]; then
  get "$RENPY_URL" "$DIR/renpy.zip"
  echo "$RENPY_SHA  $DIR/renpy.zip" | sha256sum -c --quiet
  rm -rf "$DIR/x" && mkdir "$DIR/x"
  python3 -I -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$DIR/renpy.zip" "$DIR/x"
  chmod +x "$DIR/x/renpy-8.5.3-sdk/renpy.sh" "$DIR"/x/renpy-8.5.3-sdk/lib/py3-linux-x86_64/*
  rm -rf "$DIR/renpy-sdk" && mv "$DIR/x/renpy-8.5.3-sdk" "$DIR/renpy-sdk" && rm -rf "$DIR/x" "$DIR/renpy.zip"
fi
