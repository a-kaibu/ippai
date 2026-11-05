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
| `js` | `cpu.c`'s computing cases and `bench.c` as plain wasm in V8 (node) and JavaScriptCore (jsvu's shell): the engines compared, and the cost of the x86 translation against `cpu-game` |

The scenes are 1280x720 and drawn from procedural pictures (`src/bench.h`): no game files.
Frames run unpaced (`VNVM_NOPACE`, Chromium without vsync); the WebGPU device on the runners is
SwiftShader, so the graphics numbers are mostly CPU: translating the calls, the page's work, and
the software rasterizer.

The report (job summary, artifact `bench-report`) lists every metric with the change against the
previous commit; a change is marked when it is over 3% and every round of one build beat every
round of the other. Cases that stop working fail the run; programs print checksums of what they
computed, and a checksum that changes between the builds is listed too. The artifact
`bench-history` carries the medians of the last 300 commits from run to run.

Locally (a build in `web/dist` and `target/release/ippai`, fonts from `tools/ci/sake-fonts.py`):

    mkdir -p /tmp/new && cp target/release/ippai /tmp/new && cp -r web/dist /tmp/new/web
    NEW=/tmp/new [OLD=/tmp/old] ROUNDS=1 PLAYWRIGHT=... tests/bench/ci.sh out.json FONTS d3d9 gdi
    python3 tests/bench/report.py DIR_OF_OUT_JSON --sha local

`ONLY="d3d9.exe:fade"` runs one case; `SCALE=2` doubles the work per case.
