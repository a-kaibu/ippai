#!/bin/bash
# Installs a release (vnvm-<version>.tar.zst from GitHub Releases) into a server directory
# that holds web/, vnvm-server, vnvm-pack (and the store, jit cache, logs, untouched), then
# restarts the server with that directory's run-server.sh. The previous files stay as
# web.prev, vnvm-server.prev, ...; `deploy.sh --rollback` puts them back.
#
#   tools/ci/deploy.sh vnvm-v0.1.0.tar.zst [dir]      (dir: default /home/kota/vnvm/vnvm-bench)
#   tools/ci/deploy.sh --rollback [dir]
#
# run-server.sh in dir starts the server in the background (its command line: --web web
# --store store --jit ... as that machine runs it).
set -euo pipefail
if [ "${1:-}" = "--rollback" ]; then
  DIR=${2:-/home/kota/vnvm/vnvm-bench}
  cd "$DIR"
  for f in web vnvm-server vnvm-pack wasm-opt VERSION; do
    [ -e "$f.prev" ] && rm -rf "$f" && mv "$f.prev" "$f"
  done
else
  TAR=$(realpath "$1")
  DIR=${2:-/home/kota/vnvm/vnvm-bench}
  [ -f "$TAR.sha256" ] && (cd "$(dirname "$TAR")" && sha256sum -c --quiet "$(basename "$TAR").sha256")
  cd "$DIR"
  NEW=$(mktemp -d "$DIR/.new.XXXX")
  tar -I zstd -xf "$TAR" -C "$NEW"
  SRC=$(echo "$NEW"/vnvm-*)
  for f in web vnvm-server vnvm-pack wasm-opt VERSION; do
    [ -e "$SRC/$f" ] || continue
    rm -rf "$f.prev"
    [ -e "$f" ] && mv "$f" "$f.prev"
    mv "$SRC/$f" "$f"
  done
  rm -rf "$NEW"
fi
pkill -x vnvm-server || true
for _ in $(seq 50); do pgrep -x vnvm-server >/dev/null || break; sleep 0.1; done
./run-server.sh
sleep 1
pgrep -x vnvm-server >/dev/null && echo "running $(cat VERSION 2>/dev/null)" || { echo "the server did not start"; exit 1; }
