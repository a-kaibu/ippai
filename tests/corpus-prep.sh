#!/bin/bash
# Downloads, extracts and packs one vnbox corpus title for tests/corpus.mjs:
#   tests/corpus-prep.sh ID URL EXE_REL STORE
# -> TRIALS/ID/x (extracted), STORE (hard-linked copy of BASE_STORE + tree "g"; "": none)
# prints the exe path inside the tree on success
set -e
id=$1 url=$2 exe=$3 store=$4
HERE=$(cd "$(dirname "$0")" && pwd)
T=${TRIALS:-/home/user/trials}/$id
BASE=${BASE_STORE:-/home/user/work/base-store}
mkdir -p "$T"
cd "$T"
f=dl.${url##*.}
dl() {  # url: $f.part, resumed (the proxy sometimes cuts long transfers)
  local i
  # (DL_TRIES / DL_MAX_TIME: CI gives a dead host less time)
  for i in $(seq "${DL_TRIES:-10}"); do
    curl -sSfL --connect-timeout 30 --max-time "${DL_MAX_TIME:-3600}" -C - -o "$f.part" "$1" && return 0
    sleep $((i * 2))
  done
  return 1
}
# (already extracted: no download)
if [ ! -d x ] && [ ! -s "$f" ]; then
  ok=
  # MIRROR (tests/corpus/mirror.py fetch): the VN Mirror Archive's copy when it has the title,
  # checked against its sha256; else (or when that fails) the title's own site
  if [ -n "${MIRROR:-}" ] && m=$(python3 -I "$HERE/corpus/mirror.py" lookup "$MIRROR" "$url"); then
    IFS=$'\t' read -r murl msha msize <<< "$m"
    rm -f "$f.part"
    if dl "$murl" && echo "$msha  $f.part" | sha256sum -c --status; then ok=mirror
    else echo "mirror: $murl failed or its sha256 differs; the title's own site" >&2; rm -f "$f.part"
    fi
  fi
  if [ -z "$ok" ] && [ -n "${MIRROR_ONLY:-}" ]; then echo "download failed: not on the mirror" >&2; exit 4; fi
  if [ -z "$ok" ]; then dl "$url" && ok=origin; fi
  [ -n "$ok" ] || { echo "download failed" >&2; exit 4; }
  mv "$f.part" "$f"
  echo "$ok" > source
fi
if [ ! -d x ]; then
  rm -rf x.part; mkdir x.part
  case "${f,,}" in
    # (a file or two failing, e.g. a damaged Thumbs.db, still leaves a game)
    # (unar leaves some -lh5- members of PrismRhythm_trial_Web.lzh empty; 7z reads them, and
    # Shift-JIS names are converted below)
    *.lzh) (cd x.part && LC_ALL=C.UTF-8 7z x -y ../$f >/dev/null 2>&1 ||
             LC_ALL=C.UTF-8 unar -q -f -e cp932 -o . ../$f >/dev/null 2>&1 || [ -n "$(ls)" ]) ;;
    # (unar makes a 0-byte file of a directory entry named with backslashes, "Game\MonoBleedingEdge\",
    # and then fails on the files inside it; unzip-cp932.py takes either separator. Its zipfile
    # lacks some methods unar has, so unar's partial result stays the last resort.)
    *.zip) (cd x.part && { LC_ALL=C.UTF-8 unar -q -e cp932 -o . ../$f >/dev/null 2>&1 ||
             { find . -mindepth 1 -delete && python3 -I "$HERE/unzip-cp932.py" ../$f . >/dev/null 2>&1 && [ -n "$(ls)" ]; } ||
             { find . -mindepth 1 -delete; LC_ALL=C.UTF-8 unar -q -e cp932 -o . ../$f >/dev/null 2>&1 || [ -n "$(ls)" ]; }; }) ;;
    *.rar|*.7z) (cd x.part && LC_ALL=C.UTF-8 unar -q -o . ../$f >/dev/null 2>&1 || [ -n "$(ls)" ]) ;;
    *.exe)
      # InstallShield setup with its files in the exe, a zlib-stream setup (zsfx.py), self-extracting
      # archive or Inno Setup installer. Each try starts from an empty folder: one that fails half
      # way leaves files behind (unar on Front Wing's LHa self-extractor canary_game_preview.exe:
      # a SYSTEM.PAK cut at 1.75 MB, an empty IMAGE.PAK), and the next one's whole copy under
      # Shift-JIS names then sat beside them while the game ran from the broken one
      (cd x.part && (python3 -I "$HERE/isextract.py" "../$f" . >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; python3 -I "$HERE/zsfx.py" "../$f" . >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; LC_ALL=C.UTF-8 innoextract -s -q -e "../$f" 2>/dev/null && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; LC_ALL=C.UTF-8 unar -q -e cp932 -o . "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         # (7z exits 2 when an EXEpress CX SFX names a cabinet volume it doesn't carry,
         # helter_taikenban_fubuki.exe's helt_t2.cab, though it wrote every file: kept when each
         # file its listing names is there whole, tests/7z-complete.py)
         (find . -mindepth 1 -delete; { LC_ALL=C.UTF-8 7z x -y "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]; } ||
            python3 -I "$HERE/7z-complete.py" "../$f" . >/dev/null 2>&1) ||
         (find . -mindepth 1 -delete; LC_ALL=C.UTF-8 unrar x -y "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         # an installer nothing here unpacks (michishio_trial_taming2_install.exe: Gentee
         # CreateInstall): the exe itself, under its own name, run in sake as on Windows
         { find . -mindepth 1 -delete; n=${url##*/}; n=${n%%\?*}; cp "../$f" "./${n:-setup.exe}"; }) ;;
  esac
  # Shift-JIS names split at a 0x5C trail byte (7-Zip on LZH self-extractors: 十 = 8F 5C) joined again
  python3 -I "$HERE/sjis-rejoin.py" x.part >/dev/null 2>&1 || true
  # names some extractors leave in Shift-JIS bytes -> UTF-8
  LC_ALL=C.UTF-8 convmv -r -f cp932 -t utf8 --notest --qfrom x.part >/dev/null 2>&1 || true
  # an archive that holds an archive and no program (niidumafuwatoro_trial.zip: a folder with
  # NiizumaFuwatoro_trial.zip in it): the inner archives are unpacked next to themselves
  if [ -z "$(find x.part -type f -iname '*.exe' -print -quit)" ]; then
    find x.part -type f \( -iname '*.zip' -o -iname '*.lzh' -o -iname '*.rar' -o -iname '*.7z' \) -print0 |
    while IFS= read -r -d '' a; do
      rm -rf "$a.x" && mkdir "$a.x"
      if (cd "$a.x" && { LC_ALL=C.UTF-8 unar -q -e cp932 -o . "$OLDPWD/$a" >/dev/null 2>&1 ||
            LC_ALL=C.UTF-8 7z x -y "$OLDPWD/$a" >/dev/null 2>&1; }) && [ -n "$(ls "$a.x")" ]; then rm -f "$a"
      else rm -rf "$a.x"
      fi
    done
  fi
  # installers (vnbox ran the installed game): unpack InstallShield setups, Windows Installer
  # packages and Inno Setup installers here rather than run them in the browser (an installer
  # writing a 1 GB package into the in-memory file system runs out of wasm memory)
  # (rounds: an installer can come out of another one, kagura_r_trial010a.exe is a self-extracting
  # zip of an Inno Setup installer; a round that changed no .exe ends it)
  for round in 1 2 3; do
  before=$(find x.part -type f -iname '*.exe' | sort | md5sum)
  find x.part -type f -iname '*.exe' -print0 | while IFS= read -r -d '' s; do
    if python3 -I "$HERE/isextract.py" "$s" "$s.is" >/dev/null 2>&1; then rm -f "$s"
    elif LC_ALL=C.UTF-8 innoextract -s -q -e -d "$s.inno" "$s" >/dev/null 2>&1 && [ -n "$(ls "$s.inno")" ]; then rm -f "$s"
    # a self-extracting zip of the game (kaguya's sperman_trial.exe); not a program that only
    # carries a zip of its own data (NW.js): the zip has to hold an .exe
    elif unzip -Z1 "$s" 2>/dev/null | grep -qi '\.exe$' &&
        python3 -I "$HERE/unzip-cp932.py" "$s" "$s.zip" >/dev/null 2>&1 && [ -n "$(ls "$s.zip")" ]; then rm -f "$s"
    # a WinRAR (or other) self-extractor of the game inside the archive (koikata_ichika_trial.zip:
    # its WinRAR SFX ran in sake and wrote 375 MB into the page until the browser died)
    elif LC_ALL=C.UTF-8 lsar -e cp932 "$s" 2>/dev/null | sed -n '1{/: Embedded /!q1};2,$p' | grep -qi '\.exe$' &&
        LC_ALL=C.UTF-8 unar -q -e cp932 -o "$s.sfx" "$s" >/dev/null 2>&1 && [ -n "$(ls "$s.sfx")" ]; then rm -f "$s"
    # a cabinet self-extractor holding the game (騎士姫クラリス's taiken exe, which unpacks
    # itself with cabinet.dll's FDI and quits: the game is a separate exe inside)
    elif LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | grep -qx 'Type = Cab' &&
        LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | sed '1,/^----------$/d' | grep -qiE '^Path = .*\.exe$' &&
        mkdir -p "$s.cab" && (cd "$s.cab" && LC_ALL=C.UTF-8 7z x -y "$OLDPWD/$s" >/dev/null 2>&1) && [ -n "$(ls "$s.cab")" ]; then rm -f "$s"
    else rm -rf "$s.is" "$s.inno" "$s.zip" "$s.sfx" "$s.cab"
    fi
  done
  [ "$(find x.part -type f -iname '*.exe' | sort | md5sum)" = "$before" ] && break
  done
  # BGI install media: a 16-byte "0123456789ABCDEF" file next to BGIForInstalling.exe marks
  # the media, and the game run from there quits at once (the installed copy doesn't have it)
  find x.part -type f -iname BGIForInstalling.exe -print0 | while IFS= read -r -d '' b; do
    find "$(dirname "$b")" -maxdepth 1 -type f -size 16c -exec grep -lx -m1 0123456789ABCDEF {} + 2>/dev/null | xargs -r -d '\n' rm -f
  done
  find x.part -type f -iname '*.msi' -print0 | while IFS= read -r -d '' m; do
    # (msiextract can't do packages split over several cabinets: tests/msiunpack.py)
    if mkdir -p "$m.x" && { (cd "$m.x" && LC_ALL=C.UTF-8 msiextract "$OLDPWD/$m" >/dev/null 2>&1) ||
        { rm -rf "$m.x" && mkdir "$m.x" && python3 -I "$HERE/msiunpack.py" "$m" "$m.x" >/dev/null 2>&1; }; }; then
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
# (the same file one folder further down: silimagi_trial.zip has dl/startup.exe)
elif [ -n "$exe" ] && e=$(find . -ipath "*/$exe" -printf '%P\n' | head -1) && [ -n "$e" ]; then :
else
  # (matched without the folders the unpacking above made: setup.exe.inno/app/game.exe is the
  # game Inno Setup installs, not a setup program)
  e=$(find . -iname '*.exe' -printf '%s\t%P\n' |
      while IFS=$'\t' read -r n p; do
        k=$(sed -E 's#[^/]*\.(exe|msi|zip|lzh|rar|7z)\.(is|inno|zip|sfx|x)/##Ig' <<< "$p")
        grep -qaiE 'unins|setup|install|config|WindowsInstaller|InstMsi|KB[0-9]{6}|vcredist|vc_redist|redist|directx|dxsetup|tool|設定|update|DXWeb|dotnet|CrashHandler' <<< "$k" || printf '%s\t%s\n' "$n" "$p"
      done | sort -rn | head -1 | cut -f2)
  # (nothing but installers and tools: the biggest of them, an installer sake runs itself)
  [ -n "$e" ] || e=$(find . -iname '*.exe' -printf '%s\t%P\n' | sort -rn | head -1 | cut -f2)
fi
[ -n "$e" ] || { echo "no exe" >&2; exit 3; }
# launchers that start the game as a second process: run the game exe itself (vnvm's processes
# share one address space, so the game can't get 0x400000 while the launcher holds it)
g=$(find . -iname cs2.exe -printf '%P\n' | head -1)
[ -n "$g" ] && e=$g
# (an empty STORE: only the tree, e.g. for recording on Windows: tests/replay/gametrace.sh)
if [ -n "$store" ] && [ ! -d "$store" ]; then
  rm -rf "$store.part"
  cp -al "$BASE" "$store.part"
  ippai --store "$store.part" pack . g >/dev/null
  mv "$store.part" "$store"
fi
echo "$e"
