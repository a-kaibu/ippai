#!/usr/bin/env python3
"""Differential GDI test: tests/diff/gdidiff.c on a reference (Wine, whose DIB engine follows
Windows pixel for pixel; `wine` on PATH) and on sake, operation by operation.

Each line of gdidiff's output is one drawing operation on a fresh canvas and the canvas hash.
A line sake draws differently is a mismatch; tests/diff/gdi-expected.txt lists the mismatches
known now ("<seed>:<index>" per line). The run fails on a new one; fixed ones are listed, and
--update rewrites the file.

  tests/diff/gdi.py [--seeds 1-4] [--count 500] [--update]
  SAKE=target/sake-out/sake.js; REF="wine" (the command that runs the reference)
"""
import argparse, collections, os, shlex, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))


def lines(cmd, env):
    p = subprocess.run(cmd, capture_output=True, env=env, timeout=1800)
    out = (p.stdout + p.stderr).decode(errors="replace").replace("\r", "")
    return [l for l in out.splitlines() if l[:1].isdigit()]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", default="1-4")
    ap.add_argument("--count", type=int, default=500)
    ap.add_argument("--update", action="store_true")
    a = ap.parse_args()
    lo, _, hi = a.seeds.partition("-")
    seeds = range(int(lo), int(hi or lo) + 1)
    sake = os.path.abspath(os.environ.get("SAKE", os.path.join(ROOT, "target/sake-out/sake.js")))
    tmp = tempfile.mkdtemp(prefix="gdidiff-")
    exe = os.path.join(tmp, "gdidiff.exe")
    subprocess.run(["i686-w64-mingw32-gcc", "-O2", "-Wall", os.path.join(HERE, "gdidiff.c"), "-o", exe, "-lgdi32", "-lmsimg32"], check=True)
    os.makedirs(os.path.join(tmp, "c/windows/temp"))
    env = dict(os.environ, SAKE_C=os.path.join(tmp, "c"), WINEDEBUG="-all", WINEPREFIX=os.path.join(tmp, "wineprefix"))
    ref_cmd = shlex.split(os.environ.get("REF", "wine"))
    got = set()
    kinds, bad_kinds = collections.Counter(), collections.Counter()
    first = {}
    for seed in seeds:
        args = [str(seed), str(a.count)]
        ref = lines(ref_cmd + [exe] + args, env)
        mine = lines(["node", sake, exe] + args, env)
        if len(ref) != a.count:
            sys.exit(f"reference gave {len(ref)} of {a.count} lines for seed {seed}")
        for i, r in enumerate(ref):
            m = mine[i] if i < len(mine) else "(missing)"
            k = " ".join(r.split()[2:3] + ["%sbpp" % r.split()[1]])
            kinds[k] += 1
            if r != m:
                got.add(f"{seed}:{i}")
                bad_kinds[k] += 1
                first.setdefault(k, (r, m))
    path = os.path.join(HERE, "gdi-expected.txt")
    known = set(l.strip() for l in open(path) if l.strip() and not l.startswith("#")) if os.path.exists(path) else set()
    for k in sorted(kinds):
        print(f"{k:24} {bad_kinds[k]:4} of {kinds[k]:4} differ")
    for k, (r, m) in sorted(first.items()):
        print(f"\n{k}\n  ref:  {r}\n  sake: {m}")
    new = sorted(got - known, key=lambda s: tuple(map(int, s.split(":"))))
    fixed = sorted(known - got)
    print(f"\n{len(got)} of {sum(kinds.values())} operations differ; {len(new)} new, {len(fixed)} fixed")
    if a.update:
        with open(path, "w") as f:
            f.write("# tests/diff/gdi.py: <seed>:<operation> that sake draws differently from the reference (known)\n")
            f.write("".join(k + "\n" for k in sorted(got, key=lambda s: tuple(map(int, s.split(":"))))))
        return 0
    for k in new[:50]:
        print(f"NEW {k}")
    return 1 if new else 0


if __name__ == "__main__":
    sys.exit(main())
