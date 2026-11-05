// The benchmark suite: groups of cases, one GitHub Actions job per group (.github/workflows/bench.yml).
// Each case is one program run in its own page (tests/bench/run.mjs): `exe` from the bench tree
// (tests/bench/build.sh), `args` its command line. Programs print "BENCH <metric> <ms>" lines
// (lower is better); bench.exe (host/tests/bench.c) prints its own "<kernel> <ms> ms <crc>" lines
// (format "plain"). `wall` adds the page's time from load to the program's end as a metric.
//
// Keep metric names stable: the history and the comparison with the previous commit go by them.

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
};
