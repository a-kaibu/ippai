#!/bin/bash
# Assemble the Wine install tree the wasm runtime runs from ($VNVM_WINE_ROOT, default
# /opt/wine-vnbox): vnbox's i386 PE files, plus the i386 builds of the programs vnbox (a wow64
# build) only ships for x86_64 (wineboot, services, ...), built from the same source.
#
# env: VNBOX_WINE (vnbox's installed Wine), WINE_SRC (patched Wine source), WINE_TOOLS,
#      PE_BUILD (build dir for the i386 programs), ROOT_OUT (the tree to create)
# VNVM_ARCH=x86_64: the tree for 64-bit programs (vnbox's x86_64 PE files, our DLLs built for
# x86_64; needs x86_64-w64-mingw32-gcc)
set -e
VNVM_ARCH=${VNVM_ARCH:-i386}
if [ "$VNVM_ARCH" = x86_64 ]; then
  SFX=64 MINGW=x86_64-w64-mingw32 DARCH="-D__x86_64__ -sMEMORY64=2" CONF_EXTRA="--enable-win64 ac_cv_mabi_ms=yes LDFLAGS=-sMEMORY64=2"
else
  SFX=32 MINGW=i686-w64-mingw32 DARCH=-D__i386__ CONF_EXTRA=
fi
VNBOX_WINE=${VNBOX_WINE:-/home/user/img/rootfs/opt/wine-vnbox}
WINE_SRC=${WINE_SRC:-/home/user/work/wine-src}
WINE_TOOLS=${WINE_TOOLS:-/home/user/img/buildroot/tmp/vnbox-build/wine-build-src/build}
PE_BUILD=${PE_BUILD:-/home/user/work/wine-pe$SFX}
ROOT_OUT=${ROOT_OUT:-/home/user/work/wineroot${SFX#32}}
EMSDK4=${EMSDK4:-/home/user/emsdk4}
. "$EMSDK4/emsdk_env.sh" >/dev/null 2>&1

# x86_64-only in vnbox; needs i686-w64-mingw32-gcc (the x86_64 tree has them)
PROGRAMS="conhost lsass plugplay rpcss services spoolsv termsv wineboot winemenubuilder wuauserv"
[ "$VNVM_ARCH" = x86_64 ] && PROGRAMS=
# ours (patches/wine-wasm.patch): <dir>[/<file>], the file defaulting to the dir's name
DLLS="winebrowser.drv vnd3d9/d3d9.dll vnd3d8/d3d8.dll vnd3d11/d3d11.dll vndxgi/dxgi.dll"
# (x86_64: no Direct3D 8, nor 9 on the host side: their games are 32-bit)
[ "$VNVM_ARCH" = x86_64 ] && DLLS="winebrowser.drv vnd3d9/d3d9.dll vnd3d11/d3d11.dll vndxgi/dxgi.dll"

mkdir -p "$PE_BUILD"
cd "$PE_BUILD"
if [ ! -f Makefile ] || [ "$WINE_SRC/configure" -nt Makefile ]; then
  emconfigure "$WINE_SRC/configure" --host=$VNVM_ARCH-pc-linux-gnu --build=x86_64-pc-linux-gnu $CONF_EXTRA \
    --with-wine-tools="$WINE_TOOLS" --prefix=/opt/wine-vnbox --disable-tests --without-x \
    --without-freetype --without-wayland --without-vulkan --without-opengl --without-pulse \
    --without-alsa --without-oss --without-gstreamer --without-ffmpeg --without-cups --without-dbus \
    --without-gnutls --without-krb5 --without-netapi --without-sdl --without-udev --without-usb \
    --without-v4l2 --without-capi --without-pcap --without-pcsclite --without-sane --without-gphoto \
    --without-inotify --without-fontconfig --without-xinput2 CFLAGS="-O2 $DARCH -pthread -fPIC" \
    > configure.log 2>&1
fi
make -j"$(nproc)" $(for p in $PROGRAMS; do echo programs/$p/$VNVM_ARCH-windows/$p.exe; done) \
  $(for d in $DLLS; do echo dlls/${d%%/*}/$VNVM_ARCH-windows/${d##*/}; done) > make.log 2>&1

PE=$ROOT_OUT/lib/wine/$VNVM_ARCH-windows
rm -rf "$PE" && mkdir -p "$PE" "$ROOT_OUT/lib/wine/$VNVM_ARCH-unix" "$ROOT_OUT/share"
for f in "$VNBOX_WINE"/lib/wine/$VNVM_ARCH-windows/*; do ln -s "$f" "$PE/"; done
for p in $PROGRAMS; do
  $MINGW-strip -o "$PE/$p.exe" "$PE_BUILD/programs/$p/$VNVM_ARCH-windows/$p.exe"
done
for d in $DLLS; do
  rm -f "$PE/${d##*/}"
  $MINGW-strip -o "$PE/${d##*/}" "$PE_BUILD/dlls/${d%%/*}/$VNVM_ARCH-windows/${d##*/}"
done
# ntdll looks for its own .so to find the install; the code itself is in wineunix.so
: > "$ROOT_OUT/lib/wine/$VNVM_ARCH-unix/ntdll.so"
ln -sfn "$VNBOX_WINE/share/wine" "$ROOT_OUT/share/wine"
echo "wine root in $ROOT_OUT"
