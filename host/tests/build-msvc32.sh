#!/bin/sh
# build-msvc32.sh NAME OUT: an x86 MSVC-ABI program from host/tests/NAME.cpp (clang + lld-link,
# no SDK) on msvcrt.dll and kernel32, as VC6-era programs link it: the import libraries are made
# from the object's undefined symbols. An "eha" line in the source: /EHa.
set -eu
name=$1
out=${2:-.}
src=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
cxx="clang++ --target=i686-pc-windows-msvc -O1 -fms-extensions -fexceptions -fcxx-exceptions -fno-builtin -fno-stack-protector"
grep -q '^// eha' "$src/$name.cpp" && cxx="$cxx -fasync-exceptions"
$cxx -c "$src/$name.cpp" -o "$t/$name.obj"
for d in msvcrt kernel32; do printf 'LIBRARY %s.dll\nEXPORTS\n' "$d" > "$t/$d.def"; done
llvm-nm -u "$t/$name.obj" | awk '{print $NF}' | sort -u | while read -r s; do
  b=${s#__imp_}
  case "$b" in __chkstk|__fltused|"??2@"*|"??3@"*) continue;; esac
  # (C names carry an underscore; C++ ones do not)
  case "$b" in \?*) ;; *) b=${b#_} ;; esac
  case "$b" in
    [A-Z]*) echo "$b" >> "$t/kernel32.def" ;;
    *) echo "$b" >> "$t/msvcrt.def" ;;
  esac
done
for d in msvcrt kernel32; do llvm-dlltool -m i386 -k -d "$t/$d.def" -l "$t/$d.lib"; done
lld-link /nologo /subsystem:console /entry:main /nodefaultlib /safeseh:no /out:"$out/$name.exe" "$t/$name.obj" "$t/msvcrt.lib" "$t/kernel32.lib"
