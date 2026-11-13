#!/bin/sh
# Builds highmem.exe and its DLLs (highmem.c) into $1: all three at fixed bases above 4G,
# without relocations (clang + lld for x86-64 mingw: GNU ld keeps a DLL's .reloc).
set -eu
out=${1:-.}
src=$(cd "$(dirname "$0")" && pwd)
cc="clang --target=x86_64-w64-windows-gnu -fuse-ld=lld -O2"
fixed="-Wl,--disable-dynamicbase -Wl,--disable-reloc-section"
$cc -DHM_DLL1 -shared "$src/highmem.c" -o "$out/hm1.dll" -Wl,--out-implib,"$out/libhm1.a" -Wl,--image-base,0x320000000 $fixed
$cc -DHM_DLL2 -shared "$src/highmem.c" -o "$out/hm2.dll" -L"$out" -lhm1 -Wl,--image-base,0x7ff450000000 $fixed
$cc "$src/highmem.c" -o "$out/highmem.exe" -Wl,--image-base,0x140000000 $fixed
