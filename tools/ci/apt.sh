#!/bin/bash
# apt-get install through ~/.apt-cache, which actions/cache restores (key: CI_IMAGE from
# tools/ci/cache-keys.sh and the package list). On a hit the cached .debs are installed as they
# are: no apt-get update, nothing downloaded. A miss (or an incomplete cache) downloads them.
#   tools/ci/apt.sh package...   (i386 packages: dpkg --add-architecture i386 first)
set -euo pipefail
C=$HOME/.apt-cache
export DEBIAN_FRONTEND=noninteractive
have() {
  local p
  for p in "$@"; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -qx 'install ok installed' || return 1
  done
}
if have "$@"; then
  echo "apt: already installed: $*"
  exit 0
fi
shopt -s nullglob
debs=("$C"/*.deb)
if [ ${#debs[@]} -gt 0 ] && sudo -E apt-get install -yq --no-install-recommends --no-download "${debs[@]}" && have "$@"; then
  echo "apt: ${#debs[@]} cached packages"
  exit 0
fi
echo "apt: downloading $*"
rm -rf "$C" && mkdir -p "$C/partial"
o=(-o Acquire::Retries=5 -o Acquire::http::Timeout=30 -o Dir::Cache::Archives="$C")
sudo -E apt-get "${o[@]}" update -q
sudo -E apt-get "${o[@]}" install -yq --no-install-recommends --download-only "$@"
sudo -E apt-get "${o[@]}" install -yq --no-install-recommends "$@"
sudo rm -rf "$C/partial" "$C/lock"
sudo chown -R "$(id -u):$(id -g)" "$C"
