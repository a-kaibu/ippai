# Conformance tests from outside references

Suites that check sake and koji against references they did not write themselves. Wine's tests
and the differential tests run in `.github/workflows/conformance.yml` (push to main, or by
hand); TestFloat and SingleStepTests in ci.yml (`test`, `jit16`, `jit32`). All run locally too.

## Issue-backed winref quarantine

`tests/winref/quarantine.tsv` lists existing Windows compatibility gaps temporarily excluded
from the comparison gate, with a tracking issue and reason for each probe or replay family.
This was explicitly approved while optimizing CI; it is not a claim of Windows compatibility.
The probes still execute and their outputs and image differences remain in the artifacts.
Crashes, timeouts, missing/empty output, and new unrelated probes still fail. A quarantined
probe that matches Windows is reported as passing, not skipped; remove its entry when fixed.

CI and `tests/winref/check.sh` pass `--quarantine tests/winref/quarantine.tsv` to `compare.py`.
Omit that option for an unquarantined comparison when investigating an issue.

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

## Berkeley TestFloat (tests/testfloat)

[TestFloat 3e](http://www.jhauser.us/arithmetic/TestFloat.html)'s `testfloat_gen`, built on
SoftFloat 3e specialized for x86 SSE (`8086-SSE`: x86's default NaN, NaN propagation and
integer-overflow results), writes test cases for an operation and rounding mode (operands, the
correctly rounded result, the IEEE exception flags), made to hit the corners: ties, carries out
of the significand, the exponent range's boundaries, subnormals, infinities, NaNs, integer
limits. `tests/testfloat/build.sh` builds it from pinned commits into `target/testfloat`
(nothing of it is in this repository; CI caches the build per script hash).
`tests/testfloat/ops.rs` maps ~70 x86 instructions to TestFloat functions: SSE scalar,
packed and VEX arithmetic, FMA (vfmadd132ss/sd), every conversion (cvt*, cvtt*), ROUNDSS/SD
(immediate and MXCSR modes), CMPSS/SD and VCMPSS predicates, (U)COMISS/SD; x87 arithmetic at
single and double precision, m32 operands, FLD/FST conversions, FILD/FIST/FISTP/FISTTP,
FRNDINT, FPREM1 and FUCOMIP/FCOMIP.

- `crates/koji/tests/testfloat.rs` runs every case (level 1: 6.6 M) through the interpreter in
  each rounding mode (MXCSR.RC or the x87 control word) and compares the result and MXCSR's
  exception flags; `tests/testfloat/expected.tsv` has the passing count per instruction and
  mode (all of them, 2026-11-07). `TF_FILTER`, `TF_SHOW=<n>`, `TF_LEVEL=2`, `TF_UPDATE=1`.
- `tools/jit32test testfloat` runs a sample of each instruction's cases (TF_N per mode) as
  translated loops (several cases per program; the 32-bit JIT's modes), against TestFloat.
- Not compared: the x87's exception flags (koji keeps none), and two x87 behaviours SoftFloat
  doesn't model: double rounding below the normal range (the FPU's 64-bit register, then the
  store), and which of two NaN operands an x87 operation returns. FMA's 0 * inf + NaN returns
  the NaN on x86 (diff_isa checks this against the CPU), not SoftFloat's default NaN.

Found and fixed (2026-11-07): overflow and underflow flags under directed rounding (x86 tests
both after rounding: tininess on the value rounded with an unbounded exponent, so a result
rounded toward zero just past MAX does not overflow), sums near f64::MAX (TwoSum's error term
overflowed: Fast2Sum), FMA ignoring MXCSR.RC (koji::round::fma32/fma64), FILD m64 stored to
m32/m64/m80 (rounded once from the exact integer; the JIT gives a 64-bit integer f64 can't hold
to the interpreter), FLD of an f32/f64 SNaN and FRNDINT of one (quieted; in the JIT too, #65: jit32test's sweep
runs FLD m64/m32 straight to FSTP over SNaNs).
`crates/koji/tests/fp_regress.rs` keeps one case of each, and `diff_isa` now runs every case in
a random MXCSR rounding mode against the host CPU.

## SingleStepTests (tests/sst)

Single-instruction tests recorded on real 80386EX, 80286, 8086, 8088 and NEC V20 chips, all in
real mode (9 M tests run): `crates/koji/tests/singlestep.rs` (the interpreter, every test) and
`tools/jit16test sst` (the 16-bit JIT, what it translates). What is run, what is not and the
known differences: tests/sst/README.md.

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
(x87: translated code does the plain wasm op and checks the mode, loaded once per block; outside
PC53 and round to nearest, or for products/quotients below f64's normal range, an inline
call-free path computes the exact error (TwoSum, Dekker's product, the division residual) and
regrids the result; values near the range edges go to the interpreter. SSE: translated code
runs only while MXCSR.RC is to nearest (koji::jit::sse_ok); a step that sets another mode leaves
the region and the interpreter runs on.) `jit32test` sweeps 38 FP instruction sequences over
crafted operands (ties at 24/53 bits, zero sums, odd products, range edges, specials) in every
x87 PC/RC and MXCSR.RC, with FLDCW/LDMXCSR inside the loop, JIT against the interpreter.

MXCSR exception flags (2026-11-07): the interpreter sets them as the hardware does; translated
code keeps them (koji::jit::mxguard: operands that may raise one go to the interpreter) only
from the program's first read of them on: its first STMXCSR or FXSAVE, or the CRT's
_statusfp/_clearfp, turn koji::jit::MX_EXACT on (an STMXCSR whose value the next two
instructions load and mask to the control bits is no read: koji::jit::control_read, MSVC's math
functions checking for the default modes, 449 of the 600 STMXCSR in 27 corpus executables, and
what made 8 of them switch at startup) and kura drops every translation (also the
shared and saved ones, which are kept per mode). Before that read SSE float loops run without
the checks (2-3x faster), and what they raised is missing from MXCSR: a program that reads the
flags once without clearing them first may see fewer. IPPAI_MX_EXACT=1 keeps them from the
start. Tested: jit32test/jit64test run every program both ways (random clears of the flags,
STMXCSR-AND-LDMXCSR, inside loops; fixed programs that read them after and inside a hot loop,
also through FXSAVE), comparing the flags in full once the program read them; the fuzz target
both ways; koji's tests/mx_lazy.rs the switch itself; the winref probe fpflags (a hot loop
before any read, then the flags of each kind read through STMXCSR, _statusfp and _clearfp),
against Windows.

## Trial games against Windows (tests/corpus/golden)

The corpus (tests/corpus/titles.tsv, `.github/workflows/corpus.yml`) used to grade a title only
by how far it got on sake (title screen, story). Windows' answers make it grade the screens too:
`.github/workflows/corpusref.yml` plays each title on a windows-2025 runner the way
tests/corpus.mjs plays it on sake (tests/corpus/winplay.py: the same title-screen rule, the
title's steps, story clicks; DirectX 9 runtime, a virtual sound card (Scream: without one
DirectSound fails and games stop at an error box), Japan's time zone, code page 932 through an
external manifest with activeCodePage ja-JP, the display at 1920x1080 at most) and commits the title
screen and the screen the steps lead to, halved, to tests/corpus/golden/<id>/ with result.json.
Each corpus run compares sake's title.png / steps.png with them (tests/corpus/shots.py: 32x24
cells of area averages; a cell differs by more than 24 in a channel; same screen when the mean
difference is below 6 and at most 4% of the cells differ) and grades the `look` column: none <
title < steps. Falling below it is a regression, as for `expect`; `check.py --update` raises it.

    python3 tests/corpus/golden.py todo tests/corpus/titles.tsv --set full   (titles without an answer)
    python3 tests/corpus/golden.py look ID OUT                              (one title, after a local run)
    python3 tests/corpus/shots.py compare golden.png sake.png

D3D8/9/11 titles are also recorded with apitrace (tests/replay/games.txt, docs/replay.md) and
replayed frame by frame; the screen comparison is what covers the titles drawn with GDI,
DirectDraw or OpenGL, and the game's own code (CPU, Win32, files, sound, scripts) that a replay
never runs.
