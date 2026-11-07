# Replaying recorded D3D9 calls (apitrace)

A game's (or a probe's) Direct3D 9 calls are recorded on real Windows with
[apitrace](https://github.com/apitrace/apitrace), then the same calls are played back on Windows
and on sake, and what each one drew is compared. The graphics layer is tested on what games
really do, without running the games themselves: both sides play the same recording, so
whatever the recording missed (a surface drawn with GDI through GetDC, say) is missing on both.

RenderDoc does not fit this: it has no D3D8/D3D9 (nor DirectDraw, nor GL's compatibility
profile), which is what most of the games use. It is still useful by hand for D3D11 / GL core
games, or on Chromium's GPU process to look at what sake sends to WebGPU.

## Pieces

- `crates/apitrace`: reads apitrace's `.trace` (snappy chunks, format version 6) and converts a
  32-bit D3D9 trace into a replay stream (`cargo run --release -p apitrace -- in.trace out.rpl`).
  Calls without effects (Get*, Check*, ...) are left out; objects become handles; pointed-to
  structs and arrays are packed as the 32-bit C layout; the bytes apitrace saw written into each
  locked region (its `memcpy` before Unlock) are kept per lock, with the recorded pitch.
- `tests/replay/d3d9replay.c`: plays a stream back on this system's D3D9 through each method's
  vtable index (`methods.sh`, from mingw-w64's d3d9.h; on x86 every D3D9 argument is one word),
  in its own window (windowed, at the recorded back buffer size, no vsync). It prints, in the
  winref probe format: `image f<n>` (the back buffer before the n-th Present), `image r<call>`
  (a GetRenderTargetData of the recording), `image d<call>` (with `-draws`: the render target
  after every draw), `hr <call> <method> <got> want <recorded>`, `unknown <name>`.
  Options: `-every N -from N -to N` (frames), `-tol N` (default 2), `-noimages`.
- `tests/replay/build.sh [dir]`: `d3d9replay.exe` (stream on the command line) and, for each
  `traces/<probe>.trace`, `replay_<probe>-32.exe` with the stream inside: a winref probe of its
  own (`tests/winref/build.sh` builds them; they run in the browser on sake's side).

## Probes (CI)

`tests/replay/record.txt` lists winref probes whose calls `.github/workflows/replay.yml` records
on the Windows runner (WARP) into `tests/replay/traces/` (committed with `<probe>.src`, the
probe's sources' hash: a changed probe is recorded again). winref then treats
`replay_<probe>-32` as any probe: Windows' answers in `tests/winref/golden`, sake compared
against them. `tests/replay/check.py` checks the chain itself: the replay on Windows must read
back exactly the probe's own pictures.

## Games (local only)

Games' traces hold the games' pictures: they never go to GitHub. Record on a Windows PC:

    apitrace trace --api d3d9 -o game.trace game.exe      (apitrace's win32 build for 32-bit games)

play as far as wanted, quit, then

    cargo run --release -p apitrace -- game.trace game.rpl
    d3d9replay.exe game.rpl -every 30 > windows.txt        (on the Windows PC: the answers)

and on sake, `d3d9replay.exe game.rpl -every 30` in the browser (run-browser.mjs, with the stream
next to the exe), `tests/winref/compare.py` on the two outputs.

Not yet: 64-bit traces (x64 passes floats in registers: the word-per-argument call does not hold),
D3D8 (apitrace records it; the converter and the method table are D3D9 only), Direct3D 11.
