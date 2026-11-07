# Replaying recorded D3D calls (apitrace)

A game's (or a probe's) Direct3D 8, 9 or 11 calls are recorded on real Windows with
[apitrace](https://github.com/apitrace/apitrace), then the same calls are played back on Windows
and on sake, and what each one drew is compared. The graphics layer is tested on what games
really do, without running the games themselves: both sides play the same recording, so
whatever the recording missed (a surface drawn with GDI through GetDC, say) is missing on both.
32-bit and 64-bit programs both.

RenderDoc does not fit this: it has no D3D8/D3D9 (nor DirectDraw, nor GL's compatibility
profile), which is what most of the games use. It is still useful by hand for D3D11 / GL core
games, or on Chromium's GPU process to look at what sake sends to WebGPU.

## Pieces

- `crates/apitrace`: reads apitrace's `.trace` (snappy chunks, format version 6) and converts a
  trace into a replay stream (`cargo run --release -p apitrace -- [-32|-64] in.trace out.rpl`;
  the width is found from the trace's addresses when not given; `apitrace dump in.trace` prints
  its calls). Calls without effects (Get*, Check*, ...) are left out unless they hand out
  objects; objects become handles (D3D8's DWORD shader handles too); pointed-to structs and
  arrays are packed in the C layout of the trace's width, pointers inside them (strings, bytes,
  objects, the window) fixed up by the replayer; the bytes apitrace saw written into each locked
  or mapped region (its `memcpy` before Unlock / Unmap) are kept per lock, with the recorded
  pitches. The replay is windowed, in its own window, without vsync (D3DPRESENT_PARAMETERS and
  DXGI_SWAP_CHAIN_DESC are changed so).
- `crates/apitrace/src/layouts.txt` (`tests/replay/layouts.py` makes it): every struct of
  mingw-w64's d3d8.h, d3d9.h, d3d11_4.h and dxgi1_6.h with its members' offsets on i686 and
  x86_64 (clang's record layout dump), every interface's methods with their vtable index and
  parameter types, the entry points' parameters. The converter packs by it and names each
  method's vtable index in the stream. `layouts.py --check`: it is current;
  `layouts_test.py`: gcc's own offsets agree.
- `tests/replay/replay.c` (with `d3d9img.c`, `d3d8img.c`, `d3d11img.c`): plays a stream back on
  this system through each method's vtable index; entry points (Direct3DCreate9,
  D3D11CreateDevice, ...) by name from the D3D DLLs. On x86 every D3D argument is one stack word;
  on x64 the call goes through a varargs pointer with every argument a double holding its bits
  (Win64 puts a varargs double in both its XMM and integer register), checked at start
  (`callv`). It prints, in the winref probe format:
  `image f<n>` (the back buffer before the n-th Present), `image r<call>` (D3D9: a
  GetRenderTargetData), `image l<call>` (D3D8: a LockRect for reading), `image m<call>` and
  `m<call>_alpha` (D3D11: a Map for reading, the part last copied into it), `data m<call>` (a
  buffer's first bytes), `image d<call>` (with `-draws`: the render target after every draw),
  `hr <call> <method> <got> want <recorded>`, `unknown <name>`.
  Options: `-every N -from N -to N` (frames), `-tol N` (default 2), `-noimages`.
- `tests/replay/build.sh [dir]`: `replay-32.exe` / `replay-64.exe` (stream on the command line)
  and, for each `traces/<probe>.trace`, `replay_<probe>-<bits>.exe` with the stream inside: a
  winref probe of its own (`tests/winref/build.sh` builds them; they run in the browser on
  sake's side).

## Probes (CI)

`tests/replay/record.txt` lists winref probes (and the API apitrace records: d3d9, d3d8, dxgi)
whose calls `.github/workflows/replay.yml` records on the Windows runner (WARP; apitrace's win32
build for -32 probes, win64 for -64) into `tests/replay/traces/` (committed with `<probe>.src`,
the probe's sources' hash: a changed probe is recorded again). winref then treats
`replay_<probe>-<bits>` as any probe: Windows' answers in `tests/winref/golden`, sake compared
against them. `tests/replay/check.py` (its own job, which nothing waits on) checks the chain itself: the probe's own pictures have to
be found, in order and pixel for pixel, among what the replay on Windows reads back
(`check_test.py` tests that matching); `known.txt` names a probe's pictures the recording cannot
replay (D3D8 CopyRects: apitrace records only its first rect).

## Trial games (CI)

`tests/replay/games.txt` lists trial games (ids of `tests/corpus/titles.tsv`, free downloads).
`.github/workflows/gametrace.yml` (when games.txt changes, or by hand) records each one once on
the Windows runner: the trial downloaded and unpacked (`tests/corpus-prep.sh`), its API and width
found (`gameapi.py`: the D3D DLLs its exe or the DLLs beside it name), the DirectX 9 runtime
installed, and the game played under apitrace as `corpusref.yml` plays it (Japanese fonts, a
virtual sound card, `tests/corpus/winplay.py`: code page 932, through a launcher to the title
screen, the title's steps, then 30 s of story clicks) and ended (`record-game.ps1`). apitrace's
wrapper DLL goes beside the exe with `TRACE_FILE` naming the trace (its injector starts the game
through the ANSI code page, which has no Japanese folder names on the en-US runner). winplay's
pictures are an artifact of the run (what the game got to), never committed. A trace that converts goes to `testdata/apitrace/<id>-<bits>`: zstd, in
parts of at most 90 MiB (GitHub refuses files over 100 MB), with `<id>-<bits>.txt` (api, exe,
seconds, the trace's sha256: `games.py`). The trace ends where the game was killed: the
converter keeps the calls up to the last whole one (`-strict`, which build.sh uses for the
probes' traces, refuses such a trace).

gameapi.py's API is a guess from the DLL names a game carries: a watcher lists the graphics
DLLs the game loaded, and with no trace the game is played again with the API whose system DLL
it did load (`gameapi.py next`: run 42380412096 found suzunone7 naming d3d9 and drawing with
d3d8, and hook, haruka and appetite drawing with DirectDraw only).

A title with no trace goes to `testdata/apitrace/skipped.tsv` with why (`none`: it names no
D3D8/9/11; `ddraw`: it drew with DirectDraw, which the replay does not take; `notrace`: it made none, with how far winplay got; `noconvert`: its trace
does not convert) and is not downloaded again; `again`, or naming it in `only`, tries it anew.
A download that failed is tried again next time.

From there it is a winref probe like the others: `build.sh` builds `replay_<id>-<bits>-<bits>.exe`
(a frame in 900 read back), Windows answers once (its pictures go to `tests/winref/golden` as
every probe's do), sake is checked against them on every push. Nobody runs the games again
unless asked (the workflow's `again`).

## Games of your own (local)

A game that is not a free trial is recorded on a Windows PC and never goes to GitHub:

    apitrace trace --api d3d9 -o game.trace game.exe      (--api d3d8 / dxgi; apitrace's win32
                                                           build for 32-bit games, win64 for 64)

play as far as wanted, quit, then

    cargo run --release -p apitrace -- game.trace game.rpl
    replay-32.exe game.rpl -every 30 > windows.txt        (on the Windows PC: the answers;
                                                           replay-64.exe for a 64-bit game)

and on sake, `replay-32.exe game.rpl -every 30` in the browser (run-browser.mjs, with the stream
next to the exe), `tests/winref/compare.py` on the two outputs.

Not yet: Direct3D 10 and 12, DirectDraw (apitrace records them; the converter refuses nothing but
has no pictures for them), a D3D11 deferred context's command lists (replayed as they come).
