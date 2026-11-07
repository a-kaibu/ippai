#!/bin/bash
# Parts of cache keys, into $GITHUB_ENV: CI_IMAGE (the runner image: apt packages are cached per
# image, so a new image's libraries are never mixed with an old image's packages) and CI_WEEK
# (nightly toolchains are downloaded again once a week, not every run).
set -euo pipefail
{
  echo "CI_IMAGE=${ImageOS:-local}-${ImageVersion:-0}"
  echo "CI_WEEK=$(date -u +%G-%V)"
} >> "${GITHUB_ENV:-/dev/stdout}"
