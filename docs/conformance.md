# Conformance tests from outside references

Two suites check sake against references it did not write itself. Both run in
`.github/workflows/conformance.yml` (push to main, or by hand) and locally.

## Wine's conformance tests (tests/winetest)

Wine's `dlls/<dll>/tests` are thousands of checks of how Windows behaves, each written
against real Windows (Wine marks the ones Wine itself fails with `todo_wine`). sake is not
Wine (it has no `wine_server_call`), so the tests expect Windows' results everywhere.

- `tests/winetest/build.sh` downloads the Wine release it pins (version + sha256), configures
  it for the PE side only and builds `<dll>_test.exe` for the DLLs in `tests/winetest/dlls.txt`
  (i386 and x86_64) with mingw-w64, exactly as Wine's own build does. Nothing of Wine is in
  this repository (Wine is LGPL); CI caches the built programs per Wine version and dll list.
  ~10 min on 4 cores, needs `mingw-w64 flex bison`.
- `tests/winetest/run.py` runs every test of every program (`<dll>_test.exe <test>`) as its own
  sake process (node build) with a fresh `C:\` and a time limit, and sorts the results into
  ok / fail (N failed checks) / crash (no summary line: an unhandled exception or a sake
  panic) / timeout. ~13 min for the 394 i386 tests on 4 cores.
- `tests/winetest/expected-i386.tsv` and `expected-x86_64.tsv` hold the current results (378
  tests each, 5.4M / 5.7M checks run, 2026-11-06; x86_64 runs on sake's own x64 CRT). A run fails on a regression
  (a test that got worse, or more failed checks); `--update` records improvements.
- `target/winetest/report-<arch>.md` has per-dll totals, the unimplemented functions the
  tests called (by how many tests), and the first failure messages of each test.

- The dlls in `tests/winetest/browser.txt` (ddraw, d3d8/9/10/11, dxgi, d2d1, opengl32) need a GPU,
  which the node build lacks: `run.py --browser` runs them in headless Chromium (WebGPU)
  through play.html (`tests/winetest/run-browser.mjs`: the programs packed into a scratch
  store, served by `ippai serve`, one browser context per test, the program's console lines
  parsed as for node). Results in `expected-i386-browser.tsv`; CI job `winetest-gpu`. Needs
  the web build with sake in it (`web/dist`), `target/release/ippai` and Playwright.

Two switches make the tests more useful on sake: `SAKE_PROC_STUBS=1` makes GetProcAddress find
functions sake knows from the DLL specs but does not implement (their stubs return 0), as
Windows would, so a test fails on them instead of crashing on a NULL call (games keep the
default: NULL, so they take their fallback); `SAKE_NO_EXEC=1` refuses CreateProcess instead of
replacing the process (tests start copies of themselves as children).

```
tests/winetest/build.sh                      # or: build.sh kernel32 gdi32
SAKE=target/sake-out/sake.js tests/winetest/run.py -j 4           # all
tests/winetest/run.py gdi32:bitmap kernel32  # a test, a dll
tests/winetest/run.py --update               # record the new results
PLAYWRIGHT=.../playwright/index.js WEB=web/dist FONTS=<Fonts dir> tests/winetest/run.py --browser -j 3
```

## Differential GDI test (tests/diff)

`tests/diff/gdidiff.c` draws random operations (SetPixel, PatBlt with its ROPs, rectangles,
ellipses, polygons, lines with every ROP2 and pen style, FillRect/FrameRect/InvertRect, BitBlt
with any ROP3, StretchBlt in every mode, AlphaBlend, TransparentBlt, StretchDIBits) into 32, 24,
16 and 8 bpp DIB sections, each on a fresh pseudo-random canvas, and prints a hash of the
canvas after each one. `tests/diff/gdi.py` runs it on Wine (whose DIB engine is checked against
Windows hashes in Wine's own gdi32 tests) and on sake, and compares line by line;
`tests/diff/gdi-expected.txt` lists the operations known to differ, so CI fails on a new
difference. `gdidiff.exe <seed> <n> dump <i>` prints the pixels of operation i.

Known differences (2026-11-06): hatch brushes (drawn solid), ellipses, round rectangles,
polygon fill and wide/styled pens (sake draws outlines with 1-pixel solid lines), AlphaBlend
rounding, StretchBlt/StretchDIBits stretching and mirroring rules, TransparentBlt stretching,
the null brush in ROP3 blits.

## Differential floating point test (tests/diff/fpudiff.c, fpu.py)

The boundary operands TestFloat would pick (zeros, the subnormal and normal limits, one and its
neighbours, the largest finite, infinities, quiet and signalling NaNs, the int32 limits) in
every pair, plus random operands, through SSE/SSE2 scalar arithmetic, min/max, sqrt,
conversions and comparisons in all four MXCSR rounding modes and with FTZ+DAZ, and through x87
arithmetic, fsqrt/frndint/fist/fcom/fprem/fscale/transcendentals under six control words
(PC53 near is Windows' default, PC24 is what Direct3D 9 sets). The reference is the CPU itself:
the program runs natively under Wine. sake runs it with the JIT and with the interpreter.
Differences are counted per operation as value (what the program sees), ext80 (only the 80-bit
register: koji's x87 is f64 by design) and flags (only the exception flags);
`tests/diff/fpu-expected.tsv` has the counts. x87 transcendental results differ between CPU
vendors in their last bits, so a runner with another CPU can move those counts.

Found (2026-11-06), not fixed yet: FTZ/DAZ are ignored; no exception flags are raised anywhere (MXCSR, x87 status); NaN
operand rules (the first operand's NaN for SSE, sNaN quieting on fld, the negative default NaN
of fsqrt of a negative number), fsin/fcos/fptan of |x| >= 2^63 (x86 leaves them and sets C2),
f2xm1 of zeros and tiny values, fscale and fprem special cases. Fixed (2026-11-07, koji::round):
the MXCSR rounding mode for SSE arithmetic and conversions, and the x87 rounding and precision
controls (PC24 at the x87's exponent range; products and quotients below f64's normal range
rounded twice, as the FPU's register and then its store do), in the interpreter and both JITs
(translated code checks the mode and calls the `round` import when it isn't round to nearest).
