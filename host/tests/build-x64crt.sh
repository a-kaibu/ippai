#!/bin/sh
# Builds x64crt.exe (clang + lld-link, no MSVC SDK): import libraries are made from the
# object's undefined symbols, so the exe imports ucrtbase/vcruntime140/kernel32 like a
# /MD VC++ build does.
set -eu
out=${1:-.}
src=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
clang++ --target=x86_64-pc-windows-msvc -O1 -fms-extensions -fexceptions -fcxx-exceptions \
  -fno-builtin -fno-stack-protector -c "$src/x64crt.cpp" -o "$t/x64crt.obj"
vcrt='_CxxThrowException __CxxFrameHandler3 __CxxFrameHandler4 __C_specific_handler memset memcpy memmove memcmp __std_type_info_compare __std_terminate'
k32='ExitProcess RaiseException'
for d in ucrtbase vcruntime140 kernel32; do printf 'LIBRARY %s.dll\nEXPORTS\n' "$d" > "$t/$d.def"; done
llvm-nm -u "$t/x64crt.obj" | awk '{print $NF}' | sed 's/^__imp_//' | sort -u | while read -r s; do
  case " $k32 " in *" $s "*) echo "$s" >> "$t/kernel32.def"; continue;; esac
  case " $vcrt " in *" $s "*) echo "$s" >> "$t/vcruntime140.def"; continue;; esac
  case "$s" in __chkstk|_fltused) continue;; esac
  echo "$s" >> "$t/ucrtbase.def"
done
for d in ucrtbase vcruntime140 kernel32; do llvm-dlltool -m i386:x86-64 -d "$t/$d.def" -l "$t/$d.lib"; done
lld-link /nologo /subsystem:console /entry:main /nodefaultlib /out:"$out/x64crt.exe" \
  "$t/x64crt.obj" "$t/ucrtbase.lib" "$t/vcruntime140.lib" "$t/kernel32.lib"
