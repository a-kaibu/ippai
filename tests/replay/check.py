#!/usr/bin/env python3
"""The chain recorder -> converter -> replayer, checked on Windows' own answers: a probe's trace
played back on Windows (golden/replay_<probe>) has to read back the very pictures the probe
itself did (golden/<probe>), in the same order, the same pixels (both ran on the same WARP).

    tests/replay/check.py [golden dir]     exit 1 on a difference; a probe without both answers
                                           yet is listed and passes
    tests/replay/check.py stale            the probes of record.txt whose trace is missing or
                                           older than their sources (traces/<probe>.src)
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "winref"))
import golden  # noqa: E402


def images(d):
    names = re.findall(r"^image (\S+) \d+ \d+ \d+$", open(os.path.join(d, "stdout.txt"), encoding="latin-1").read(), re.M)
    return [(n, golden.png_read(os.path.join(d, n + ".png"))) for n in names]


def stale():
    for l in open(os.path.join(HERE, "record.txt")):
        p = l.split("#")[0].strip()
        if not p:
            continue
        f = os.path.join(HERE, "traces", p + ".src")
        if not os.path.exists(f) or open(f).read().strip() != golden.src_hash(p):
            print(p)


def main():
    if sys.argv[1:2] == ["stale"]:
        return stale()
    root = sys.argv[1] if len(sys.argv) > 1 else golden.GOLDEN
    bad = 0
    for t in sorted(os.listdir(os.path.join(HERE, "traces"))):
        if not t.endswith(".trace"):
            continue
        probe = t[:-6]
        want_d, got_d = os.path.join(root, probe), os.path.join(root, "replay_" + probe + "-32")
        if not (os.path.exists(want_d) and os.path.exists(got_d)):
            print(f"{probe}: no answer yet")
            continue
        want = images(want_d)
        got = [g for g in images(got_d) if g[0].startswith("r")]
        # (a probe that reads pixels with LockRect prints values, not pictures: nothing to match)
        if want and len(want) != len(got):
            print(f"{probe}: {len(got)} pictures read back in the replay, the probe read {len(want)}")
            bad += 1
        for (wn, w), (gn, g) in zip(want, got):
            if w != g:
                print(f"{probe}: {wn} differs in the replay ({gn})")
                bad += 1
        print(f"{probe}: {min(len(want), len(got))} pictures compared")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
