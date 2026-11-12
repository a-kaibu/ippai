# Benchmarks

`tests/bench` measures ippai on every commit to main (`.github/workflows/bench.yml`, after CI).
Each group runs on its own GitHub runner; the commit's build and the previous commit's build
(CI's `ippai` artifacts) run side by side, alternating round by round, so the comparison does not
depend on how fast that runner happens to be. Every number is milliseconds, lower is better
(per frame for the graphics cases).

| group | what |
|---|---|
| `cpu-kernels` | `host/tests/bench.c` (crc32, lz, blend, memcpy, sort, float, calls); start-up of a program |
| `cpu-game` | `src/cpu.c`: a scenario bytecode VM, Huffman decoding, PNG unfiltering, YUV to RGB, audio mixing, software composition (C and SSE2), x87 transforms, heaps, strings and Shift_JIS, cheap Win32 calls, locks and event ping-pong, threads, file reads and mappings; `src/cxx.cpp`: map, string, vector/sort, virtual calls, exceptions, unordered_map |
| `gdi` | software-composed VN scene + BitBlt, dirty rectangles, AlphaBlend, StretchDIBits, text, small blits |
| `ddraw` | VN scene (BltFast, color keys, Lock, GetDC text), full-frame Lock, 200 color-keyed sprites, exclusive-mode flip chain |
| `d3d8` | VN scene with text texture, 2000 DrawPrimitiveUP sprites, movie upload |
| `d3d9` | VN scene, ps_2_0 crossfade, 2000 sprites batched / one call each, render-target blur passes, movie upload, state changes |
| `d3d11` | the same set with SM4 shaders (HLSL compiled at start by D3DCompile), UpdateSubresource and Map uploads |
| `d2d` | shapes + DrawText scene, DrawBitmap scene, 30 text lines, 1000 shapes |
| `gl` | VN scene (glBegin), 2000 sprites immediate / vertex arrays, glTexSubImage2D, GLSL crossfade, FBO blur passes |
| `scenario` | `src/scenario.c`: a small D3D9 visual novel played through by `scenario.mjs` (paced, as players play): title, lines with voices, scene transitions (pictures read from a 70 MB archive in no useful order, unfiltered, crossfaded), a movie, a choice, a save; then the save loaded in a new page. Click to the next line p50/p95, transitions, start-ups by what is already there (`start.nothing`: no saved translations (`&jit=0`) in a new browser context, the first start ever; `cold.startup`: saved translations, a new context; `start.chunks_kept`: the store's chunks in Cache Storage but the HTTP cache emptied; `start.warm`: everything kept) and restore (the same context), the game's frame intervals p50/p95/p99, the browser's CPU while the game waits (`idle_cpu`, and by process and thread: `idle_cpu.workers` the game's workers, `.renderer_main`, `.renderer_other`, `.gpu` the GPU process (Dawn and SwiftShader), `.browser`, `.other`; the busiest threads are in the job's log), peak memory, chunks fetched |
| `kirikiri` | the same played on a small game made with 吉里吉里Z 1.4.0r2 and KAG3 (`testdata/kirikiri`, packed into data.xp3 by `pack-kirikiri.mjs`; the engine fetched pinned by `fetch-krkrz.sh`): TJS and KAG scripts, PNG pictures, WAV voices and BGM, links for the choice, KAG's save (with a thumbnail) and load, and effects chosen to be heavy, each its own `fx_*` metric: extrans transitions (ripple, wave, rotatezoom, mosaic), a slide-in with a screen quake, three characters crossfaded at once, edged text with ruby typed out, a full-screen box blur and sepia, and 48 petals drawn with operateAffine over the screen every frame (`kk_petals_p50/p95`, the game's frame intervals meanwhile). Its `Debug.message` lines (krkrz prints them to stdout) are the harness's states; at 9 still screens (title, scenes after transitions, the three characters, ruby text, the blur, the choice, the restored save) the game says `BENCH-SHOT <name>` and holds still: `shots.mjs` takes the picture shown (kura's vnhost_render_shot) until two in a row are alike and compares a 32x18 grid of mean colours with `testdata/kirikiri/expected.json` (a few cells may differ; a wrong or blank screen fails the case). `BENCH_SHOTS=DIR` saves the pictures, `BENCH_SHOTS_UPDATE=1` rewrites the expected ones |
| `krkr2` | the same game on 吉里吉里2 2.32r2 (`fetch-krkr2.sh`, Vector's release pinned by its hash): the engine most released KiriKiri games use, DirectDraw/GDI, its own TJS2. Scripts go in UTF-16 (it reads them as Shift_JIS otherwise); it writes `Debug.message` only to its log file (`Debug.startLogToFile`), which sake echoes to the console (`SAKE_ECHO_FILE=krkr.console.log`) |
| `renpy-gl`, `renpy-angle` | the same story on Ren'Py 8.5.3 (`testdata/renpy`, compiled and packed into an RPA archive by `pack-renpy.py`; the SDK fetched pinned by `fetch-renpy.sh`): a 64-bit program, Python 3, SDL2, ffmpeg (WAV, a VP9 movie: `fx_movie`), GLSL (blur, sepia, a ripple shader of its own, a pixellate, an ImageDissolve wipe, a rotozoom), petals as a particle system (`rp_petals_p50/p95`), through OpenGL or ANGLE's Direct3D 11. It runs on sake's own x64 C runtime (no ucrtbase.dll of Wine's) |
| `onscripter`, `onscripter64` | the same story on ONScripter 20230825 (`testdata/onscripter/0.txt`, an NScripter script), built from pinned sources with SDL 1.2 (GDI), SDL_image/mixer/ttf and FreeType by `build-onscripter.sh` for i686 and x64 (the x64 one on sake's own msvcrt), patched with a `benchlog` command for the states: its own sprite compositing, rule-image, mosaic and breakup.dll effects, `monocro`, ruby text, a sprite-button choice, `ns_petals_frame` (48 sprites moved and redrawn per frame, the mean) |
| `win16` | the same story on a 16-bit Windows 3.1 program (`testdata/novel16`, Open Watcom, built by `build-novel16.sh`; the pictures as 256-colour BMPs by `pack-novel16.py`): GDI only, StretchDIBits, masked sprites (SRCAND + SRCPAINT), a dissolve of pattern-brush raster operations, a wipe, blinds and a StretchBlt mosaic, Shift_JIS TextOut, sndPlaySound voices, `w16_petals_frame`. Its states go to DOS handle 1 |
| `win16-pascal` | the same script on a second Win16 engine of the other kind (`testdata/novel16p`, Free Pascal's i8086-win16 compiler built from a pinned commit by `build-novel16p.sh`; one shared palette, `pack-novel16p.py`): it composes its 640x360 256-colour screen itself in 16-bit code, in six bands under 64 KB (sprites with a transparent index, a text window darkened through a colour table, an ordered-dither dissolve, wipe, blinds and mosaic pixel by pixel) and shows each frame as six StretchDIBits; `w16p_petals_frame` |
| `js` | `cpu.c`'s computing cases and `bench.c` as plain wasm in V8 (node) and JavaScriptCore (WebKitGTK's nightly shell, pinned by revision + sha256): the engines compared, and the cost of the x86 translation against `cpu-game`; `dyn.js`: wasm the way koji makes it (modules compiled at run time, instantiation, a module per block, `call` vs `call_indirect`, first tier vs optimized) |

The scenes are 1280x720 and drawn from procedural pictures (`src/bench.h`): no game files.
Frames run unpaced (`IPPAI_NOPACE`, Chromium without vsync); the WebGPU device on the runners is
SwiftShader, so the graphics numbers are mostly CPU: translating the calls, the page's work, and
the software rasterizer.

Every graphics case reads back what it showed (GetDIBits of the window, a Lock of the primary
surface, GetRenderTargetData / CopyRects, a staging copy, glReadPixels): the last warm-up frame's
hash is a checksum (`<case>.frame`), a frame of one color fails the case, and the timed frames
end with a read back, so the GPU's queue is done when the clock stops. The pages say
`BENCH-HOST nopace` when they run unpaced (`IPPAI_NOPACE`); a previous build that doesn't is left
out of the comparison (its numbers would be its 60 fps pacing), and this build not saying it
fails the run.

Where the device has `timestamp-query` (Chromium runs with `--enable-webgpu-developer-features`,
so the times aren't rounded to 100 µs), every render pass writes its start and end
(`host/vngpu.js`) and kura sums them (`vnhost_gpu_total`): `<case>.gpu` is the GPU time per timed
frame (`bench_frames` prints `BENCH-MARK` and `BENCH-FRAMES` around them and pauses 200 ms at
each, so the times of other frames stay out), and the scenario's `gpu_busy` the GPU time per
second of play. On SwiftShader it is the software rasterizer's time.

The report (job summary, artifact `bench-report`) lists every metric with the change against the
previous commit; a change is marked when it is over 3% and every round of one build beat every
round of the other. The run fails when the results can't be trusted or something broke: a case
that worked before fails, a checksum differs from the previous build's (or, without one, from the
history's), the JS engines disagree, a group left no results, a case has fewer numbers than its
rounds, or there are none. The artifact `bench-history` carries the medians and checksums of the
last 300 commits from run to run; `VERSION` in `suite.mjs` says how they were measured, and only
the same version is compared with (bump it when the numbers stop being comparable). The report's
計測条件 lists Chromium, node, Emscripten and the JavaScriptCore revision (pinned by `IPPAI_JSC_REV` + `IPPAI_JSC_SHA256` in `js.sh`).

The JavaScriptCore shell is Safari's engine on Linux, not iPhone Safari: no iOS memory limits,
other JIT settings, another CPU.

Locally (a build in `web/dist` and `target/release/ippai`, fonts from `tools/ci/sake-fonts.py`):

    mkdir -p /tmp/new && cp target/release/ippai /tmp/new && cp -r web/dist /tmp/new/web
    NEW=/tmp/new [OLD=/tmp/old] ROUNDS=1 PLAYWRIGHT=... tests/bench/ci.sh out.json FONTS d3d9 gdi
    python3 tests/bench/report.py DIR_OF_OUT_JSON --sha local

`ONLY="d3d9.exe:fade"` runs one case; `SCALE=2` doubles the work per case.

## Interpreter operand scan (2026-11-06)

`cargo build --release -p koji --example ibench`; compare the original and modified
executables on the same machine, six runs each, alternating their order each round.
Each run executes 20 million instructions per mode. Local native results (median):

| mode | before (ns/instruction) | after | change |
|---|---:|---:|---:|
| decode cache | 20.3 | 20.0 | -1.5% |
| no cache | 44.55 | 44.65 | +0.2% |

The interpreter now scans operand kinds only for LOCK-prefixed instructions and XCHG,
which are the only instructions needing its atomic-memory path. The scan previously ran
for every instruction. Atomic-memory handling itself is unchanged.
This is a small microbenchmark improvement; native uncached execution is effectively
unchanged. These numbers do not establish a browser or whole-game speedup.
Validation: `cargo test -p koji --release`.

## D3D9 draw preparation (2026-11-12)

Draw preparation reuses two pairs of uniform buffers, places fixed-function temporary
registers on the stack, and fingerprints input blocks eight bytes at a time instead
of multiplying once per byte. Uniform equality and GPU batch-generation checks remain
in place. The fingerprint is internal and nonpersistent; no file/cache migration is needed.

Comparison: runtime before this change (`b930f93f`) versus the candidate, Chromium
156.0.8078.79 / Node 26.13.1 on Linux. Both use the same web app and benchmark PE.
`tests/bench/run.mjs`, `ROUNDS=3 SCALE=0.5`, one unmeasured warm-up round, alternating
build order, separate saved-JIT directories, unpaced frames. Median ms/frame:

| case | before | after |
|---|---:|---:|
| VN scene | 1.060 | 0.896 |
| fade | 25.322 | 25.182 |
| 2,000 sprites, one call | 91.281 | 90.636 |
| 2,000 sprites, separate calls | 135.705 | 133.738 |
| render-target blur | 238.025 | 235.198 |
| texture upload | 26.785 | 27.074 |
| 500 draws with state changes | 77.033 | 73.689 |

All seven frame checksums match, with no errors, unstable checksums or excluded builds.
The ranges overlap: these large-screen results do not establish a general FPS improvement.
In particular, the median VN improvement is noisy and must not be advertised as a 15% game
speedup. Rasterization dominates most of these cases on this software GPU.

`tests/bench/d3d9-submit.c` uses 8x8 targets to reduce rasterization cost: 12,004 draws
per round, including opaque/alpha/color-mask changes, two target formats and synchronous
pixel readback. Compile with an i686 Windows GCC (`-O2 -ld3d9 -lgdi32`) to
`d3d9-submit-32.exe`; put `d3d9-submit-32` in the executable directory's `browser.txt`.
Run it with `tests/winref/run-browser.mjs EXE_DIR OUT_DIR`, setting `IPPAI`, `WEB` and
`PLAYWRIGHT` as for other browser probes. Compare the non-timing output exactly and
discard round zero of each process before comparing the remaining five timings.

The submission experiment ran six blocks with all six permutations of before/after/control
order; control is the identical old runtime. Each process ran six rounds, its first discarded
(30 measured samples per label). Overall medians: before **715.5 ms**, after **674.5 ms**
(-5.7%), old-runtime control **699.5 ms** (-2.2%). Median within each process, in block order:

| build | block 1 | 2 | 3 | 4 | 5 | 6 |
|---|---:|---:|---:|---:|---:|---:|
| before | 716 | 764 | 713 | 733 | 697 | 696 |
| after | 650 | 644 | 650 | 651 | 685 | 748 |
| old control | 697 | 782 | 681 | 665 | 863 | 697 |

All 18 processes returned zero and identical non-timing output. The last block regressed;
this is a modest, noisy submission-benchmark gain, not a guaranteed percentage on games.

Validation also includes 38 native D3D9 tests and four Windows PE probes (`d3d9`,
`d3d9rtfmt`, `d3d9sb`, `d3d9state`): all 1,853 output lines match the previous runtime
with fixed-memory JIT, interpreter and growable-memory JIT. Android device FPS has
not been measured.
