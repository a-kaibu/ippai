#!/bin/bash
# Builds the winref probes (probes/*.c) for x86 and x86-64, and host/tests' console checks for
# x86, into $1 (default target/winref/exe). A "libs:" line in a source names its import
# libraries; "arch: 32" builds it for x86 only. Every exe carries ja.manifest: code page 932 as
# the process's ANSI code page (activeCodePage), as on the Japanese Windows games expect; the
# runner itself is en-US.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$ROOT/target/winref/exe}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
(cd "$HERE" && i686-w64-mingw32-windres ja.rc -O coff -o "$OUT/ja32.res" && x86_64-w64-mingw32-windres ja.rc -O coff -o "$OUT/ja64.res")
libs() { sed -n 's/.*libs: *\([-a-zA-Z0-9_ ,=]*\).*/\1/p' "$1" | head -1; }
# probes marked "run: browser" draw through the GPU: sake runs them in Chromium (run-browser.mjs)
: > "$OUT/browser.txt"
for src in "$HERE"/probes/*.c; do
  n=$(basename "$src" .c)
  if grep -q 'run: browser' "$src"; then
    echo "$n-32" >> "$OUT/browser.txt"
    grep -q 'arch: 32' "$src" || echo "$n-64" >> "$OUT/browser.txt"
  fi
  l=$(libs "$src")
  # (a probe with its own resources: probes/<name>.rc, which carries the manifest too)
  r32=$OUT/ja32.res r64=$OUT/ja64.res
  if [ -f "$HERE/probes/$n.rc" ]; then
    r32=$OUT/$n-32.res r64=$OUT/$n-64.res
    (cd "$HERE" && i686-w64-mingw32-windres "probes/$n.rc" -O coff -o "$r32" && x86_64-w64-mingw32-windres "probes/$n.rc" -O coff -o "$r64")
  fi
  i686-w64-mingw32-gcc -O2 -msse2 -Wall -Wno-format -Wno-unused -o "$OUT/$n-32.exe" "$src" "$r32" "$HERE/fpu53.c" $l
  if ! grep -q 'arch: 32' "$src"; then
    x86_64-w64-mingw32-gcc -O2 -Wall -Wno-format -Wno-unused -o "$OUT/$n-64.exe" "$src" "$r64" $l
  fi
done
# host/tests checks that print results (tests/host-tests.sh compares them with expected/*.txt;
# here they are compared with Windows' own output)
for t in changenotify callwndhook xfile mmiobuf nullcall dxversion dircase intops simd atomics smc cmp64 ssecvt textbounds crtstdio fileseek crtfiles struct64 srwlock jitcode sendrace mbshort makepath netbios mbtokens mbsecure recalloc msgwaitstart dialogtab passwordchar winineturl wininetquery aclentries acctnames acmdriver stacksize dlgenter dlgthread editnotify shfileinfo deleteonclose filelock bcryptkeys listbox searchpath resupdate shfolders shelllink sendnotify browsefolder dllversion virtualex flatsb propsheet trackmouse getmsghook memdccaps; do
  src=$ROOT/host/tests/$t.c
  l=$(libs "$src")
  fpu=$HERE/fpu53.c
  [[ "$l" == *-nostdlib* ]] && fpu=
  i686-w64-mingw32-gcc -O2 -mmmx -msse2 -mssse3 -o "$OUT/host_$t-32.exe" "$src" "$OUT/ja32.res" $fpu $l
done
x86_64-w64-mingw32-gcc -O2 -o "$OUT/host_struct64-64.exe" "$ROOT/host/tests/struct64.c" "$OUT/ja64.res" $(libs "$ROOT/host/tests/struct64.c")
x86_64-w64-mingw32-gcc -O2 -o "$OUT/host_msvcpchar-64.exe" "$ROOT/host/tests/msvcpchar.c" "$OUT/ja64.res"
rm "$OUT"/*.res
# D3D9 traces replayed (tests/replay: replay_<trace>-32.exe)
"$ROOT/tests/replay/build.sh" "$OUT"
ls "$OUT" | wc -l
