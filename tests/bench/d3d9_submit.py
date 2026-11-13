#!/usr/bin/env python3
"""tests/bench/d3d9-submit.c in CI: built (build EXE), then run on sake in the browser
(check EXE OUT, with run-browser.mjs's IPPAI / WEB / PLAYWRIGHT): it must end with exit 0,
print all six rounds, and every round's non-timing output must be the same (the draws and
readbacks give the same pixels each time). The timings are printed, not judged.
"""
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
WINREF = HERE.parent / "winref"
NAME = "d3d9-submit-32"


def build(exe):
    exe.mkdir(parents=True, exist_ok=True)
    subprocess.run(["i686-w64-mingw32-gcc", "-O2", "-msse2", str(HERE / "d3d9-submit.c"), str(WINREF / "fpu53.c"),
                    "-ld3d9", "-lgdi32", "-o", str(exe / f"{NAME}.exe")], check=True)
    (exe / "browser.txt").write_text(NAME + "\n")


def rounds(lines):
    """The output split at its "bench round N: ..." lines: each round's other lines, and the timings."""
    out, cur, times = [], [], []
    for line in lines:
        if line.startswith("bench round "):
            out.append(cur)
            times.append(line)
            cur = []
        else:
            cur.append(line)
    return out, times, cur


def check(exe, out):
    env = dict(os.environ)
    env.setdefault("TIMEOUT", "300")
    subprocess.run(["node", str(WINREF / "run-browser.mjs"), str(exe), str(out)], env=env, check=True)
    result = out / NAME
    assert (result / "exit").read_text().strip() == "0", (result / "stderr.txt").read_text()[-4000:]
    lines = (result / "stdout.txt").read_text().splitlines()
    per, times, rest = rounds(lines)
    assert len(per) == 6 and not rest, f"{len(per)} rounds, then {rest[:5]}"
    assert per[0] and all(r == per[0] for r in per), "a round's output differs from the first's"
    assert not any("bad" in line for line in per[0]), per[0]
    print("\n".join(times))
    print(f"d3d9-submit: 6 rounds of {len(per[0])} matching lines")


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["build"] and len(a) == 2:
        build(Path(a[1]))
    elif a[:1] == ["check"] and len(a) == 3:
        check(Path(a[1]), Path(a[2]))
    else:
        sys.exit("usage: d3d9_submit.py build EXE | check EXE OUT")
