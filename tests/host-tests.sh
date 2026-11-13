#!/bin/bash
# Console checks of host/tests on sake (its node build): each is built with mingw-w64, then run
# with the JIT and with the interpreter (IPPAI_JIT=0). Both outputs must equal
# host/tests/expected/<name>.txt (checksums of integer, MMX/SSE, atomics, self-modifying code
# results; they match Windows). UPDATE=1 rewrites the expected files. Exits 1 on a failure.
# Tests in X64 are also built for x86-64 (mingw-w64: msvcrt) and must print the same: sake's
# C runtime has to behave alike in both; they run once more merging regions at the first hot
# jump and reclaiming after every load (IPPAI_JIT_MERGE_HOPS=1 IPPAI_JIT_RECLAIM=1). CLANG64 are x86-64 programs clang builds (SEH, and
# x64crt: an MSVC-ABI program on ucrtbase/vcruntime140; msvcpsync: msvcp140 threads/sync;
# msvcstream: its iostreams and locale facets, built against Microsoft's STL headers; msvcrtti:
# dynamic_cast and typeid (vcruntime140's __RTDynamicCast...); msvcseh: _set_se_translator
# with /EHa; mmdev: the audio device enumerator; avx256: ymm forms, expected from a native
# x86-64 Linux build), each with its own expected file. CLANG32 are
# x86 MSVC-ABI programs on msvcrt.dll (build-msvc32.sh), expected in <name>-32.txt.
#
#   tests/host-tests.sh [name ...]      SAKE=path/to/sake.js (default /home/user/work/sake-out/sake.js)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SAKE=${SAKE:-/home/user/work/sake-out/sake.js}
OUT=${OUT:-$(mktemp -d)}
TESTS=${*:-afdpoll callwndhook dllinit dxversion dircase dlgenter bigwindow aclentries acctnames acmdriver stacksize passwordchar wininetquery msgwaitstart makepath netbios mbshort mbtokens winineturl dialogtab mbsecure olefont comclass registrymulti imagecert capiprovider recalloc comrpc services dllsearch cryptoapi nullcall intops simd atomics smc cmp64 ssecvt dinputtest textbounds crtstdio fileseek crtfiles hlsltest d3dx9shader d3dx9effect d3dx9resource struct64 guard sse4 xaudio2 winrt wtsapi fastpaths apc rawinput dxgidesc mmxblit sysdlls dshowend srwlock beginthread d3d12delay mfengine jitcode wasapi waveout hops sendrace dlgthread editnotify shfileinfo deleteonclose filelock bcryptkeys listbox searchpath resupdate shfolders shelllink sendnotify browsefolder shellstubs msiinstall msiui nsisinstall innoinstall jalocale msgboxes wideprintf dllversion mkdirsep apiimage nuldevice virtualex filetitle}
X64=" afdpoll nuldevice callwndhook dllinit dircase dlgenter dlgthread editnotify shfileinfo deleteonclose filelock bcryptkeys listbox searchpath resupdate shfolders shelllink sendnotify browsefolder shellstubs aclentries acctnames acmdriver stacksize passwordchar wininetquery msgwaitstart makepath netbios mbshort mbtokens winineturl dialogtab mbsecure olefont comclass registrymulti imagecert capiprovider recalloc comrpc services dllsearch cryptoapi hops struct64 guard xaudio2 crtstdio fileseek crtfiles cmp64 winrt wtsapi atomics fastpaths apc rawinput dxgidesc mmxblit sysdlls srwlock beginthread d3d12delay mfengine wasapi waveout msiinstall msiui nsisinstall innoinstall jalocale msgboxes wideprintf dllversion mkdirsep virtualex filetitle "
CLANG64=${CLANG64-x64seh x64crt msvcpchar msvcpsync msvcstream msvcrtti msvcseh mmdev avx256}
CLANG32=${CLANG32-msvcseh}
# C++ (g++, static libstdc++) built for x86 and x64, one expected output for both
GXX=${GXX-gxxunwind}
# Tests in VEX are also built with -mavx (the VEX forms of their SSE instructions)
VEX=" sse4 "
fail=0
for t in $TESTS; do
  test_env=()
  ldir=$OUT/lib-none
  fixture() {
    if [ "$t" = dllinit ]; then
      "$1" -O2 -DBUILD_FIXTURE -shared -Wl,--kill-at "$ROOT/host/tests/dllinit.c" -o "$OUT/dllinit-fixture.dll"
      return $?
    fi
    if [ "$t" = d3d12delay ]; then
      # (d3d12.dll as a delay-load import, as UnityPlayer.dll has it: mingw's own helper)
      ldir=$OUT/lib-$t-${1%%-*}
      local at16=@16 at8=@8
      [ "${1%%-*}" = x86_64 ] && at16= at8=
      mkdir -p "$ldir" && printf 'LIBRARY d3d12.dll\nEXPORTS\nD3D12CreateDevice%s\nD3D12GetDebugInterface%s\n' "$at16" "$at8" > "$ldir/d3d12.def" &&
        "${1%gcc}dlltool" -k -d "$ldir/d3d12.def" -y "$ldir/libd3d12.a"
      return $?
    fi
    if [ "$t" = comclass ]; then
      "$1" -O2 -DBUILD_FIXTURE -shared -Wl,--kill-at "$ROOT/host/tests/comclass.c" -lole32 -luuid -o "$OUT/comclass-fixture.dll"
      return $?
    fi
    if [ "$t" = comrpc ]; then
      "$1" -O2 -shared -Wl,--kill-at "$ROOT/host/tests/comrpc-proxy.c" -loleaut32 -lrpcrt4 -o "$OUT/comrpc-proxy.dll"
      return $?
    fi
    if [ "$t" = msiinstall ]; then
      # (the package twice: files in its embedded cabinet, and uncompressed beside it)
      local d=$OUT/msiinstall-src
      rm -rf "$d" "$OUT/MsiTest" && mkdir -p "$d" "$OUT/MsiTest/sub" || return 1
      printf 'hello msi\r\n' > "$d/a.txt"
      head -c 70000 /dev/zero | tr '\0' b > "$d/b.dat"
      printf 'extra\r\n' > "$d/extra.txt"
      sed 's/COMPRESSED/yes/' "$ROOT/host/tests/msiinstall.wxs" > "$d/t.wxs"
      (cd "$d" && wixl -o "$OUT/msiinstall.msi" t.wxs) || return 1
      # (a folder search wixl cannot write: probe.txt one level under Program Files\MsiSearch)
      msibuild "$OUT/msiinstall.msi" -q 'CREATE TABLE `DrLocator` (`Signature_` CHAR(72) NOT NULL, `Parent` CHAR(72), `Path` CHAR(255), `Depth` SHORT PRIMARY KEY `Signature_`, `Parent`, `Path`)' \
        -q "INSERT INTO \`DrLocator\` (\`Signature_\`, \`Path\`, \`Depth\`) VALUES ('probe', '[ProgramFilesFolder]MsiSearch', 1)" \
        -q "INSERT INTO \`Signature\` (\`Signature\`, \`FileName\`) VALUES ('probe', 'PROBE.TXT|probe.txt')" \
        -q "INSERT INTO \`AppSearch\` (\`Property\`, \`Signature_\`) VALUES ('FOUNDFILE', 'probe')" || return 1
      cp "$OUT/msiinstall.msi" "$OUT/msiinstall-u.msi" && msibuild "$OUT/msiinstall-u.msi" -q "UPDATE File SET Attributes = 8192" || return 1
      cp "$d/a.txt" "$d/extra.txt" "$OUT/MsiTest/" && cp "$d/b.dat" "$OUT/MsiTest/sub/"
      return $?
    fi
    if [ "$t" = nsisinstall ]; then
      local d=$OUT/nsisinstall-src
      rm -rf "$d" && mkdir -p "$d" || return 1
      printf 'hello nsis\r\n' > "$d/a.txt"
      head -c 300000 /dev/zero | tr '\0' n > "$d/b.dat"
      cp "$ROOT/host/tests/nsisinstall.nsi" "$d/" && (cd "$d" && makensis -V1 nsisinstall.nsi) && cp "$d/nsisinstall-setup.exe" "$OUT/"
      return $?
    fi
    if [ "$t" = innoinstall ]; then
      # Inno Setup 6.7.4 (its official installer, checked; kept in INNO_CACHE) installed on sake
      # by itself once per OUT, then its ISCC on sake compiles innoinstall.iss (once: the x64
      # run takes the same installer)
      if [ -f "$OUT/innoinstall-setup.exe" ]; then
        python3 "$ROOT/tests/check-inno-fixture.py" "$OUT/innoinstall-setup.exe" >> "$OUT/inno-fixture.log" 2>&1 || {
          cat "$OUT/inno-fixture.log"; return 1;
        }
        return 0
      fi
      local v=6.7.4 sum=a549929319f9120716677c685535ca4671e4b937b5fdc87e2585f32d8eb30e8c
      local cache=${INNO_CACHE:-$HOME/.cache/ippai-inno} tools=$OUT/inno-tools
      local exe=$cache/innosetup-$v.exe
      mkdir -p "$cache"
      # (CI runs this file twice at once (sake and sake-grow): one downloads the installer, the
      # other waits; the lock goes with the subshell, so a failed download holds nothing)
      if ! (
        if command -v flock >/dev/null; then flock 9; fi
        if ! echo "$sum  $exe" | sha256sum -c --quiet 2>/dev/null; then
          curl -sSfL --retry 4 -o "$exe.part" "https://github.com/jrsoftware/issrc/releases/download/is-${v//./_}/innosetup-$v.exe" &&
            echo "$sum  $exe.part" | sha256sum -c --quiet && mv "$exe.part" "$exe" || exit 1
        fi
      ) 9>"$cache/.download.lock"; then return 1; fi
      if [ ! -f "$tools/IS6/ISCC.exe" ]; then
        rm -rf "$tools" && mkdir -p "$tools/windows/system32" "$tools/windows/temp" || return 1
        (cd "$(dirname "$SAKE")" && timeout 900 env SAKE_C="$tools" SAKE_FONTS="$FONTS" node "$SAKE" "$exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART '/DIR=C:\IS6' /CURRENTUSER > "$OUT/inno-setup.log" 2>&1)
        [ -f "$tools/IS6/ISCC.exe" ] || { tail -5 "$OUT/inno-setup.log"; return 1; }
      fi
      rm -rf "$tools/src" && mkdir -p "$tools/src" || return 1
      printf 'hello inno\r\n' > "$tools/src/a.txt"
      head -c 300000 /dev/zero | tr '\0' i > "$tools/src/b.dat"
      cp "$ROOT/host/tests/innoinstall.iss" "$tools/src/" || return 1
      (cd "$(dirname "$SAKE")" && timeout 900 env SAKE_C="$tools" SAKE_FONTS="$FONTS" SAKE_TRACE_EXIT=1 SAKE_TRACE_FILES=1 node "$SAKE" "$tools/IS6/ISCC.exe" 'C:\src\innoinstall.iss' > "$OUT/inno-iscc.log" 2>&1) &&
        cp "$tools/src/out/innoinstall-setup.exe" "$OUT/" || { tail -20 "$OUT/inno-iscc.log"; return 1; }
      # Keep a broken output for the CI artifact; do not retry it into a passing run.
      python3 "$ROOT/tests/check-inno-fixture.py" "$OUT/innoinstall-setup.exe" > "$OUT/inno-fixture.log" 2>&1 || {
        cat "$OUT/inno-fixture.log"; tail -40 "$OUT/inno-iscc.log"; return 1;
      }
      python3 "$ROOT/tests/test_inno_fixture.py" "$OUT/innoinstall-setup.exe" >> "$OUT/inno-fixture.log" 2>&1 || {
        cat "$OUT/inno-fixture.log"; return 1;
      }
      return 0
    fi
    if [ "$t" = msiui ]; then
      # (msiinstall.wxs's package with dialogs: Welcome (name, extras, Next / Cancel), a
      # modeless progress dialog, Exit)
      local d=$OUT/msiui-src
      rm -rf "$d" && mkdir -p "$d" || return 1
      printf 'hello msi\r\n' > "$d/a.txt"
      head -c 70000 /dev/zero | tr '\0' b > "$d/b.dat"
      printf 'extra\r\n' > "$d/extra.txt"
      sed 's/COMPRESSED/yes/' "$ROOT/host/tests/msiinstall.wxs" > "$d/t.wxs"
      (cd "$d" && wixl -o "$OUT/msiui.msi" t.wxs) || return 1
      local ctl="INSERT INTO \`Control\` (\`Dialog_\`, \`Control\`, \`Type\`, \`X\`, \`Y\`, \`Width\`, \`Height\`, \`Attributes\`, \`Property\`, \`Text\`) VALUES"
      local ev="INSERT INTO \`ControlEvent\` (\`Dialog_\`, \`Control_\`, \`Event\`, \`Argument\`, \`Condition\`, \`Ordering\`) VALUES"
      local dlg="INSERT INTO \`Dialog\` (\`Dialog\`, \`HCentering\`, \`VCentering\`, \`Width\`, \`Height\`, \`Attributes\`, \`Title\`, \`Control_First\`, \`Control_Default\`, \`Control_Cancel\`) VALUES"
      msibuild "$OUT/msiui.msi" \
        -q 'CREATE TABLE `Dialog` (`Dialog` CHAR(72) NOT NULL, `HCentering` SHORT NOT NULL, `VCentering` SHORT NOT NULL, `Width` SHORT NOT NULL, `Height` SHORT NOT NULL, `Attributes` LONG, `Title` CHAR(128), `Control_First` CHAR(50) NOT NULL, `Control_Default` CHAR(50), `Control_Cancel` CHAR(50) PRIMARY KEY `Dialog`)' \
        -q 'CREATE TABLE `Control` (`Dialog_` CHAR(72) NOT NULL, `Control` CHAR(50) NOT NULL, `Type` CHAR(20) NOT NULL, `X` SHORT NOT NULL, `Y` SHORT NOT NULL, `Width` SHORT NOT NULL, `Height` SHORT NOT NULL, `Attributes` LONG, `Property` CHAR(72), `Text` CHAR(255), `Control_Next` CHAR(50), `Help` CHAR(50) PRIMARY KEY `Dialog_`, `Control`)' \
        -q 'CREATE TABLE `ControlEvent` (`Dialog_` CHAR(72) NOT NULL, `Control_` CHAR(50) NOT NULL, `Event` CHAR(50) NOT NULL, `Argument` CHAR(255) NOT NULL, `Condition` CHAR(255), `Ordering` SHORT PRIMARY KEY `Dialog_`, `Control_`, `Event`, `Argument`, `Condition`)' \
        -q 'CREATE TABLE `EventMapping` (`Dialog_` CHAR(72) NOT NULL, `Control_` CHAR(50) NOT NULL, `Event` CHAR(50) NOT NULL, `Attribute` CHAR(50) NOT NULL PRIMARY KEY `Dialog_`, `Control_`, `Event`)' \
        -q "$dlg ('WelcomeDlg', 50, 50, 270, 150, 3, '[ProductName] Setup', 'Name', 'Next', 'Cancel')" \
        -q "$dlg ('ProgressDlg', 50, 50, 270, 80, 1, '[ProductName] Setup', 'ActionText', '', '')" \
        -q "$dlg ('ExitDlg', 50, 50, 270, 100, 3, '[ProductName] Setup', 'Finish', 'Finish', 'Finish')" \
        -q "$ctl ('WelcomeDlg', 'Title', 'Text', 15, 10, 240, 15, 3, '', 'Welcome to [ProductName]')" \
        -q "$ctl ('WelcomeDlg', 'Name', 'Edit', 15, 40, 200, 18, 3, 'USERNAME', '{80}')" \
        -q "$ctl ('WelcomeDlg', 'Extra', 'CheckBox', 15, 65, 200, 17, 3, 'WANTEXTRAS', 'Install extras')" \
        -q "$ctl ('WelcomeDlg', 'Next', 'PushButton', 150, 120, 56, 17, 3, '', '&Next')" \
        -q "$ctl ('WelcomeDlg', 'Cancel', 'PushButton', 210, 120, 56, 17, 3, '', 'Cancel')" \
        -q "$ctl ('ProgressDlg', 'ActionText', 'Text', 15, 15, 240, 15, 3, '', '')" \
        -q "$ctl ('ProgressDlg', 'Bar', 'ProgressBar', 15, 40, 240, 10, 1, '', '')" \
        -q "$ctl ('ExitDlg', 'Done', 'Text', 15, 15, 240, 15, 3, '', 'Thanks, [USERNAME]')" \
        -q "$ctl ('ExitDlg', 'Finish', 'PushButton', 210, 70, 56, 17, 3, '', '&Finish')" \
        -q "$ev ('WelcomeDlg', 'Next', 'AddLocal', 'Extras', 'WANTEXTRAS = \"1\"', 1)" \
        -q "$ev ('WelcomeDlg', 'Next', 'EndDialog', 'Return', '1', 2)" \
        -q "$ev ('WelcomeDlg', 'Cancel', 'EndDialog', 'Exit', '1', 1)" \
        -q "$ev ('ExitDlg', 'Finish', 'EndDialog', 'Return', '1', 1)" \
        -q "INSERT INTO \`EventMapping\` (\`Dialog_\`, \`Control_\`, \`Event\`, \`Attribute\`) VALUES ('ProgressDlg', 'ActionText', 'ActionText', 'Text')" \
        -q "INSERT INTO \`EventMapping\` (\`Dialog_\`, \`Control_\`, \`Event\`, \`Attribute\`) VALUES ('ProgressDlg', 'Bar', 'SetProgress', 'Progress')" \
        -q "INSERT INTO \`Property\` (\`Property\`, \`Value\`) VALUES ('USERNAME', 'nobody')" \
        -q "INSERT INTO \`ActionText\` (\`Action\`, \`Description\`) VALUES ('InstallFiles', 'Copying new files')" \
        -q "INSERT INTO \`InstallUISequence\` (\`Action\`, \`Condition\`, \`Sequence\`) VALUES ('WelcomeDlg', 'NOT Installed', 1100)" \
        -q "INSERT INTO \`InstallUISequence\` (\`Action\`, \`Sequence\`) VALUES ('ProgressDlg', 1280)" \
        -q "INSERT INTO \`InstallUISequence\` (\`Action\`, \`Sequence\`) VALUES ('ExitDlg', -1)" || return 1
      return 0
    fi
    [ "$t" != dllsearch ] && return 0
    mkdir -p "$OUT/dllsearch-prefix/windows/system32"
    "${1%gcc}windres" "$ROOT/host/tests/dllsearch.rc" -O coff -o "$OUT/dllsearch-res.o" || return 1
    "$1" -shared -nostdlib -DBUILD_FIXTURE -Wl,--entry,0 "$ROOT/host/tests/dllsearch.c" "$OUT/dllsearch-res.o" -o "$OUT/dllsearch-fixture.dll"
  }
  case "$t" in
    comclass|registrymulti|comrpc|services|capiprovider|dllsearch|msiinstall|msiui|nsisinstall|innoinstall)
      [[ $t = msi* || $t = nsis* || $t = inno* ]] && rm -rf "$OUT/$t-prefix"
      mkdir -p "$OUT/$t-prefix/windows/system32"
      test_env=("SAKE_C=$OUT/$t-prefix")
      ;;
  esac
  # (Inno Setup lays its windows out in dialog units: fonts, SAKE_FONTS or made from the IPA
  # fonts by tools/ci/sake-fonts.py)
  case "$t" in
    innoinstall)
      FONTS=${SAKE_FONTS:-$OUT/fonts}
      if [ ! -d "$FONTS" ] && ! { mkdir -p "$FONTS" && python3 "$ROOT/tools/ci/sake-fonts.py" "$FONTS" > /dev/null; }; then
        echo "FAIL $t: no fonts (SAKE_FONTS, or fonts-ipafont and fonttools for tools/ci/sake-fonts.py)"; fail=1; continue
      fi
      test_env+=("SAKE_FONTS=$FONTS")
      ;;
  esac
  if ! fixture i686-w64-mingw32-gcc; then
    echo "FAIL $t: fixture build"; fail=1; continue
  fi
  # a "libs:" line in the source names its import libraries
  libs=$(sed -n 's/.*libs: *\([-a-zA-Z0-9_ ,=]*\).*/\1/p' "$ROOT/host/tests/$t.c" | head -1)
  # (host/tests/<name>.rc: the test's resources, linked in)
  res=
  if [ -f "$ROOT/host/tests/$t.rc" ] && [ "$t" != dllsearch ]; then
    res=$OUT/$t-res32.o
    (cd "$ROOT/host/tests" && i686-w64-mingw32-windres "$t.rc" -O coff -o "$res") || { echo "FAIL $t: resources"; fail=1; continue; }
  fi
  if ! i686-w64-mingw32-gcc -O2 -mmmx -msse2 -mssse3 "$ROOT/host/tests/$t.c" $res -o "$OUT/$t.exe" -L"$ldir" $libs; then
    echo "FAIL $t: build"; fail=1; continue
  fi
  # (the whole output kept: a process that fails shows its exit status and its last lines, sake's
  # own too, so that a rare failure can be understood from the log of the run it happened in)
  # (EXE: another build of the test; LOG: its own log)
  run() { local log=$OUT/${LOG:-$t}.log; (cd "$(dirname "$SAKE")" && timeout 300 env "${test_env[@]}" "$@" node "$SAKE" "${EXE:-$OUT/$t.exe}" > "$log" 2>&1; s=$?; grep -v '^sake:' "$log"; exit $s); }
  failed_run() { echo "FAIL $t: $1 process failed or timed out (status $2):"; tail -20 "$OUT/${LOG:-$t}.log" | sed 's/^/    /'; }
  jit=$(run IPPAI_JIT=1); st=$?
  if [ $st != 0 ]; then
    failed_run JIT $st
    fail=1
    continue
  fi
  interp=$(run IPPAI_JIT=0); st=$?
  if [ $st != 0 ]; then
    failed_run interpreter $st
    fail=1
    continue
  fi
  want=$ROOT/host/tests/expected/$t.txt
  if [ -n "${UPDATE:-}" ]; then
    echo "$jit" > "$want"
  fi
  if [ "$jit" != "$(cat "$want" 2>/dev/null)" ]; then
    echo "FAIL $t (JIT):"; diff <(echo "$jit") "$want" | head -10; fail=1
  elif [ "$interp" != "$jit" ]; then
    echo "FAIL $t (interpreter):"; diff <(echo "$interp") "$want" | head -10; fail=1
  else
    echo "ok   $t"
  fi
  if [[ "$VEX" == *" $t "* ]]; then
    if ! i686-w64-mingw32-gcc -O2 -mavx "$ROOT/host/tests/$t.c" -o "$OUT/$t-vex.exe" $libs; then
      echo "FAIL $t (VEX): build"; fail=1; continue
    fi
    tv=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t-vex.exe" 2>&1 | grep -v '^sake:') )
    if [ "$tv" != "$(cat "$want")" ]; then
      echo "FAIL $t (VEX):"; diff <(echo "$tv") "$want" | head -10; fail=1
    else
      echo "ok   $t (VEX)"
    fi
  fi
  if [[ "$X64" == *" $t "* ]]; then
    if ! fixture x86_64-w64-mingw32-gcc; then
      echo "FAIL $t (x64): fixture build"; fail=1; continue
    fi
    if ! x86_64-w64-mingw32-gcc -O2 "$ROOT/host/tests/$t.c" -o "$OUT/$t-64.exe" -L"$ldir" $libs; then
      echo "FAIL $t (x64): build"; fail=1; continue
    fi
    t64=$(EXE=$OUT/$t-64.exe LOG=$t-64 run); st=$?
    if [ $st != 0 ]; then
      LOG=$t-64 failed_run x64 $st
      fail=1
    elif [ "$t64" != "$(cat "$want")" ]; then
      echo "FAIL $t (x64):"; diff <(echo "$t64") "$want" | head -10; fail=1
    else
      echo "ok   $t (x64)"
    fi
    # (kura's merge_hot64 at the first hot jump, and reclaim after every load: regions are
    # merged into bigger ones and their own modules go while the program runs)
    tm=$(EXE=$OUT/$t-64.exe LOG=$t-64m run IPPAI_JIT_MERGE_HOPS=1 IPPAI_JIT_RECLAIM=1); st=$?
    if [ $st != 0 ]; then
      LOG=$t-64m failed_run "x64 merging" $st
      fail=1
    elif [ "$tm" != "$(cat "$want")" ]; then
      echo "FAIL $t (x64, merging):"; diff <(echo "$tm") "$want" | head -10; fail=1
    else
      echo "ok   $t (x64, merging)"
    fi
  fi
