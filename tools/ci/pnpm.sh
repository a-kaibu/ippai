#!/bin/bash
# pnpm on a CI runner (this repository uses pnpm only, never npm: tools/ci/no-npm.sh): corepack's
# pnpm at the version web/package.json names (packageManager), the store at ~/.pnpm-store (the
# workflows cache it with ~/.cache/node/corepack).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
V=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["packageManager"].split("@")[1])' "$ROOT/web/package.json")
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
corepack enable pnpm 2>/dev/null || sudo corepack enable pnpm
corepack install -g "pnpm@$V"
pnpm config set --location=global store-dir "$HOME/.pnpm-store"
echo "COREPACK_ENABLE_DOWNLOAD_PROMPT=0" >> "${GITHUB_ENV:-/dev/null}"
pnpm --version
