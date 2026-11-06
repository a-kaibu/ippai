#!/bin/sh
# Builds the graphics checks (each file's "Build:" line) into $1 (default /home/user/work/gfxtest),
# which tests/gfx.mjs runs after `ippai pack $1 gfxtest`.
set -e
cd "$(dirname "$0")"
out=${1:-/home/user/work/gfxtest}
mkdir -p "$out"
for t in ddrawtest d3d8test d3d11test d3d10test d2dtest gltest nullcall; do
  cmd=$(sed -n 's|^ \* Build: \(.*\) \*/$|\1|p' $t.c)
  eval "$(echo "$cmd" | sed "s| -o $t.exe | -o $out/$t.exe |")"
done
