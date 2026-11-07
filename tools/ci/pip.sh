#!/bin/bash
# pip install --user from the wheels in ~/.pip-wheels (actions/cache, key: the pinned packages):
# PyPI is asked only when a wheel is missing.
#   tools/ci/pip.sh package==version...
set -euo pipefail
W=$HOME/.pip-wheels
mkdir -p "$W"
if ! pip install --user -q --no-index --find-links "$W" "$@" 2>/dev/null; then
  pip download -q -d "$W" "$@"
  pip install --user -q --no-index --find-links "$W" "$@"
fi
