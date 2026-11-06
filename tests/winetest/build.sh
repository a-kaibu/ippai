#!/bin/bash
# Builds Wine's conformance tests (dlls/<dll>/tests) as Windows programs, the way Wine itself
# builds them (its own headers, import libraries and test.h, compiled with mingw-w64), from a
# pinned Wine release downloaded at build time. Nothing of Wine is kept in this repository
# (Wine is LGPL); only the list of tests and their expected results on sake are.
#
#   tests/winetest/build.sh [dll ...]     (default: every dll in tests/winetest/dlls.txt)
#   WINETEST=dir (default target/winetest): bin/<arch>/<dll>_test.exe, plus wine-<ver>/ and build/
#   ARCHS="i386 x86_64" (default both)
#
# Needs: mingw-w64, gcc, flex, bison, make, curl, xz. ~10 min for everything on 4 cores.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
VER=11.20
SHA256=eac8ca448f9c0087f963026b4dc276d1e2b7fbb64056edc093e59bbb7e56fe28
W=${WINETEST:-$HERE/../../target/winetest}
ARCHS=${ARCHS:-i386 x86_64}
mkdir -p "$W"
W=$(cd "$W" && pwd)
DLLS=${*:-$(grep -v '^#' "$HERE/dlls.txt" | awk 'NF {print $1}')}

if [ ! -f "$W/wine-$VER/configure" ]; then
  curl -fsSL -o "$W/wine-$VER.tar.xz" "https://dl.winehq.org/wine/source/${VER%%.*}.x/wine-$VER.tar.xz"
  echo "$SHA256  $W/wine-$VER.tar.xz" | sha256sum -c --quiet
  tar xJf "$W/wine-$VER.tar.xz" -C "$W"
  rm "$W/wine-$VER.tar.xz"
fi
B=$W/build
if [ ! -f "$B/Makefile" ]; then
  mkdir -p "$B"
  # (only the PE side and the build tools are used: no Unix libraries)
  (cd "$B" && "$W/wine-$VER/configure" --enable-archs="${ARCHS// /,}" --without-x --without-freetype \
    --without-gnutls --without-alsa --without-pulse --without-dbus --without-cups --without-sane \
    --without-gphoto --without-gstreamer --without-krb5 --without-netapi --without-opencl --without-pcap \
    --without-sdl --without-udev --without-usb --without-v4l2 --without-vulkan --without-wayland \
    --without-xcomposite --without-xinerama --without-fontconfig --without-capi --without-gettext \
    --without-pcsclite --without-ffmpeg --without-opengl --without-inotify --without-coreaudio \
    --without-oss > "$W/configure.log" 2>&1) || { tail -20 "$W/configure.log"; exit 1; }
fi
targets=()
for a in $ARCHS; do
  for d in $DLLS; do targets+=("dlls/$d/tests/$a-windows/${d%.*}_test.exe"); done
done
make -C "$B" -j"$(nproc)" -k "${targets[@]}" > "$W/make.log" 2>&1 || grep -E ': error:|\*\*\*' "$W/make.log" | head -20
for a in $ARCHS; do
  mkdir -p "$W/bin/$a"
  case $a in i386) s=i686-w64-mingw32-strip ;; *) s=$a-w64-mingw32-strip ;; esac
  for d in $DLLS; do
    t=$B/dlls/$d/tests/$a-windows/${d%.*}_test.exe
    if [ -f "$t" ]; then $s -o "$W/bin/$a/${d%.*}_test.exe" "$t"; else echo "not built: $a $d"; fi
  done
done
echo "built $(ls "$W"/bin/*/*.exe | wc -l) test programs in $W/bin"
