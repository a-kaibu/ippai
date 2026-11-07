#!/usr/bin/env python3
"""The chain recorder -> converter -> replayer, checked on Windows' own answers: a probe's trace
played back on Windows (golden/replay_<probe>-<bits>) has to read back the very pictures the
probe itself did (golden/<probe>), in the same order, the same pixels (both ran on the same
WARP). The replay reads back more than the probe shows (every D3D11 Map for reading, with its
alpha; a probe printing one pixel's value reads it through a picture too): the probe's pictures
have to be found among the replay's, in order.

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
        p = l.split("#")[0].split()[0] if l.split("#")[0].strip() else ""
        if not p:
            continue
        f = os.path.join(HERE, "traces", p + ".src")
        if not os.path.exists(f) or open(f).read().strip() != golden.src_hash(p):
            print(p)


def match(want, got):
    """The probe's pictures (name, pixels) found in order among the replay's: [(want name, got
    name)], and the first one not found (None: all were)"""
    out, j = [], 0
    for wn, w in want:
        while j < len(got) and got[j][1] != w:
            j += 1
        if j == len(got):
            return out, wn
        out.append((wn, got[j][0]))
        j += 1
    return out, None


def replay_name(probe):
    return f"replay_{probe}-{probe.rsplit('-', 1)[1]}"


def check(root, probe, src_hash=golden.src_hash):
    """-> (ok, message)"""
    rp = replay_name(probe)
    want_d, got_d = os.path.join(root, probe), os.path.join(root, rp)
    if not (os.path.exists(want_d) and os.path.exists(got_d)):
        return True, f"{probe}: no answer yet"
    # (answers of older sources: Windows has not answered for these yet)
    old = [p for p in (probe, rp) if open(os.path.join(root, p, "src")).read().strip() != src_hash(p)]
    if old:
        return True, f"{probe}: waiting for Windows' answers for {' '.join(old)}"
    want = images(want_d)
    # (the back buffer at each Present is the replay's own look, not a read of the probe's)
    got = [g for g in images(got_d) if not g[0].startswith("f")]
    pairs, missing = match(want, got)
    if missing is not None:
        return False, f"{probe}: the probe's {missing} is not among the replay's pictures after {pairs[-1][1] if pairs else 'the start'} ({len(pairs)} of {len(want)} found)"
    return True, f"{probe}: {len(want)} pictures found in the replay's {len(got)}"


def main():
    if sys.argv[1:2] == ["stale"]:
        return stale()
    root = sys.argv[1] if len(sys.argv) > 1 else golden.GOLDEN
    bad = 0
    for t in sorted(os.listdir(os.path.join(HERE, "traces"))):
        if t.endswith(".trace"):
            ok, msg = check(root, t[:-6])
            print(msg)
            bad += not ok
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
