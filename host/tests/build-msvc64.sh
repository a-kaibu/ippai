#!/bin/sh
# build-msvc64.sh NAME OUT: an x86-64 MSVC-ABI program from host/tests/NAME.cpp (clang +
# lld-link, no SDK), importing kernel32, msvcp140, vcruntime140 and ucrtbase like a /MD build:
# the import libraries are made from the object's undefined symbols (msvcp140: the _Mtx_ /
# _Cnd_ / _Thrd_ / _Query_perf_ / _Xtime_ functions and std:: ones).
set -eu
name=$1
out=${2:-.}
src=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
clang++ --target=x86_64-pc-windows-msvc -O1 -fms-extensions -fexceptions -fcxx-exceptions \
  -fno-builtin -fno-stack-protector -c "$src/$name.cpp" -o "$t/$name.obj"
vcrt='_CxxThrowException __CxxFrameHandler3 __CxxFrameHandler4 __C_specific_handler memset memcpy memmove memcmp __std_type_info_compare __std_terminate'
for d in ucrtbase vcruntime140 kernel32 msvcp140; do printf 'LIBRARY %s.dll\nEXPORTS\n' "$d" > "$t/$d.def"; done
llvm-nm -u "$t/$name.obj" | awk '{print $NF}' | sort -u | while read -r s; do
  b=${s#__imp_}
  case "$b" in __chkstk|_fltused) continue;; esac
  case " $vcrt " in *" $b "*) echo "$b" >> "$t/vcruntime140.def"; continue;; esac
  case "$b" in
    _Mtx_*|_Cnd_*|_Thrd_*|_Query_perf_*|_Xtime_*|*@std@@*) echo "\"$b\"" >> "$t/msvcp140.def" ;;
    [A-Z]*) echo "$b" >> "$t/kernel32.def" ;;
    *) echo "$b" >> "$t/ucrtbase.def" ;;
  esac
done
libs=
for d in ucrtbase vcruntime140 kernel32 msvcp140; do
  # (a DLL nothing is imported from: no library)
  [ "$(wc -l < "$t/$d.def")" -gt 2 ] || continue
  llvm-dlltool -m i386:x86-64 -d "$t/$d.def" -l "$t/$d.lib"
  libs="$libs $t/$d.lib"
done
lld-link /nologo /subsystem:console /entry:main /nodefaultlib /out:"$out/$name.exe" "$t/$name.obj" $libs
