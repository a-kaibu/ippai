#!/usr/bin/env python3
"""Differential floating point test: tests/diff/fpudiff.c on the real CPU (under Wine, which
runs it natively; `wine` on PATH) and on sake (JIT, then the interpreter), operation by
operation.

Each difference is sorted by what differs:
  value  the result a program sees (the double / float / integer, x87 condition codes)
  ext80  only the 80-bit x87 register (sake's x87 works in f64 by design: crate koji fpu.rs)
  flags  only the exception flags (MXCSR, x87 status word)
tests/diff/fpu-expected.tsv has the counts per operation and kind; the run fails when one
grows, and --update records the new counts.

  tests/diff/fpu.py [--seed 1] [--count 2000] [--update] [--show N]
  SAKE=target/sake-out/sake.js; REF="wine"
"""
import argparse, collections, os, shlex, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
KINDS = ("value", "ext80", "flags")


def run(cmd, env):
    p = subprocess.run(cmd, capture_output=True, env=env, timeout=3600)
    out = (p.stdout + p.stderr).decode(errors="replace").replace("\r", "")
    return [l for l in out.splitlines() if l and not l.startswith("sake:") and "->" in l]


def parts(l):
    p = l.split()
    op, i = p[0], p.index("->")
    res = p[i + 1:]
    if op == "fcom":  # two status words and EFLAGS: C3 C2 C1 C0 and ZF PF CF are values
        a, b = int(res[0], 16), int(res[1], 16)
        return op, " ".join(p[:i]) + f" {a & 0x4700:04x} {res[2]}", "", f"{a & 0x3f:02x} {b & 0x3f:02x}"
    if op.startswith("f") and op != "fist":
        return op, " ".join(p[:i] + [res[0]]), res[1], res[2]
    return op, " ".join(p[:i] + res[:-1]), "", res[-1]


def compare(ref, mine):
    counts = collections.Counter()
    first = {}
    for k, r in enumerate(ref):
        m = mine[k] if k < len(mine) else "(missing) -> x"
        op, va, ea, fa = parts(r)
        try:
            _, vb, eb, fb = parts(m)
        except (ValueError, IndexError):
            vb = eb = fb = None
        kind = "value" if va != vb else "ext80" if ea != eb else "flags" if fa != fb else None
        counts[(op, None)] += 1
        if kind:
            counts[(op, kind)] += 1
            first.setdefault((op, kind), []).append((r, m))
    return counts, first


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", default="1")
    ap.add_argument("--count", default="2000")
    ap.add_argument("--update", action="store_true")
    ap.add_argument("--show", type=int, default=2)
    a = ap.parse_args()
    sake = os.path.abspath(os.environ.get("SAKE", os.path.join(ROOT, "target/sake-out/sake.js")))
    tmp = tempfile.mkdtemp(prefix="fpudiff-")
    exe = os.path.join(tmp, "fpudiff.exe")
    subprocess.run(["i686-w64-mingw32-gcc", "-O1", "-msse2", "-Wall", os.path.join(HERE, "fpudiff.c"), "-o", exe], check=True)
    os.makedirs(os.path.join(tmp, "c/windows/temp"))
    env = dict(os.environ, SAKE_C=os.path.join(tmp, "c"), WINEDEBUG="-all", WINEPREFIX=os.path.join(tmp, "wineprefix"))
    ref = run(shlex.split(os.environ.get("REF", "wine")) + [exe, a.seed, a.count], env)
    if not ref:
        sys.exit("the reference printed nothing")
    path = os.path.join(HERE, "fpu-expected.tsv")
    known = {}
    if os.path.exists(path):
        for l in open(path):
            if l.strip() and not l.startswith("#"):
                engine, op, kind, n = l.split()
                known[(engine, op, kind)] = int(n)
    rows, worse = [], []
    for engine, jit in (("jit", "1"), ("interp", "0")):
        mine = run(["node", sake, exe, a.seed, a.count], dict(env, MASU_JIT=jit))
        if len(mine) != len(ref):
            print(f"sake ({engine}) stopped after {len(mine)} of {len(ref)} operations")
            worse.append(f"{engine}: incomplete output")
        counts, first = compare(ref, mine)
        print(f"== sake ({engine}): {len(mine)} of {len(ref)} operations")
        print(f"{'op':10} " + " ".join(f"{k:>6}" for k in KINDS) + "    all")
        for op in sorted({o for o, _ in counts}):
            print(f"{op:10} " + " ".join(f"{counts[(op, k)]:6}" for k in KINDS) + f" {counts[(op, None)]:6}")
            for k in KINDS:
                n = counts[(op, k)]
                rows.append((engine, op, k, n))
                if n > known.get((engine, op, k), 0):
                    worse.append(f"{engine} {op} {k}: {known.get((engine, op, k), 0)} -> {n}")
        for (op, k), ex in sorted(first.items()):
            if k == "value":
                for r, m in ex[: a.show]:
                    print(f"  {op} value\n    cpu:  {r}\n    sake: {m}")
    if a.update:
        with open(path, "w") as f:
            f.write("# tests/diff/fpu.py: operations sake gets differently from the CPU, by engine, op and kind (known)\n")
            for engine, op, k, n in rows:
                if n:
                    f.write(f"{engine}\t{op}\t{k}\t{n}\n")
        return 0
    for w in worse:
        print(f"WORSE {w}")
    return 1 if worse else 0


if __name__ == "__main__":
    sys.exit(main())
