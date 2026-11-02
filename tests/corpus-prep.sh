#!/bin/bash
# Downloads, extracts and packs one vnbox corpus title for tests/corpus.mjs:
#   tests/corpus-prep.sh ID URL EXE_REL STORE
# -> TRIALS/ID/x (extracted), STORE (hard-linked copy of BASE_STORE + tree "g")
# prints the exe path inside the tree on success
set -e
id=$1 url=$2 exe=$3 store=$4
T=${TRIALS:-/home/user/trials}/$id
BASE=${BASE_STORE:-/home/user/work/base-store}
mkdir -p "$T"
cd "$T"
f=dl.${url##*.}
if [ ! -s "$f" ]; then
  # the proxy sometimes cuts long transfers: resume
  ok=
  for i in 1 2 3 4 5 6 7 8 9 10; do
    curl -sSfL --max-time 3600 -C - -o "$f.part" "$url" && { ok=1; break; }
    sleep $((i * 2))
  done
  [ -n "$ok" ] || { echo "download failed" >&2; exit 4; }
  mv "$f.part" "$f"
fi
if [ ! -d x ]; then
  rm -rf x.part; mkdir x.part
  case "${f,,}" in
    # (a file or two failing, e.g. a damaged Thumbs.db, still leaves a game)
    *.zip|*.lzh) (cd x.part && LC_ALL=C.UTF-8 unar -q -e cp932 -o . ../$f >/dev/null 2>&1 || [ -n "$(ls)" ]) ;;
    *.rar|*.7z) (cd x.part && LC_ALL=C.UTF-8 unar -q -o . ../$f >/dev/null 2>&1 || [ -n "$(ls)" ]) ;;
    *.exe)
      # self-extracting archive or Inno Setup installer
      (cd x.part && (LC_ALL=C.UTF-8 innoextract -s -q -e "../$f" 2>/dev/null && [ -n "$(ls)" ]) ||
         (LC_ALL=C.UTF-8 unar -q -e cp932 -o . "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (LC_ALL=C.UTF-8 7z x -y "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (LC_ALL=C.UTF-8 unrar x -y "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ])) ;;
  esac
  # names some extractors leave in Shift-JIS bytes -> UTF-8
  LC_ALL=C.UTF-8 convmv -r -f cp932 -t utf8 --notest --qfrom x.part >/dev/null 2>&1 || true
  # installers (vnbox ran the installed game): unpack InstallShield setups, Windows Installer
  # packages and Inno Setup installers here rather than run them in the browser (an installer
  # writing a 1 GB package into the in-memory file system runs out of wasm memory)
  find x.part -type f -iname '*.exe' -print0 | while IFS= read -r -d '' s; do
    if python3 -I "$(dirname "$0")/isextract.py" "$s" "$s.is" >/dev/null 2>&1; then rm -f "$s"
    elif LC_ALL=C.UTF-8 innoextract -s -q -e -d "$s.inno" "$s" >/dev/null 2>&1 && [ -n "$(ls "$s.inno")" ]; then rm -f "$s"
    else rm -rf "$s.is" "$s.inno"
    fi
  done
  # BGI install media: a 16-byte "0123456789ABCDEF" file next to BGIForInstalling.exe marks
  # the media, and the game run from there quits at once (the installed copy doesn't have it)
  find x.part -type f -iname BGIForInstalling.exe -print0 | while IFS= read -r -d '' b; do
    find "$(dirname "$b")" -maxdepth 1 -type f -size 16c -exec grep -lx -m1 0123456789ABCDEF {} + 2>/dev/null | xargs -r -d '\n' rm -f
  done
  find x.part -type f -iname '*.msi' -print0 | while IFS= read -r -d '' m; do
    # (msiextract can't do packages split over several cabinets: tests/msiunpack.py)
    if mkdir -p "$m.x" && { (cd "$m.x" && LC_ALL=C.UTF-8 msiextract "$OLDPWD/$m" >/dev/null 2>&1) ||
        { rm -rf "$m.x" && mkdir "$m.x" && python3 -I "$(dirname "$0")/msiunpack.py" "$m" "$m.x" >/dev/null 2>&1; }; }; then
      # its registry entries, for the launcher helper to write (install paths the game reads);
      # INSTALLDIR: where its biggest exe went
      d=$(find "$m.x" -iname '*.exe' -printf '%s\t%h\n' | sort -rn | head -1 | cut -f2)
      [ -n "$d" ] && python3 -I "$(dirname "$0")/msireg.py" "$m" "Z:\\vnvm\\game\\$(echo "${d#x.part/}" | tr / '\\')" >> x.part/_vnvm_reg.tsv
      rm -f "$m"
    else rm -rf "$m.x"
    fi
  done
  mv x.part x
  rm -f "$f"
fi
cd x
# the exe: vnbox's path (Games/<id>/...), else the biggest .exe that's not a tool
if [ -n "$exe" ] && [ -f "$exe" ]; then e=$exe
else
  e=$(find . -iname '*.exe' -printf '%s\t%P\n' | grep -aviE 'unins|setup|install|config|WindowsInstaller|InstMsi|KB[0-9]{6}|vcredist|vc_redist|redist|directx|dxsetup|tool|設定|update|DXWeb|dotnet' | sort -rn | head -1 | cut -f2)
fi
[ -n "$e" ] || { echo "no exe" >&2; exit 3; }
# launchers that start the game as a second process: run the game exe itself (vnvm's processes
# share one address space, so the game can't get 0x400000 while the launcher holds it)
g=$(find . -iname cs2.exe -printf '%P\n' | head -1)
[ -n "$g" ] && e=$g
# the launcher helper (host/tests/autoclick.c) goes in the tree root: corpus-run.py starts the
# game through it so launcher dialogs get pressed and every dialog's text gets logged
AC=${AUTOCLICK_EXE:-$HOME/.cache/vnvm/autoclick.exe}
if [ ! -f "$AC" ] && command -v i686-w64-mingw32-gcc >/dev/null; then
  mkdir -p "$(dirname "$AC")"
  i686-w64-mingw32-gcc -O2 "$(dirname "$0")/../host/tests/autoclick.c" -o "$AC.tmp" -municode && mv "$AC.tmp" "$AC"
fi
[ -f "$AC" ] && cp "$AC" ./_vnvm_autoclick.exe
if [ ! -d "$store" ]; then
  rm -rf "$store.part"
  cp -al "$BASE" "$store.part"
  vnvm --store "$store.part" pack . g >/dev/null
  mv "$store.part" "$store"
fi
echo "$e"
