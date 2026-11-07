#!/usr/bin/env python3
"""Trial games' traces in testdata/apitrace (.github/workflows/gametrace.yml records them):
<id>-<bits>.txt (what was recorded: api, exe, seconds, the trace's size and sha256, parts) and the
trace, zstd-compressed, in parts of at most 90 MiB (GitHub refuses a file over 100 MB):
<id>-<bits>.trace.zst.000, .001, ...

    games.py todo [--again=true] [id ...]     the titles to record: those given, else
                                             tests/replay/games.txt's without a trace yet
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


def names(d=GAMES):
    return sorted(os.path.basename(m)[:-4] for m in glob.glob(os.path.join(d, "*.txt")))


def todo(ids, again, d=GAMES, known=None):
    known = titles() if known is None else known
    given = bool(ids)
    ids = ids or listed()
    bad = [i for i in ids if i not in known]
    if bad:
        raise SystemExit(f"not in tests/corpus/titles.tsv: {' '.join(bad)}")
    have = {re.sub(r"-(32|64)$", "", n) for n in names(d)}
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
    else:
        print("\n".join(names(o.dir)))


if __name__ == "__main__":
    main()
