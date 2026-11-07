#!/usr/bin/env python3
"""Writes crates/kura/src/d3d11/winfmt.rs, Windows' answers per DXGI format, from the d3d11api
probe's golden output (python3 tests/winref/golden.py expand <dir>, then this with
<dir>/d3d11api-32/stdout.txt). Run it again when that golden changes."""
import sys, os

COLS = ["none", "vb", "ib", "cb", "srv", "so", "rt", "ds", "uav", "mips", "staging", "dynamic", "cube", "genmips", "ms4", "tex1d", "tex3d", "bsrv"]

rows = {}
for line in open(sys.argv[1]):
    t = line.split()
    if not t or t[0] != "fmt":
        continue
    f = int(t[1])
    sup = int(t[3], 16) if t[2] == "ok" else 0
    sup2 = int(t[5], 16) if t[4] == "ok" else 0
    ms = [int(t[7]), int(t[8]), int(t[9])]
    res = t[11:20] + t[21:29] + [t[30]]
    assert t[10] == "tex" and t[20] == "|" and t[29] == "bsrv" and len(res) == len(COLS), line
    mask = sum(1 << i for i, r in enumerate(res) if r == "ok")
    bad = [r for r in res if r not in ("ok", "inval")]
    assert not bad, line
    rows[f] = (t[2] == "ok", sup, t[4] == "ok", sup2, ms, mask)

out = os.path.join(os.path.dirname(__file__), "../../crates/kura/src/d3d11/winfmt.rs")
with open(out, "w") as o:
    o.write("//! Windows' answers per DXGI format (1..=132), from the d3d11api winref probe's golden\n")
    o.write("//! output: written by tests/winref/d3d11_formats.py, do not edit.\n\n")
    o.write("/// what CreateTexture2D (16x16, one level unless said, DEFAULT) accepts, bit per case\n")
    for i, c in enumerate(COLS):
        o.write(f"pub const {c.upper()}: u32 = 1 << {i};\n")
    o.write("\n/// (CheckFormatSupport succeeds, its bits, FORMAT_SUPPORT2 succeeds, its bits,\n")
    o.write("/// quality levels at 2/4/8 samples, accepted cases)\n")
    o.write("pub const FORMATS: [(bool, u32, bool, u32, [u8; 3], u32); 133] = [\n")
    o.write("    (false, 0, false, 0, [0, 0, 0], 0),\n")
    for f in range(1, 133):
        ok, sup, ok2, sup2, ms, mask = rows[f]
        o.write(f"    ({str(ok).lower()}, {sup:#x}, {str(ok2).lower()}, {sup2:#x}, [{ms[0]}, {ms[1]}, {ms[2]}], {mask:#x}),\n")
    o.write("];\n")
print("wrote", os.path.normpath(out))
