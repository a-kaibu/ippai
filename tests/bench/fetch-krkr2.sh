#!/bin/bash
# Fetches the engine of the 吉里吉里2 benchmark (the KiriKiri game of testdata/kirikiri, packed by
# tests/bench/pack-kirikiri.mjs) into DIR/krkr2, once: 吉里吉里2 2.32r2 (krkr.eXe, plugin/) from
# Vector, pinned by its hash. (GPL 2 / its own license: redistribution allowed; license.txt goes
# with the game.)
#
#   tests/bench/fetch-krkr2.sh DIR      (DIR/krkr2/kirikiri2/krkr.eXe)
set -euo pipefail
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
KRKR2_URL=https://ftp.vector.co.jp/62/92/4005/kr2_232r2.zip
KRKR2_SHA=645a6dc0898e41e20603e134ca6ff6a22994e66a9ed9049c059c83ac91b8fa2d
get() {  # <url> <file>: with retries
  for i in 1 2 3 4; do curl -fsSL --retry 3 -o "$2" "$1" && return; sleep $((i * 2)); done
  return 1
}
if [ ! -f "$DIR/krkr2/kirikiri2/krkr.eXe" ]; then
  get "$KRKR2_URL" "$DIR/krkr2.zip"
  echo "$KRKR2_SHA  $DIR/krkr2.zip" | sha256sum -c --quiet
  rm -rf "$DIR/x" && mkdir "$DIR/x"
  python3 -I -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$DIR/krkr2.zip" "$DIR/x"
  rm -rf "$DIR/krkr2" && mv "$DIR/x/kr2_232r2" "$DIR/krkr2" && rm -rf "$DIR/x" "$DIR/krkr2.zip"
fi
