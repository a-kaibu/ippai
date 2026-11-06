#!/bin/bash
# Fetches the engine of the KiriKiri benchmark's game (testdata/kirikiri, packed by
# tests/bench/pack-kirikiri.mjs) into DIR/krkrz, once: 吉里吉里Z 1.4.0r2's release (tvpwin32.exe,
# plugin/), pinned by its hash. (Its license, modified BSD, allows redistribution with the
# notice: license.txt goes with the game.)
#
#   tests/bench/fetch-krkrz.sh DIR
set -euo pipefail
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
KRKRZ_URL=https://github.com/krkrz/krkrz/releases/download/1.4.0r2/krkrz_20171225r2.7z
KRKRZ_SHA=a58da24a2e14eac102f0fd8bf66106b22ad238d17a317b8d44b65d68efb574df
get() {  # <url> <file>: with retries
  for i in 1 2 3 4; do curl -fsSL --retry 3 -o "$2" "$1" && return; sleep $((i * 2)); done
  return 1
}
if [ ! -f "$DIR/krkrz/tvpwin32.exe" ]; then
  get "$KRKRZ_URL" "$DIR/krkrz.7z"
  echo "$KRKRZ_SHA  $DIR/krkrz.7z" | sha256sum -c --quiet
  rm -rf "$DIR/x" && mkdir "$DIR/x"
  if command -v 7z >/dev/null; then 7z x -o"$DIR/x" "$DIR/krkrz.7z" >/dev/null
  else python3 -I -c "import py7zr,sys; py7zr.SevenZipFile(sys.argv[1]).extractall(sys.argv[2])" "$DIR/krkrz.7z" "$DIR/x"; fi
  rm -rf "$DIR/krkrz" && mv "$DIR/x/krkrz_20171225" "$DIR/krkrz" && rm -rf "$DIR/x" "$DIR/krkrz.7z"
fi
