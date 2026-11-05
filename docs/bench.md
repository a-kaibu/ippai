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
| `d3d11` | the same set with SM4 shaders (`dxbc.py`: hand-assembled, sake has no HLSL compiler), UpdateSubresource and Map uploads |
| `d2d` | shapes + DrawText scene, DrawBitmap scene, 30 text lines, 1000 shapes |
| `gl` | VN scene (glBegin), 2000 sprites immediate / vertex arrays, glTexSubImage2D, GLSL crossfade, FBO blur passes |
| `scenario` | `src/scenario.c`: a small D3D9 visual novel played through by `scenario.mjs` (paced, as players play): title, lines with voices, scene transitions (pictures read from a 70 MB archive in no useful order, unfiltered, crossfaded), a movie, a choice, a save; then the save loaded in a new page. Click to the next line p50/p95, transitions, start-up (cold: a new browser context) and restore (warm: the same context), the game's frame intervals p50/p95/p99, the browser's CPU while the game waits, peak memory, chunks fetched |
| `js` | `cpu.c`'s computing cases and `bench.c` as plain wasm in V8 (node) and JavaScriptCore (jsvu's shell): the engines compared, and the cost of the x86 translation against `cpu-game`; `dyn.js`: wasm the way koji makes it (modules compiled at run time, instantiation, a module per block, `call` vs `call_indirect`, first tier vs optimized) |

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

The report (job summary, artifact `bench-report`) lists every metric with the change against the
previous commit; a change is marked when it is over 3% and every round of one build beat every
round of the other. The run fails when the results can't be trusted or something broke: a case
that worked before fails, a checksum differs from the previous build's (or, without one, from the
history's), the JS engines disagree, a group left no results, a case has fewer numbers than its
rounds, or there are none. The artifact `bench-history` carries the medians and checksums of the
last 300 commits from run to run; `VERSION` in `suite.mjs` says how they were measured, and only
the same version is compared with (bump it when the numbers stop being comparable). The report's
計測条件 lists Chromium, node, Emscripten and the JavaScriptCore revision (`JSC_REV` pins one).

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
