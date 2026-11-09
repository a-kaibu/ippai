#!/bin/sh
# The game database rebuilt and published by hand, with the GitHub CLI's sign-in for the token
# (the user, 2026-11-09: "ghコマンド使う版でupdateするスクリプト"). Run it on a host ErogameScape
# answers (worker2, a home PC; Git Bash on Windows): tools/gamedb/README.md.
#
#     tools/gamedb/update.sh [work dir]      (default ~/.cache/ippai/gamedb)
#
# ippai: $IPPAI, else `ippai` on PATH when it has `gamedb update`, else the latest release's.
set -eu

repo=a-kaibu/ippai
dir=${1:-$HOME/.cache/ippai/gamedb}

command -v gh >/dev/null 2>&1 || { echo "gh (GitHub CLI) が要ります: https://cli.github.com/" >&2; exit 1; }
gh auth status -h github.com >/dev/null 2>&1 || gh auth login -h github.com -s write:packages
# (the push needs write:packages; a token without it is refused by ghcr.io)
if ! gh auth status -h github.com 2>&1 | grep -q "write:packages"; then
    gh auth refresh -h github.com -s write:packages
fi

has_update() { "$1" gamedb --help 2>/dev/null | grep -q "update"; }

ippai=${IPPAI:-}
if [ -z "$ippai" ] && command -v ippai >/dev/null 2>&1 && has_update ippai; then
    ippai=ippai
fi
if [ -z "$ippai" ]; then
    case "$(uname -s)" in
        Linux) os=linux ext=tar.gz ;;
        MINGW* | MSYS* | CYGWIN*) os=windows ext=zip ;;
        *) echo "$(uname -s): ippai のリリースがありません (IPPAI=<ippai> で指定してください)" >&2; exit 1 ;;
    esac
    case "$(uname -m)" in
        x86_64 | amd64) arch=amd64 ;;
        aarch64 | arm64) arch=arm64 ;;
        *) echo "$(uname -m): ippai のリリースがありません" >&2; exit 1 ;;
    esac
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    gh release download -R "$repo" -p "ippai_*_${os}_${arch}.${ext}" -D "$tmp"
    if [ "$ext" = zip ]; then
        (cd "$tmp" && unzip -q ippai_*.zip)
        ippai=$tmp/ippai.exe
    else
        tar -xzf "$tmp"/ippai_*.tar.gz -C "$tmp"
        ippai=$tmp/ippai
    fi
    has_update "$ippai" || { echo "最新リリースに gamedb update がまだありません (v0.3.3 以降が要ります)" >&2; exit 1; }
fi

echo "ippai: $ippai ($("$ippai" --version 2>/dev/null || echo '?'))"
GHCR_TOKEN=$(gh auth token -h github.com)
export GHCR_TOKEN
"$ippai" gamedb update --egs "$dir"