done
for t in $CLANG64; do
  case $t in
  x64crt) "$ROOT/host/tests/build-x64crt.sh" "$OUT" ;;
  msvcpsync|msvcstream|msvcrtti|msvcseh) "$ROOT/host/tests/build-msvc64.sh" "$t" "$OUT" ;;
  *) clang --target=x86_64-w64-windows-gnu -fms-extensions -fuse-ld=lld -O2 "$ROOT/host/tests/$t.c" -o "$OUT/$t.exe" ;;
  esac || { echo "FAIL $t: build"; fail=1; continue; }
  got=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t.exe" 2>&1 | grep -v '^sake:') )
  want=$ROOT/host/tests/expected/$t.txt
  [ -n "${UPDATE:-}" ] && echo "$got" > "$want"
  if [ "$got" != "$(cat "$want" 2>/dev/null)" ]; then
    echo "FAIL $t:"; diff <(echo "$got") "$want" | head -10; fail=1
  else
    echo "ok   $t"
  fi
done
for t in $CLANG32; do
  mkdir -p "$OUT/32"
  "$ROOT/host/tests/build-msvc32.sh" "$t" "$OUT/32" || { echo "FAIL $t (x86): build"; fail=1; continue; }
  want=$ROOT/host/tests/expected/$t-32.txt
  for jit in 1 0; do
    got=$( (cd "$(dirname "$SAKE")" && timeout 300 env IPPAI_JIT=$jit node "$SAKE" "$OUT/32/$t.exe" 2>&1 | grep -v '^sake:') )
    [ -n "${UPDATE:-}" ] && [ $jit = 1 ] && echo "$got" > "$want"
    if [ "$got" != "$(cat "$want" 2>/dev/null)" ]; then
      echo "FAIL $t (x86, IPPAI_JIT=$jit):"; diff <(echo "$got") "$want" | head -10; fail=1
    else
      echo "ok   $t (x86, IPPAI_JIT=$jit)"
    fi
  done
done
for t in $GXX; do
  want=$ROOT/host/tests/expected/$t.txt
  for arch in i686 x86_64; do
    if ! $arch-w64-mingw32-g++ -O2 -static "$ROOT/host/tests/$t.cpp" -o "$OUT/$t-$arch.exe"; then
      echo "FAIL $t ($arch): build"; fail=1; continue
    fi
    got=$( (cd "$(dirname "$SAKE")" && timeout 300 node "$SAKE" "$OUT/$t-$arch.exe" 2>&1 | grep -v '^sake:') )
    [ -n "${UPDATE:-}" ] && [ $arch = i686 ] && echo "$got" > "$want"
    if [ "$got" != "$(cat "$want" 2>/dev/null)" ]; then
      echo "FAIL $t ($arch):"; diff <(echo "$got") "$want" | head -10; fail=1
    else
      echo "ok   $t ($arch)"
    fi
  done
done
exit $fail
