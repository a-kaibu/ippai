#!/bin/bash
# Fails when npm or npx is used anywhere in the repository's scripts and workflows, or a
# package-lock.json is committed: this repository uses pnpm (pnpm install --frozen-lockfile,
# pnpm exec, pnpm add --dir for tools). npm is slower and keeps its own lock file.
set -uo pipefail
cd "$(dirname "$0")/../.."
bad=0
if git ls-files | grep -E '(^|/)(package-lock\.json|npm-shrinkwrap\.json)$'; then bad=1; fi
# (the words npm / npx used as commands; this file and lock files aside)
if git grep -nE '(^|[^[:alnum:]_./-])(npm|npx)([[:space:]]|$)' -- ':!tools/ci/no-npm.sh' ':!*.lock' ':!*lock.yaml' ':!*.md'; then bad=1; fi
[ $bad = 0 ] || { echo "no-npm: use pnpm (see tools/ci/no-npm.sh)"; exit 1; }
echo "no-npm: ok"
