#!/usr/bin/env python3
"""status.py: the stages, the first differing frame and draw, drill.txt's narrowing."""
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import status  # noqa: E402

fails = 0


def expect(what, got, want):
    global fails
    if got != want:
        print(f"status_test: {what}: got {got!r}, want {want!r}")
        fails += 1


def picture(name, px):
    return [f"image {name} 2 1 0", px]


def write(d, probe, lines, ex="0"):
    os.makedirs(os.path.join(d, probe), exist_ok=True)
    open(os.path.join(d, probe, "stdout.txt"), "w").write("\n".join(lines) + "\n")
    open(os.path.join(d, probe, "exit"), "w").write(ex + "\n")


# what says a replay was not the recording
for l, bad in [("hr 12 IDirect3DDevice9::Clear 0x8876086c want 0", True), ("unknown DirectDrawCreateEx", True), ("no object IDirectDrawSurface7::Blt", True),
               ("memcpy into no lock (object 3 key 0)", True), ("image f3: GetAttachedSurface 0x887601ff", True), ("image f3 640 480 2", False), ("calls 10 frames 2", False), ("draw 41 IDirectDrawSurface7::Blt", False)]:
    expect(l, bool(status.UNFAITHFUL.match(l)), bad)
expect("frames", status.frames("image f0 2 1 0\nx\nimage d5 2 1 0\ny\nimage f900 2 1 0\nz\n"), [0, 900])

# the first differing frame and draw (known.txt's rules aside)
ref, got = tempfile.mkdtemp(), tempfile.mkdtemp()
a, b = "000000ffffff", "000000fffffe"
write(ref, "p", ["x"] + picture("f0", a) + ["draw 7 IDirect3DDevice9::DrawPrimitive"] + picture("d7", a) + ["draw 9 IDirect3DDevice9::DrawPrimitive"] + picture("d9", a) + picture("f900", a))
write(got, "p", ["x"] + picture("f0", a) + ["draw 7 IDirect3DDevice9::DrawPrimitive"] + picture("d7", a) + ["draw 9 IDirect3DDevice9::DrawPrimitive"] + picture("d9", "000000ff00ff") + picture("f900", "000000000000"))
r = status.probe_diffs(ref, got, "p", [])
expect("first frame", r[1], "f900")
expect("first draw", r[2], "d9")
expect("tolerance 0: one step off counts", status.probe_diffs(ref, got, "p", [])[0] != [], True)
write(got, "q", ["x"] + picture("f0", a))
write(ref, "q", ["x"] + picture("f0", a))
expect("the same", status.probe_diffs(ref, got, "q", [])[0], [])
import re
expect("known.txt's rules", status.probe_diffs(ref, got, "p", [("p", re.compile(r"image (f900|d9):"))])[0], [])
expect("not run on sake", status.probe_diffs(ref, got, "r", []), None)

# drill.txt: a 16th of the range a run, then one frame's draws, a finished one kept
rows = [("g1", "Faithful", {"name": "g1-32", "range": (900, 1800)}),
        ("g2", "Faithful", {"name": "g2-32", "range": (1000, 1001)}),
        ("g3", "Faithful", {"name": "g3-32", "range": (5, 5), "drill_probe": (5, 5, 0), "first_draw": (41, "IDirectDrawSurface7::Blt")}),
        ("g4", "Compatible", {"name": "g4-32"}), ("g5", "Captured", {"name": "g5-32"})]
expect("next drill", status.next_drill(rows), [("g1-32", 900, 1800, 56), ("g2-32", 1001, 1001, 0), ("g3-32", 5, 5, 0)])
d = tempfile.mkdtemp()
open(os.path.join(d, "drill.txt"), "w").write("# x\ng1-32 900 1800 56\ng2-32 1001 1001 0  # one frame\n\n")
expect("drill lines", status.drill_lines(os.path.join(d, "drill.txt")), [("g1-32", 900, 1800, 56), ("g2-32", 1001, 1001, 0)])
# the stages counted cumulatively
text = status.render([("a", "Compatible", {}), ("b", "Faithful", {"first_frame": "f900", "first_draw": (9, "X::Draw")}), ("c", "", {"why": "notrace: x"})])
expect("render", text.splitlines()[0], "apitrace replays of 3 trial games: 2 Captured, 2 Convertible, 2 Faithful, 1 Compatible")
expect("render first draw", "first frame f900; first draw: call 9 X::Draw" in text, True)
if fails:
    sys.exit(1)
print("status_test: ok")
