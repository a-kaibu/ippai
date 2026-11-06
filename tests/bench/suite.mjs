// The benchmark suite: groups of cases, one GitHub Actions job per group (.github/workflows/bench.yml).
// Each case is one program run in its own page (tests/bench/run.mjs): `exe` from the bench tree
// (tests/bench/build.sh), `args` its command line. Programs print "BENCH <metric> <ms>" lines
// (lower is better); bench.exe (host/tests/bench.c) prints its own "<kernel> <ms> ms <crc>" lines
// (format "plain"). `wall` adds the page's time from load to the program's end as a metric.
//
// Keep metric names stable: the history and the comparison with the previous commit go by them.

// What the numbers mean: bump it when a change to the programs or the harness makes the numbers
// of before incomparable (the report then compares only with the history of the same version).
//   2: frames read back at the end of the timed ones; IPPAI_NOPACE (was VNVM_NOPACE)
//   3: scenario: starts by what is kept (start.*), idle_cpu by process; kirikiri: still screens
//      for the checkpoint pictures (2.5 s each)
export const VERSION = 3;

// More rounds for the groups whose cases are short and noisy (run.mjs's default when ROUNDS isn't set)
export const MIN_ROUNDS = { "cpu-kernels": 7, "cpu-game": 5, scenario: 5, kirikiri: 5 };

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
};
