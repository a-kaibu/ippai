#!/bin/bash
# Downloads, extracts and packs one masu corpus title for tests/corpus.mjs:
#   tests/corpus-prep.sh ID URL EXE_REL STORE
# -> TRIALS/ID/x (extracted), STORE (hard-linked copy of BASE_STORE + tree "g"; "": none)
# prints the exe path inside the tree on success (C:\...: the game a setup program installed)
# env: SAKE_NODE (node's sake.js: setup programs run in it, tests/corpus/install.py), SAKE_FONTS
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
      # (not a setup program that runs an .msi, InstallShield's Setup Launcher: it runs in sake,
      # tests/corpus/install.py keep)
      (cd x.part && { python3 -I "$HERE/corpus/install.py" keep "../$f" && n=${url##*/} && n=${n%%\?*} && cp "../$f" "./${n:-setup.exe}"; } ||
         (python3 -I "$HERE/isextract.py" "../$f" . >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; python3 -I "$HERE/zsfx.py" "../$f" . >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; LC_ALL=C.UTF-8 innoextract -s -q -e "../$f" 2>/dev/null && [ -n "$(ls)" ]) ||
         (find . -mindepth 1 -delete; LC_ALL=C.UTF-8 unar -q -e cp932 -o . "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]) ||
         # (7z exits 2 when an EXEpress CX SFX names a cabinet volume it doesn't carry,
         # helter_taikenban_fubuki.exe's helt_t2.cab, though it wrote every file: kept when each
         # file its listing names is there whole, tests/7z-complete.py)
         # (not a program with nothing in it, which 7-Zip opens as "Type = PE" and unpacks into
         # its sections: .text, .rsrc/...)
         (find . -mindepth 1 -delete; ! LC_ALL=C.UTF-8 7z l -slt "../$f" 2>/dev/null | grep -qx 'Type = PE' &&
            { { LC_ALL=C.UTF-8 7z x -y "../$f" >/dev/null 2>&1 && [ -n "$(ls)" ]; } ||
              python3 -I "$HERE/7z-complete.py" "../$f" . >/dev/null 2>&1; }) ||
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
  # installers (masu ran the installed game): unpack InstallShield setups, Windows Installer
  # packages and Inno Setup installers here rather than run them in the browser (an installer
  # writing a 1 GB package into the in-memory file system runs out of wasm memory)
  # (rounds: an installer can come out of another one, kagura_r_trial010a.exe is a self-extracting
  # zip of an Inno Setup installer; a round that changed no .exe ends it)
  for round in 1 2 3; do
  before=$(find x.part -type f -iname '*.exe' | sort | md5sum)
  find x.part -type f -iname '*.exe' -print0 | while IFS= read -r -d '' s; do
    # (a setup program that runs an .msi stays, for sake: tests/corpus/install.py keep)
    if python3 -I "$HERE/corpus/install.py" keep "$s"; then :
    elif python3 -I "$HERE/isextract.py" "$s" "$s.is" >/dev/null 2>&1; then rm -f "$s"
    elif LC_ALL=C.UTF-8 innoextract -s -q -e -d "$s.inno" "$s" >/dev/null 2>&1 && [ -n "$(ls "$s.inno")" ]; then rm -f "$s"
    # a self-extracting zip of the game (kaguya's sperman_trial.exe); not a program that only
    # carries a zip of its own data (NW.js): the zip has to hold an .exe
    elif unzip -Z1 "$s" 2>/dev/null | grep -qi '\.exe$' &&
        python3 -I "$HERE/unzip-cp932.py" "$s" "$s.zip" >/dev/null 2>&1 && [ -n "$(ls "$s.zip")" ]; then rm -f "$s"
    # a WinRAR (or other) self-extractor of the game inside the archive (koikata_ichika_trial.zip:
    # its WinRAR SFX ran in sake and wrote 375 MB into the page until the browser died)
    elif LC_ALL=C.UTF-8 lsar -e cp932 "$s" 2>/dev/null | sed -n '1{/: Embedded /!q1};2,$p' | grep -qi '\.exe$' &&
        LC_ALL=C.UTF-8 unar -q -e cp932 -o "$s.sfx" "$s" >/dev/null 2>&1 && [ -n "$(ls "$s.sfx")" ]; then rm -f "$s"
    # (one unar does not know, as a RAR5 one: 7-Zip lists it, unrar unpacks it (Debian's 7-Zip
    # has no RAR codec). CYCLET's tokage_trial_kanau_web.exe ran in sake, unpacked part of itself,
    # opened its readme and ended)
    elif LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | grep -qxE 'Type = Rar5?' &&
        LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | sed '1,/^----------$/d' | grep -qiE '^Path = .*\.exe$' &&
        mkdir -p "$s.sfx" && (cd "$s.sfx" && LC_ALL=C.UTF-8 unrar x -y "$OLDPWD/$s" >/dev/null 2>&1) && [ -n "$(ls "$s.sfx")" ]; then rm -f "$s"
    # a cabinet self-extractor holding the game (騎士姫クラリス's taiken exe, which unpacks
    # itself with cabinet.dll's FDI and quits: the game is a separate exe inside)
    elif LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | grep -qx 'Type = Cab' &&
        LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | sed '1,/^----------$/d' | grep -qiE '^Path = .*\.exe$' &&
        # (an EXEpress CX one names a cabinet volume it doesn't carry and 7z exits 2 after writing
        # every file, 下戸勇者_体験版.exe's "EXEpress Project.cab": kept when all are there whole)
        mkdir -p "$s.cab" && (cd "$s.cab" && { LC_ALL=C.UTF-8 7z x -y "$OLDPWD/$s" >/dev/null 2>&1 ||
            python3 -I "$HERE/7z-complete.py" "$OLDPWD/$s" . >/dev/null 2>&1; }) && [ -n "$(ls "$s.cab")" ]; then rm -f "$s"
    # an LHa self-extractor holding the game (痴漢車両2号車R's ChikanSyaryou2TaikenData.exe in
    # discovery_0037t.zip; lsar fails on it, "Archive parsing failed"): 7z reads it
    elif LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | grep -qx 'Type = Lzh' &&
        LC_ALL=C.UTF-8 7z l -slt "$s" 2>/dev/null | sed '1,/^----------$/d' | grep -qiE '^Path = .*\.exe$' &&
        mkdir -p "$s.lzh" && (cd "$s.lzh" && LC_ALL=C.UTF-8 7z x -y "$OLDPWD/$s" >/dev/null 2>&1) && [ -n "$(ls "$s.lzh")" ]; then rm -f "$s"
    # an electron-builder NSIS installer (あにまるめいど!'s animal_maid-1.0.0-x64.exe): the app is
    # $PLUGINSDIR/app-64.7z (or app-32.7z), what the installer would put in its folder. Unpacked
    # here, the page runs its resources/app.asar as a web game (masu pack: asar.rs) instead of
    # sake running the installer (#234: 914 MB in the page) and then Electron's x64 Chromium
    elif a=$(python3 -I "$HERE/corpus/electron_nsis.py" "$s" "$s.app" 2>/dev/null) && [ -n "$(ls "$s.app")" ]; then rm -f "$s"
    else rm -rf "$s.is" "$s.inno" "$s.zip" "$s.sfx" "$s.cab" "$s.lzh" "$s.app"
    fi
  done
  [ "$(find x.part -type f -iname '*.exe' | sort | md5sum)" = "$before" ] && break
  done
  # (names from the extractors above, 7z on a cabinet or LHa self-extractor: as after the outer archive)
  python3 -I "$HERE/sjis-rejoin.py" x.part >/dev/null 2>&1 || true
  LC_ALL=C.UTF-8 convmv -r -f cp932 -t utf8 --notest --qfrom x.part >/dev/null 2>&1 || true
  # BGI install media: a 16-byte "0123456789ABCDEF" file next to BGIForInstalling.exe marks
  # the media, and the game run from there quits at once (the installed copy doesn't have it)
  find x.part -type f -iname BGIForInstalling.exe -print0 | while IFS= read -r -d '' b; do
    find "$(dirname "$b")" -maxdepth 1 -type f -size 16c -exec grep -lx -m1 0123456789ABCDEF {} + 2>/dev/null | xargs -r -d '\n' rm -f
  done
  find x.part -type f -iname '*.msi' -print0 | while IFS= read -r -d '' m; do
    # (msiextract can't do packages split over several cabinets: tests/msiunpack.py)
    # (a package its setup program installs is left to it: tests/corpus/install.py keep)
    if python3 -I "$HERE/corpus/install.py" keep "$m"; then :
    elif mkdir -p "$m.x" && { (cd "$m.x" && LC_ALL=C.UTF-8 msiextract "$OLDPWD/$m" >/dev/null 2>&1) ||
        { rm -rf "$m.x" && mkdir "$m.x" && python3 -I "$HERE/msiunpack.py" "$m" "$m.x" >/dev/null 2>&1; }; }; then
      rm -f "$m"
    else rm -rf "$m.x"
    fi
  done
  mv x.part x
  rm -f "$f"
fi
cd x
# the exe: masu's path (Games/<id>/...), else tests/corpus/pickexe.py's
if [ -n "$exe" ] && [ -f "$exe" ]; then e=$exe
# (the same file one folder further down: silimagi_trial.zip has dl/startup.exe)
elif [ -n "$exe" ] && e=$(find . -ipath "*/$exe" -printf '%P\n' | head -1) && [ -n "$e" ]; then :
else
  # (tests/corpus/pickexe.py: Unity's and Ren'Py's own exe, else the biggest that is not a tool)
  # (exit 5: not a game, a sample movie or a patch: tests/corpus/pickexe.py not_game)
  rc=0; e=$(python3 -I "$HERE/corpus/pickexe.py" . "${url%%\?*}") || rc=$?
  [ "$rc" = 5 ] && exit 5
  [ "$rc" = 0 ] || e=
fi
# setup programs nothing here unpacks (Gentee CreateInstall, DGCA and YU-RIS self-extractors) and
# install media whose game only runs installed (autorun.inf beside a setup program: BLUEGALE's
# setup.exe, CatSystem2's INSTALL.exe): run in sake under node as a person taking the defaults
# (tests/corpus/install.py, SAKE_AUTO), once per extracted tree. Its C:\ ($T/c: the installed
# game, the registry) becomes the title's C:\ (the store's prefix32), what it wrote beside
# itself stays in the tree; the game it made is run (C:\... or a path in the tree).
if [ -n "${SAKE_NODE:-}" ] && [ ! -e "$T/install.done" ] && s=$(python3 -I "$HERE/corpus/install.py" choose . "$e"); then
  rm -rf "$T/c"
  python3 -I "$HERE/corpus/install.py" run . "$s" "$T/c" > "$T/install.done.part" 2>> "$T/install.err" || true
  mv "$T/install.done.part" "$T/install.done"
fi
installed=
[ -n "$e" ] || [ -s "$T/install.done" ] || { echo "no exe" >&2; exit 3; }
if [ -s "$T/install.done" ]; then
  IFS=$'\t' read -r where g < "$T/install.done"
  case $where in
    Z) e=$g ;;
    C) e="C:\\${g//\//\\}"; installed=1 ;;
  esac
fi
# launchers that start the game as a second process: run the game exe itself (vnvm's processes
# share one address space, so the game can't get 0x400000 while the launcher holds it)
g=$(find . -iname cs2.exe -printf '%P\n' | head -1)
[ -n "$g" ] && [ -z "$installed" ] && e=$g
# (an empty STORE: only the tree, e.g. for recording on Windows: tests/replay/gametrace.sh)
if [ -n "$store" ] && [ ! -d "$store" ]; then
  rm -rf "$store.part"
  cp -al "$BASE" "$store.part"
  masu --store "$store.part" pack . g >/dev/null
  # (an installed game: the title's own C:\, the installer's with sake's fonts, as prefix32;
  # the base's tree list is a hard link, removed rather than written through)
  if [ -d "$T/c" ] && [ -n "${SAKE_FONTS:-}" ] && [ -n "$(find "$T/c" -type f -not -ipath '*/windows/system32/msiexec.exe' -print -quit)" ]; then
    rm -rf "$T/prefix" && mkdir -p "$T/prefix" && cp -al "$T/c" "$T/prefix/drive_c"
    w=$(find "$T/prefix/drive_c" -mindepth 1 -maxdepth 1 -iname windows -print -quit)
    w=${w:-$T/prefix/drive_c/windows}
    mkdir -p "$w"
    [ -n "$(find "$w" -mindepth 1 -maxdepth 1 -iname temp -print -quit)" ] || mkdir "$w/temp"
    [ -n "$(find "$w" -mindepth 1 -maxdepth 1 -iname fonts -print -quit)" ] || cp -r "$SAKE_FONTS" "$w/Fonts"
    rm -f "$store.part/trees/prefix32.tsv"
    masu --store "$store.part" pack "$T/prefix" prefix32 >/dev/null
    rm -rf "$T/prefix"
  fi
  mv "$store.part" "$store"
fi
echo "$e"
