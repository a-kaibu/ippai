// The benchmark suite: groups of cases, one GitHub Actions job per group (.github/workflows/bench.yml).
// Each case is one program run in its own page (tests/bench/run.mjs): `exe` from the bench tree
// (tests/bench/build.sh), `args` its command line. Programs print "BENCH <metric> <ms>" lines
// (lower is better); bench.exe (host/tests/bench.c) prints its own "<kernel> <ms> ms <crc>" lines
// (format "plain"). `wall` adds the page's time from load to the program's end as a metric.
//
// Keep metric names stable: the history and the comparison with the previous commit go by them.

// What the numbers mean: bump it when a change to the programs or the harness makes the numbers
// of before incomparable (the report then compares only with the history of the same version).
//   2: frames read back at the end of the timed ones; MASU_NOPACE (was VNVM_NOPACE)
//   3: scenario: starts by what is kept (start.*), idle_cpu by process; kirikiri: still screens
//      for the checkpoint pictures (2.5 s each)
export const VERSION = 3;

// More rounds for the groups whose cases are short and noisy (run.mjs's default when ROUNDS isn't set)
export const MIN_ROUNDS = { wic: 5, "memory-grow": 5, "cpu-kernels": 7, "cpu-game": 5, scenario: 5, kirikiri: 5, krkr2: 5, "renpy-gl": 5, "renpy-angle": 5, onscripter: 5, onscripter64: 5, win16: 5, "win16-pascal": 5 };

const cases = (exe, names, extra = {}) => names.map((c) => ({ exe, args: c, ...extra }));

export const GROUPS = {
  // the x86 JIT and the Win32 layer without graphics: simple kernels, then game-like work
  "cpu-kernels": [
    { exe: "bench.exe", args: "2", format: "plain" },
    { exe: "nop.exe", args: "", wall: "startup" },
  ],
  "cpu-game": [
    ...cases("cpu.exe", ["script", "huffman", "unfilter", "yuv", "mix", "compose", "compose_sse", "transform"]),
    ...cases("cpu.exe", ["heap", "strings", "api", "sync", "threads", "file"]),
    ...cases("cxx.exe", ["map", "string", "vector", "virtual", "exceptions", "unordered"]),
  ],
  gdi: cases("gdi.exe", ["vn", "vn_dirty", "alphablend", "stretch", "text", "small"]),
  wic: cases("wic.exe", ["fant_grow", "fant_shrink", "cubic_grow", "cubic_shrink", "linear_grow"]),
  ddraw: cases("ddraw.exe", ["vn", "lock", "colorkey", "flip"]),
  d3d8: cases("d3d8.exe", ["vn", "sprites", "upload"]),
  d3d9: cases("d3d9.exe", ["vn", "fade", "sprites_batched", "sprites_calls", "rt", "upload", "state"]),
  d3d11: cases("d3d11.exe", ["vn", "fade", "sprites_batched", "sprites_calls", "rt", "upload", "upload_map"]),
  d2d: cases("d2d.exe", ["vn", "bitmap", "text", "shapes"]),
  gl: cases("gl.exe", ["vn", "sprites_immediate", "sprites_arrays", "upload", "fade", "rt"]),
  // a whole (small) game played as a player plays it, paced: tests/bench/scenario.mjs
  scenario: [{ exe: "scenario.exe", args: "play", scenario: true }],
  // the same, a game made with 吉里吉里Z and KAG3 (tests/bench/gen-kirikiri.mjs): the engine games
  // ship with, its scripts (TJS), archive (xp3), PNG pictures and transitions
  kirikiri: [{ exe: "kirikiri\\tvpwin32.exe", name: "kirikiri", args: "-mode=play", load: "-mode=load", scenario: true }],
  // the same game on 吉里吉里2 2.32r2 (tests/bench/fetch-krkr2.sh): the engine most released
  // KiriKiri games use (DirectDraw/GDI, its own TJS2 and KAG3). It logs Debug.message only to its
  // log file, which sake echoes (SAKE_ECHO_FILE)
  krkr2: [{ exe: "krkr2\\krkr.eXe", name: "krkr2", args: "-mode=play", load: "-mode=load", env: ["SAKE_ECHO_FILE=krkr.console.log"], scenario: true }],
  // the same story on Ren'Py 8.5 (testdata/renpy, tests/bench/pack-renpy.py): a 64-bit program,
  // Python 3, SDL2, ffmpeg (WAV, a VP9 movie), GLSL shaders (blur, sepia, a ripple of its own),
  // through OpenGL (gl2) or ANGLE's Direct3D 11 (angle2). ('""': Ren'Py's base directory, the exe's)
  "renpy-gl": [{ exe: "renpy\\RenBench.exe", name: "renpy-gl", args: '"",gl', load: '"",gl_load', env: ["SAKE_ECHO_FILE=log.txt"], scenario: true }],
  "renpy-angle": [{ exe: "renpy\\RenBench.exe", name: "renpy-angle", args: '"",angle', load: '"",angle_load', env: ["SAKE_ECHO_FILE=log.txt"], scenario: true }],
  // the same story on ONScripter (testdata/onscripter, tests/bench/build-onscripter.sh): the
  // NScripter clone many doujin games ship with, SDL 1.2 through GDI, its own sprite compositing
  // and effects (rule images, mosaic, breakup.dll), FreeType text. The mode comes from a .reg file
  onscripter: [{ exe: "onscripter\\onscripter.exe", name: "onscripter", args: "--registry,play.reg", load: "--registry,load.reg", scenario: true }],
  // the same, built for x64 (mingw-w64's msvcrt.dll is sake's own): sake's 64-bit path
  // the same story on a Win16 program (testdata/novel16, tests/bench/build-novel16.sh): a small
  // engine as Windows 3.1 games were written, 16-bit code (Open Watcom), GDI raster operations only
  win16: [{ exe: "novel16\\novel16.exe", name: "win16", args: "", load: "load", scenario: true }],
  // the same script on a second Win16 engine of the other kind (testdata/novel16p, Free Pascal,
  // tests/bench/build-novel16p.sh): it composes its 256-colour screen in its own 16-bit code
  // (sprites, a darkened text window, pixel transitions) and shows it with StretchDIBits
  "win16-pascal": [{ exe: "novel16p\\novel16p.exe", name: "win16-pascal", args: "", load: "load", scenario: true }],
  // sake-grow.wasm, the page's fallback where 4 GB at once is refused (web/src/runtime.ts
  // sakeBuild): its memory starts at 2.06 GB and grows (&grow takes 512 MiB mid-run so that it
  // does). The same kernels as above, which must give the same checks; against cpu-game's numbers:
  // what the growable memory costs (on desktop Chromium; Android's bounds checks cost more)
  "memory-grow": [
    { exe: "bench.exe", args: "2", format: "plain", page: "memory=grow&grow=512", grows: true },
    ...cases("cpu.exe", ["script", "heap", "strings", "threads", "file"], { page: "memory=grow&grow=512", grows: true }),
    ...cases("cxx.exe", ["map", "exceptions"], { page: "memory=grow&grow=512", grows: true }),
  ],
  onscripter64: [{ exe: "onscripter64\\onscripter.exe", name: "onscripter64", args: "--registry,play.reg", load: "--registry,load.reg", scenario: true }],
};
