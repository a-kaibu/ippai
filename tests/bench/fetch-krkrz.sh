#!/bin/bash
# Fetches what the KiriKiri benchmark's game is made with (tests/bench/gen-kirikiri.mjs) into DIR,
# once: 吉里吉里Z's release (DIR/krkrz: tvpwin32.exe, plugin/; its license, modified BSD, allows
# redistribution with the notice: license.txt goes with the game) and KAG3 (DIR/kag3: krkrz/kag3's
# data/, "改変・配布は自由"). Both pinned.
#
#   tests/bench/fetch-krkrz.sh DIR
set -euo pipefail
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
KRKRZ_URL=https://github.com/krkrz/krkrz/releases/download/1.4.0r2/krkrz_20171225r2.7z
KRKRZ_SHA=a58da24a2e14eac102f0fd8bf66106b22ad238d17a317b8d44b65d68efb574df
KAG3_REV=1f3ab309106d210e3169bbbe0fb4e066ae463b42
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
if [ ! -f "$DIR/kag3/startup.tjs" ]; then
  get "https://codeload.github.com/krkrz/kag3/tar.gz/$KAG3_REV" "$DIR/kag3.tar.gz"
  rm -rf "$DIR/kag3" "$DIR/kag3-$KAG3_REV"
  tar -C "$DIR" -xzf "$DIR/kag3.tar.gz" && mv "$DIR/kag3-$KAG3_REV/data" "$DIR/kag3" && rm -rf "$DIR/kag3-$KAG3_REV" "$DIR/kag3.tar.gz"
fi
