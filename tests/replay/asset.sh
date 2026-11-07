#!/bin/sh
# Which apitrace release asset to download for a build (win32 / win64): reads the release's asset
# names on stdin, prints the one name, and fails unless exactly one matches (14.0 has win64 and
# win64-arm: a "*win64*" pattern took both, and no apitrace.exe was found for the 64-bit probes).
a=$1
[ -n "$a" ] || { echo "usage: asset.sh win32|win64 < names" >&2; exit 2; }
m=$(grep -E "^apitrace-[0-9][0-9.]*-$a\.7z\$")
n=$(printf '%s' "$m" | grep -c .)
[ "$n" = 1 ] || { echo "asset.sh: $n assets for $a" >&2; exit 1; }
printf '%s\n' "$m"
