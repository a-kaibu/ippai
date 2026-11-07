#!/bin/sh
# asset.sh on apitrace 14.0's real asset list and on lists it must refuse.
set -e
cd "$(dirname "$0")"
l14='apitrace-14.0-Linux-arm64.tar.bz2
apitrace-14.0-Linux.tar.bz2
apitrace-14.0-win32.7z
apitrace-14.0-win64-arm.7z
apitrace-14.0-win64.7z'
fail=0
want() { got=$(printf '%s\n' "$2" | sh asset.sh "$1" 2>/dev/null) || got=FAIL
  [ "$got" = "$3" ] || { echo "asset.sh $1: got $got, want $3"; fail=1; }; }
want win32 "$l14" apitrace-14.0-win32.7z
want win64 "$l14" apitrace-14.0-win64.7z
want win64 'apitrace-14.0-win64-arm.7z' FAIL
want win64 "$(printf 'apitrace-14.0-win64.7z\napitrace-13.0-win64.7z')" FAIL
want win32 '' FAIL
want win64 'apitrace-14.0-win64.7z.sha256' FAIL
want '' "$l14" FAIL
[ $fail = 0 ] && echo "asset_test: ok"
exit $fail
