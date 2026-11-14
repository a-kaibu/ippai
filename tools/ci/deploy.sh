#!/bin/bash
# Installs a release binary (masu_<version>_linux_<arch>.tar.gz from GitHub Releases, or CI's
# masu-<version>.tar.zst) into a server directory and restarts the server with that
# directory's run-server.sh. The store, jit cache and logs there are left as they are. The
# previous binary stays as masu.prev; `deploy.sh --rollback` puts it back.
#
#   tools/ci/deploy.sh masu_0.1.0_linux_amd64.tar.gz [dir]   (dir: default /home/kota/vnvm/vnvm-bench)
#   tools/ci/deploy.sh --rollback [dir]
#
# run-server.sh starts the server, e.g. `./masu serve --store store --jit jit --logs logs
# --listen 127.0.0.1:8080` (masu serve goes to the background by itself and returns once it
# serves). The web app is built into the binary: no --web unless one is to be served instead.
set -euo pipefail
FILES="masu web wasm-opt VERSION"
if [ "${1:-}" = "--rollback" ]; then
  DIR=${2:-/home/kota/vnvm/vnvm-bench}
  cd "$DIR"
  for f in $FILES; do
    [ -e "$f.prev" ] && rm -rf "$f" && mv "$f.prev" "$f"
  done
else
  TAR=$(realpath "$1")
  DIR=${2:-/home/kota/vnvm/vnvm-bench}
  cd "$DIR"
  NEW=$(mktemp -d "$DIR/.new.XXXX")
  case "$TAR" in
    *.zst) tar -I zstd -xf "$TAR" -C "$NEW" ;;
    *) tar -xzf "$TAR" -C "$NEW" ;;
  esac
  SRC=$NEW
  [ -e "$SRC/masu" ] || SRC=$(echo "$NEW"/masu-*)
  for f in $FILES; do
    [ -e "$SRC/$f" ] || continue
    rm -rf "$f.prev"
    [ -e "$f" ] && mv "$f" "$f.prev"
    mv "$SRC/$f" "$f"
  done
  rm -rf "$NEW"
fi
./masu stop >/dev/null 2>&1 || true
pkill -f '(^|/)(vnvm|masu) serve' || true  # (older servers, and ones started without the socket)
gone() { ! pgrep -f '(^|/)(vnvm|masu) serve' >/dev/null; }
for _ in $(seq 50); do gone && break; sleep 0.1; done
# (a server still flushing after 5 s: 25 s more, then killed; the new one never starts beside it on
# the same store and address)
for _ in $(seq 250); do gone && break; sleep 0.1; done
if ! gone; then
  echo "the old server is still running: killed"
  pkill -9 -f '(^|/)(vnvm|masu) serve' || true
  for _ in $(seq 50); do gone && break; sleep 0.1; done
  gone || { echo "the old server did not go"; exit 1; }
fi
grep -q -- '--web' run-server.sh && echo "note: run-server.sh passes --web: the built-in web app is not used"
./run-server.sh
sleep 1
pgrep -f 'masu serve' >/dev/null && echo "running $(./masu --version)" || { echo "the server did not start"; exit 1; }
