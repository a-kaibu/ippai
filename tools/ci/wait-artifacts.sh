#!/bin/bash
# Downloads artifacts of this workflow run as soon as another job of the run uploads them, so a
# job can start before that one ends (release.yml: bin builds ippai while web builds the page,
# and takes the page when it is there). Fails when the uploading job ended without them.
#   tools/ci/wait-artifacts.sh <job name> <artifact>=<dir>...
# (GH_TOKEN with actions: read; GITHUB_REPOSITORY and GITHUB_RUN_ID from Actions)
set -euo pipefail
job=$1
shift
api=repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID
names=()
for a in "$@"; do names+=("${a%%=*}"); done
start=$SECONDS
while :; do
  have=$(gh api "$api/artifacts?per_page=100" --jq '.artifacts[].name' || true)
  missing=0
  for n in "${names[@]}"; do grep -qxF "$n" <<<"$have" || missing=1; done
  [ "$missing" = 0 ] && break
  # (the uploading job's state: read after the artifacts, so one that uploaded and ended is seen
  # with them)
  state=$(gh api "$api/jobs?per_page=100" --jq ".jobs[] | select(.name == \"$job\") | .status + \" \" + (.conclusion // \"\")" || true)
  case $state in
    completed\ *)
      have=$(gh api "$api/artifacts?per_page=100" --jq '.artifacts[].name' || true)
      for n in "${names[@]}"; do
        grep -qxF "$n" <<<"$have" || { echo "job $job ended ($state) without artifact $n" >&2; exit 1; }
      done
      break ;;
  esac
  sleep 5
done
echo "artifacts after $((SECONDS - start)) s of waiting"
for a in "$@"; do
  gh run download "$GITHUB_RUN_ID" --repo "$GITHUB_REPOSITORY" -n "${a%%=*}" -D "${a#*=}"
done
