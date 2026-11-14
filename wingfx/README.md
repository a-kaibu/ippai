# wingfx

Conformance tests for the Windows graphics APIs: GDI, DirectDraw, Direct3D 8, 9 and 11, WIC so
far, then the rest (GDI+, Direct2D, DirectWrite). Written to check any
implementation of them (Wine, an emulator, a translation layer) against real Windows, and to
be usable without a Windows machine.

It stands alone: its own Cargo workspace, no dependency on the repository around it.

## How it works

```
cases/<suite>.txt ──► runner/wingfx.c (executor, a Win32 console program) ──► output
        ▲                    │ on Windows: the reference, recorded once
        │                    │ on Wine / sake / ...: the implementation under test
   src/gen.rs                ▼
 (generators)          src/model/ (executable specification) ──► the output Windows should give
```

* **Cases** are short programs in a line language (SPEC.md): create DIB sections, fill them
  with pseudo-random bytes, set brushes and clipping, BitBlt; create a device, set and read
  states, record and apply state blocks. They are generated from fixed seeds: structured
  coverage (all 256 raster operations on every pixel format, every format pair, every
  documented state, every state block type) plus hundreds of random cases at the edges
  (clipping, bitmap borders, overlap, both row orders, custom color tables, odd sizes).
* **The executor** has the case file compiled in, takes no arguments and needs no files, so it
  runs the same under a console, Wine, or an emulator in a browser page.
* **Windows' answers** are the executor's output on Windows (x86 and x86-64), recorded by CI
  on a Windows runner and kept in git (in masu: `tests/winref/golden/wingfx_<suite>-<arch>/`).
* **The models** (Rust) are written from Microsoft's documentation and corrected only by
  Windows' answers; what the documentation leaves open is `unmodeled` until an answer shows
  it, then recorded as `MEASURED` in the model. `cargo test` checks the models against every
  recorded answer, so the models are as good as Windows for every case they answer, and then
  stand in for Windows: an implementation can be checked against the model on any number of
  freshly generated cases.

## Test methods

| method | where |
|---|---|
| differential against real Windows (x86, x86-64) | recorded answers, `wingfx compare` |
| executable specification from the documentation, checked against Windows | `src/model`, `tests/windows.rs` |
| exhaustive proofs over finite domains (truth table of all 256 ROPs, operand use) | `tests/laws.rs`; Kani harness `src/model/gdi.rs` (`cargo kani`) |
| property-based (metamorphic: clipping = masking, blit = two half blits, BitBlt without source = PatBlt, DSTINVERT twice = identity, brush origin modulo the pattern, color round trips, state blocks restore) | `tests/laws.rs` (proptest) |
| coverage-guided fuzzing of the parser, models and comparison | `fuzz/` (`cargo +nightly fuzz run cases`) |
| mutation testing of the models | `.cargo/mutants.toml` (`cargo mutants`) |
| generated cases, deterministic seeds | `src/gen.rs`, `cases/` (a test keeps them current) |

## Use

```
cargo run -- gen gdi > cases/gdi.txt          # the case files (committed)
runner/build.sh out/                          # mingw-w64: out/wingfx_<suite>-32.exe, -64.exe
wine out/wingfx_gdi-32.exe > wine.txt         # any implementation; on Windows, the reference
cargo run -- model gdi > model.txt            # what Windows prints, by the model
cargo run -- compare windows.txt wine.txt     # case by case, first differing line and pixel
cargo test                                    # laws, models against Windows' answers
```

An adapter is just how the executor is started: natively on Windows, `wine`, or for masu's
sake `node sake.js` (tests/winref/run.sh) and its browser runner for the GPU suites
(tests/winref/run-browser.mjs, which reads `browser.txt`).

## Suites

* `gdi`: PatBlt and BitBlt into DIB sections of 1, 4, 8, 16 (5-5-5 and 5-6-5), 24 and 32 bpp,
  all 256 ternary raster operations, solid / hatch / DIB pattern / null brushes, brush origin,
  IntersectClipRect, format conversion, overlap, borders.
* `stretch`: StretchBlt in the four stretch modes, every 1..12 to 1..12 scale of a row and a
  column, random formats / extents (mirroring) / operations.
* `pixel`: SetPixel / GetPixel (clipping, out of bounds) and GetDIBits / SetDIBits between every
  pair of formats.
* `d3d9`: Direct3D 9 render, texture stage and sampler states (defaults, Set/Get of every
  state), state blocks (D3DSBT_ALL / PIXELSTATE / VERTEXSTATE, recording, Capture, Apply).
* `d3d8`: the same for Direct3D 8 (its own states, sampler states as stage states, state block
  tokens), on software, hardware and mixed vertex processing devices.
* `wic`: IWICFormatConverter between all 23 native pixel formats (every pair, every byte value
  to and from 32bppBGRA, premultiplied alpha, rows ending inside a byte).
* `ddraw`: DirectDraw 7 Blt / BltFast between system-memory surfaces of 8, 16 (555, 565) and 32
  bpp: copies, fills, source and destination color keys and their overrides, mirroring,
  overlap, rectangles at and past the edges.
* `d3d11`: Direct3D 11 blend, depth stencil, rasterizer and sampler state objects: every word of
  each description through its values of interest (every filter value below 0x400), with the
  parts that switch others off on and off; which descriptions are refused, what GetDesc gives
  back, when the runtime hands back the same object; ppState / pDesc NULL.

Each suite's behavior is written down as numbered clauses in `spec/<api>.txt` (documented,
measured on Windows, assumed, open), and `cargo test` checks every clause is reached.

License: MIT OR Apache-2.0.
