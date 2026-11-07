#!/bin/sh
# build-msvc64.sh NAME OUT: an x86-64 MSVC-ABI program from host/tests/NAME.cpp (clang +
# lld-link, no SDK), importing kernel32, msvcp140, vcruntime140 and ucrtbase like a /MD build:
# the import libraries are made from the object's undefined symbols (msvcp140: the _Mtx_ /
# _Cnd_ / _Thrd_ / _Query_perf_ / _Xtime_ functions and std:: ones).
# A source with a "msstl" line is built against Microsoft's STL headers (a vs-2022-17.11
# checkout in $MSSTL, fetched once) with mingw-w64's C headers and msstl-shim/ for the
# vcruntime ones, plus msstl-shim/glue.cpp (operator new/delete, _fltused).
set -eu
name=$1
out=${2:-.}
src=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
cxx="clang++ --target=x86_64-pc-windows-msvc -O1 -fms-extensions -fexceptions -fcxx-exceptions -fno-builtin -fno-stack-protector"
objs="$t/$name.obj"
if grep -q '^// msstl' "$src/$name.cpp"; then
  MSSTL=${MSSTL:-$HOME/.cache/msstl-17.11}
  if [ ! -f "$MSSTL/stl/inc/iostream" ]; then
    rm -rf "$MSSTL"
    git clone -q --depth 1 --branch vs-2022-17.11 --filter=blob:none --sparse https://github.com/microsoft/STL.git "$MSSTL"
    git -C "$MSSTL" sparse-checkout set stl/inc
  fi
  cxx="$cxx -std=c++17 -fms-compatibility -fms-compatibility-version=19.40 -fno-rtti -fno-threadsafe-statics
    -D_DLL -D_MT -D_HAS_STATIC_RTTI=0 -isystem $src/msstl-shim -isystem $MSSTL/stl/inc
    -isystem $(clang -print-resource-dir)/include -isystem /usr/x86_64-w64-mingw32/include -include $src/msstl-shim/pre.h"
  $cxx -c "$src/msstl-shim/glue.cpp" -o "$t/glue.obj"
  objs="$objs $t/glue.obj"
fi
$cxx -c "$src/$name.cpp" -o "$t/$name.obj"
vcrt='_CxxThrowException __CxxFrameHandler3 __CxxFrameHandler4 __C_specific_handler memset memcpy memmove memcmp __std_type_info_compare __std_terminate'
for d in ucrtbase vcruntime140 kernel32 msvcp140; do printf 'LIBRARY %s.dll\nEXPORTS\n' "$d" > "$t/$d.def"; done
llvm-nm -u $objs | awk '{print $NF}' | sort -u | while read -r s; do
  b=${s#__imp_}
  case "$b" in __chkstk|_fltused|_purecall|"??2@"*|"??3@"*|"??_U@"*|"??_V@"*) continue;; esac
  case " $vcrt " in *" $b "*) echo "$b" >> "$t/vcruntime140.def"; continue;; esac
  case "$b" in
    _Mtx_*|_Cnd_*|_Thrd_*|_Query_perf_*|_Xtime_*|*@std@@*|_Getctype|_Getcvt|_Getcoll|_Getwctype|_Getwctypes|_Tolower|_Toupper|_Towlower|_Towupper|_Strcoll|_Strxfrm|_Wcscoll|_Wcsxfrm|_Mbrtowc|_Wcrtomb|_Stod|_Stof|_Stold|_Getdays|_Getmonths|_Gettnames|_Strftime|_W_Get*|_Getdateorder|_Cosh|_Sinh|_Exp|_Dtest|_FDtest|_LDtest)
      echo "\"$b\"" >> "$t/msvcp140.def" ;;
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
lld-link /nologo /subsystem:console /entry:main /nodefaultlib /out:"$out/$name.exe" $objs $libs
