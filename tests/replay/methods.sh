#!/bin/bash
# The vtable index of every method of mingw-w64's d3d9.h, for d3d9replay.c: lines
#   {"IDirect3DDevice9::Clear", 43, 1},     (1: returns an HRESULT)
# in the order the header declares them (the C vtable order: each interface lists its base
# interfaces' methods first).
set -euo pipefail
H=${1:-$(echo '#include <d3d9.h>' | i686-w64-mingw32-gcc -E -M -x c - | tr ' \\' '\n\n' | grep '/d3d9\.h$' | head -1)}
awk '
/^DECLARE_INTERFACE_\(/ { s = $0; sub(/^DECLARE_INTERFACE_\(/, "", s); sub(/[ ,].*/, "", s); iface = s; n = 0; next }
iface != "" && /^};/ { iface = ""; next }
iface != "" && /STDMETHOD/ {
    s = $0
    if (s ~ /STDMETHOD_\(/) {
        t = s; sub(/.*STDMETHOD_\( */, "", t); sub(/ *,.*/, "", t); hr = (t == "HRESULT")
        sub(/.*STDMETHOD_\([^,]*, */, "", s)
    } else { hr = 1; sub(/.*STDMETHOD\( */, "", s) }
    sub(/ *\).*/, "", s)
    printf "{\"%s::%s\", %d, %d},\n", iface, s, n, hr
    n++
}' "$H"
