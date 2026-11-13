#!/bin/sh
# build-msvc64.sh NAME OUT: an x86-64 (MSVC_ARCH=i686: x86) MSVC-ABI program from host/tests/NAME.cpp (clang +
# lld-link, no SDK), importing kernel32, msvcp140, vcruntime140 and ucrtbase like a /MD build:
# the import libraries are made from the object's undefined symbols (msvcp140: the _Mtx_ /
# _Cnd_ / _Thrd_ / _Query_perf_ / _Xtime_ functions and std:: ones).
# A source with a "msstl" line is built against Microsoft's STL headers (a vs-2022-17.11
# checkout in $MSSTL, fetched once) with mingw-w64's C headers and msstl-shim/ for the
# vcruntime ones, plus msstl-shim/glue.cpp (operator new/delete, _fltused); x86 programs get
# msstl-shim/x86div.c (_aulldiv...).
# x86: C names carry an underscore (stripped for the import libraries; stdcall ones keep @N for
# llvm-dlltool -k), member functions are thiscall.
set -eu
arch=${MSVC_ARCH:-x86_64}
case $arch in
  x86_64) machine=i386:x86-64 ;;
  i686) machine=i386 ;;
  *) echo "MSVC_ARCH: x86_64 or i686" >&2; exit 2 ;;
esac
name=$1
out=${2:-.}
src=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
cxx="clang++ --target=$arch-pc-windows-msvc -O1 -fms-extensions -fexceptions -fcxx-exceptions -fno-builtin -fno-stack-protector"
objs="$t/$name.obj"
# (an "eha" line: /EHa, structured exceptions reach catch blocks)
grep -q '^// eha' "$src/$name.cpp" && cxx="$cxx -fasync-exceptions"
if grep -q '^// msstl' "$src/$name.cpp"; then
  MSSTL=${MSSTL:-$HOME/.cache/msstl-17.11}
  mkdir -p "$(dirname "$MSSTL")"
  # (CI runs two host/tests suites at once: one clones it, the other waits and finds it there;
  # an rm -rf beside a clone used to leave "Directory not empty")
  if ! (
    if command -v flock >/dev/null; then flock 9; fi
    if [ ! -f "$MSSTL/stl/inc/iostream" ]; then
      rm -rf "$MSSTL"
      git clone -q --depth 1 --branch vs-2022-17.11 --filter=blob:none --sparse https://github.com/microsoft/STL.git "$MSSTL"
      git -C "$MSSTL" sparse-checkout set stl/inc
    fi
  ) 9>"$MSSTL.lock"; then exit 1; fi
  cxx="$cxx -std=c++17 -fms-compatibility -fms-compatibility-version=19.40 -fno-rtti -fno-threadsafe-statics
    -D_DLL -D_MT -D_HAS_STATIC_RTTI=0 -isystem $src/msstl-shim -isystem $MSSTL/stl/inc
    -isystem $(clang -print-resource-dir)/include -isystem /usr/$arch-w64-mingw32/include -include $src/msstl-shim/pre.h"
  # (mingw-w64's headers tell x86 by _X86_, which clang's MSVC target leaves out)
  [ "$arch" = i686 ] && cxx="$cxx -D_X86_"
  $cxx -c "$src/msstl-shim/glue.cpp" -o "$t/glue.obj"
  objs="$objs $t/glue.obj"
fi
$cxx -c "$src/$name.cpp" -o "$t/$name.obj"
# (x86: the 64-bit division helpers msvcrt.lib links into every program)
if [ "$arch" = i686 ]; then
  clang --target=i686-pc-windows-msvc -O1 -fno-builtin -c "$src/msstl-shim/x86div.c" -o "$t/x86div.obj"
  objs="$objs $t/x86div.obj"
fi
# (where Windows' DLLs export them: vcruntime140 has the exception copies and the mem/str
# search functions; printf, the STL's vector algorithms, _Facet_Register and type_info's
# vftable are no DLL's: glue.cpp has them, as msvcrt.lib / msvcprt.lib do)
vcrt='__intrinsic_setjmp __intrinsic_setjmpex _setjmp3 _except_handler3 _except_handler4_common longjmp _CxxThrowException __CxxFrameHandler3 __CxxFrameHandler4 __C_specific_handler memset memcpy memmove memcmp memchr strchr strrchr strstr wcschr wcsrchr wcsstr __std_type_info_compare __std_terminate __std_exception_copy __std_exception_destroy __RTDynamicCast __RTCastToVoid __RTtypeid _set_se_translator'
defined=$(llvm-nm --defined-only $objs | awk '{print $NF}' | sort -u)
for d in ucrtbase vcruntime140 kernel32 msvcp140; do printf 'LIBRARY %s.dll\nEXPORTS\n' "$d" > "$t/$d.def"; done
llvm-nm -u $objs | awk '{print $NF}' | sort -u | while read -r s; do
  b=${s#__imp_}
  # (defined in one of the objects: glue.cpp's)
  case "
$defined
" in *"
$b
"*) continue;; esac
  [ "$arch" = i686 ] && case "$b" in \?*) ;; *) b=${b#_} ;; esac
  case "$b" in __chkstk|_chkstk|_alloca_probe|_fltused|_purecall|"??2@"*|"??3@"*|"??_U@"*|"??_V@"*) continue;; esac
  case " $vcrt " in *" $b "*) echo "\"$b\"" >> "$t/vcruntime140.def"; continue;; esac
  case "$b" in
    _Mtx_*|_Cnd_*|_Thrd_*|_Query_perf_*|_Xtime_*|*@std@@*|*@Concurrency@@*|_Getctype|_Getcvt|_Getcoll|_Getwctype|_Getwctypes|_Tolower|_Toupper|_Towlower|_Towupper|_Strcoll|_Strxfrm|_Wcscoll|_Wcsxfrm|_Mbrtowc|_Wcrtomb|_Stod|_Stof|_Stold|_Getdays|_Getmonths|_Gettnames|_Strftime|_W_Get*|_Getdateorder|_Cosh|_Sinh|_Exp|_Dtest|_FDtest|_LDtest)
      echo "\"$b\"" >> "$t/msvcp140.def" ;;
    [A-Z]*) echo "$b" >> "$t/kernel32.def" ;;
    *) echo "$b" >> "$t/ucrtbase.def" ;;
  esac
done
libs=
for d in ucrtbase vcruntime140 kernel32 msvcp140; do
  # (a DLL nothing is imported from: no library)
  [ "$(wc -l < "$t/$d.def")" -gt 2 ] || continue
  llvm-dlltool -m $machine -k -d "$t/$d.def" -l "$t/$d.lib"
  libs="$libs $t/$d.lib"
done
safeseh=; [ "$arch" = i686 ] && safeseh=/safeseh:no
lld-link /nologo /subsystem:console /entry:main /nodefaultlib $safeseh /out:"$out/$name.exe" $objs $libs
