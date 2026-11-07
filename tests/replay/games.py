#!/usr/bin/env python3
"""Trial games' traces in testdata/apitrace (.github/workflows/gametrace.yml records them):
<id>-<bits>.txt (what was recorded: api, exe, seconds, the trace's size and sha256, parts) and the
trace, zstd-compressed, in parts of at most 90 MiB (GitHub refuses a file over 100 MB):
<id>-<bits>.trace.zst.000, .001, ...

    games.py todo [--again=true] [id ...]     the titles to record: those given, else
                                             tests/replay/games.txt's without a trace yet and not
                                             skipped (--again: all of them)
    games.py skip <id> <reason> [detail]      a title with no trace to keep (none: it draws without
                                             D3D8/9/11; notrace: it made none; noconvert: its trace
                                             does not convert) in skipped.tsv, not tried again
    games.py took <id>                        a title whose trace was committed: off skipped.tsv
    games.py pack <x.trace> <dir/name> --api A --exe E --seconds S [--part BYTES]
    games.py unpack <dir/name> <x.trace>      the parts put together, checked against the sha256
    games.py names [dir]                      the traces there (<id>-<bits>), for build.sh
"""
import argparse
import glob
import hashlib
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
GAMES = os.path.join(HERE, "..", "..", "testdata", "apitrace")
TITLES = os.path.join(HERE, "..", "corpus", "titles.tsv")
PART = 90 << 20


def listed(path=os.path.join(HERE, "games.txt")):
    return [l.split("#")[0].split()[0] for l in open(path, encoding="utf-8") if l.split("#")[0].strip()]


def titles(path=TITLES):
    return {l.split("\t")[0] for l in open(path, encoding="utf-8").read().splitlines()[1:] if l}


REASONS = ("none", "notrace", "noconvert")


def skipped(d=GAMES):
    """skipped.tsv: id -> (reason, detail)"""
    f = os.path.join(d, "skipped.tsv")
    if not os.path.exists(f):
        return {}
    out = {}
    for l in open(f, encoding="utf-8").read().splitlines():
        if not l or l.startswith("#"):
            continue
        i, r, *x = l.split("\t")
        if r not in REASONS:
            raise SystemExit(f"{f}: {i}: reason {r!r}")
        out[i] = (r, x[0] if x else "")
    return out


def write_skipped(m, d=GAMES):
    f = os.path.join(d, "skipped.tsv")
    os.makedirs(d, exist_ok=True)
    with open(f, "w", encoding="utf-8") as o:
        o.write("# trial games with no trace, not recorded again unless asked (games.py; gametrace.yml's again)\n")
        for i in sorted(m):
            r, x = m[i]
            o.write(f"{i}\t{r}\t{x}\n" if x else f"{i}\t{r}\n")


def skip(i, reason, detail="", d=GAMES):
    if reason not in REASONS:
        raise SystemExit(f"reason {reason!r}: one of {' '.join(REASONS)}")
    m = skipped(d)
    m[i] = (reason, " ".join(detail.split()))
    write_skipped(m, d)


def took(i, d=GAMES):
    m = skipped(d)
    if m.pop(i, None) is not None:
        write_skipped(m, d)


def names(d=GAMES):
    return sorted(os.path.basename(m)[:-4] for m in glob.glob(os.path.join(d, "*.txt")))


def todo(ids, again, d=GAMES, known=None):
    known = titles() if known is None else known
    given = bool(ids)
    ids = ids or listed()
    bad = [i for i in ids if i not in known]
    if bad:
        raise SystemExit(f"not in tests/corpus/titles.tsv: {' '.join(bad)}")
    have = {re.sub(r"-(32|64)$", "", n) for n in names(d)} | set(skipped(d))
    return [i for i in ids if given or again or i not in have]


def parts(prefix):
    p = sorted(glob.glob(glob.escape(prefix) + ".trace.zst.[0-9][0-9][0-9]"))
    # (000, 001, ... with none missing)
    if [x[-3:] for x in p] != [f"{k:03d}" for k in range(len(p))]:
        raise SystemExit(f"{prefix}: parts {' '.join(x[-3:] for x in p)}")
    return p


def pack(trace, prefix, api, exe, seconds, part=PART):
    data = open(trace, "rb").read()
    z = subprocess.run(["zstd", "-19", "-T0", "-q", "-c"], input=data, stdout=subprocess.PIPE, check=True).stdout
    for old in glob.glob(glob.escape(prefix) + ".trace.zst.*"):
        os.remove(old)
    n = max(1, -(-len(z) // part))
    for k in range(n):
        open(f"{prefix}.trace.zst.{k:03d}", "wb").write(z[k * part:(k + 1) * part])
    with open(prefix + ".txt", "w", encoding="utf-8") as f:
        f.write(f"api {api}\nexe {exe}\nseconds {seconds}\nbytes {len(data)}\nsha256 {hashlib.sha256(data).hexdigest()}\nparts {n}\n")
    return n


def meta(prefix):
    return dict(l.split(" ", 1) for l in open(prefix + ".txt", encoding="utf-8").read().splitlines() if " " in l)


def unpack(prefix, out):
    m = meta(prefix)
    p = parts(prefix)
    if len(p) != int(m["parts"]):
        raise SystemExit(f"{prefix}: {len(p)} parts, {m['parts']} recorded")
    z = b"".join(open(x, "rb").read() for x in p)
    r = subprocess.run(["zstd", "-d", "-q", "-c"], input=z, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.returncode:
        raise SystemExit(f"{prefix}: zstd: {r.stderr.decode(errors='replace').strip()}")
    data = r.stdout
    if hashlib.sha256(data).hexdigest() != m["sha256"] or len(data) != int(m["bytes"]):
        raise SystemExit(f"{prefix}: the parts do not make the recorded trace")
    open(out, "wb").write(data)


def main():
    a = argparse.ArgumentParser()
    s = a.add_subparsers(dest="cmd", required=True)
    t = s.add_parser("todo")
    t.add_argument("--again", default="false")
    t.add_argument("ids", nargs="*")
    p = s.add_parser("pack")
    p.add_argument("trace")
    p.add_argument("prefix")
    p.add_argument("--api", required=True)
    p.add_argument("--exe", required=True)
    p.add_argument("--seconds", required=True)
    p.add_argument("--part", type=int, default=PART)
    u = s.add_parser("unpack")
    u.add_argument("prefix")
    u.add_argument("out")
    k = s.add_parser("skip")
    k.add_argument("id")
    k.add_argument("reason")
    k.add_argument("detail", nargs="?", default="")
    k.add_argument("--dir", default=GAMES)
    t2 = s.add_parser("took")
    t2.add_argument("id")
    t2.add_argument("--dir", default=GAMES)
    n = s.add_parser("names")
    n.add_argument("dir", nargs="?", default=GAMES)
    o = a.parse_args()
    if o.cmd == "todo":
        print(" ".join(todo(o.ids, o.again == "true")))
    elif o.cmd == "pack":
        k = pack(o.trace, o.prefix, o.api, o.exe, o.seconds, o.part)
        print(f"{o.prefix}: {k} part(s)")
    elif o.cmd == "unpack":
        unpack(o.prefix, o.out)
    elif o.cmd == "skip":
        skip(o.id, o.reason, o.detail, o.dir)
    elif o.cmd == "took":
        took(o.id, o.dir)
    else:
        print("\n".join(names(o.dir)))


if __name__ == "__main__":
    main()
