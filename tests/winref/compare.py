#!/usr/bin/env python3
"""Compares probe outputs (tests/winref/run.sh) of sake with the reference from real Windows.

    compare.py <reference dir> <sake dir> [--known tests/winref/known.txt] [--png dir] [--report file.md]

The reference is run.sh's output on Windows or tests/winref/golden (Windows' answers kept in
the repository, golden.py): then a probe whose source changed since its answer is reported as
stale, and one without an answer as new; neither fails the run.

Line kinds (probe.h): plain lines must be equal; "ulp <n> <label> <hex>" doubles within n ulps;
"image <name> <w> <h> <tol>" pictures within tol per channel (a PNG of reference / sake /
difference goes to --png for each one that differs); "skip ..." as the first reference line
skips the probe. known.txt lists differences already understood: "<probe glob> <regex>  # why",
a diff line matching it does not fail the run (it is still reported, as known).
Exits 1 on any other difference, a probe missing on the sake side or a different exit code.
"""
import difflib
import fnmatch
import os
import re
import struct
import sys
import zlib


def parse(path):
    """-> (lines without images, {image name: (w, h, tol, rows)})"""
    lines, images = [], {}
    try:
        raw = open(path, encoding="latin-1").read().splitlines()
    except FileNotFoundError:
        return None, {}
    i = 0
    while i < len(raw):
        m = re.match(r"image (\S+) (\d+) (\d+) (\d+)$", raw[i])
        if m:
            w, h, tol = int(m[2]), int(m[3]), int(m[4])
            rows = raw[i + 1:i + 1 + h]
            images[m[1]] = (w, h, tol, rows)
            lines.append(raw[i])
            i += 1 + h
        else:
            lines.append(raw[i])
            i += 1
    return lines, images


def ulp_fix(want, got):
    """Rewrite sake's "ulp" lines that are within tolerance of the reference to the reference's text."""
    def key(l):
        p = l.split(" ")
        return (p[2], p[1]) if len(p) == 4 and p[0] == "ulp" else None
    seen, out = {}, list(got)
    pos = {}
    for k, l in enumerate(got):
        kk = key(l)
        if kk:
            pos.setdefault(kk, []).append(k)
    for l in want:
        kk = key(l)
        if not kk:
            continue
        n = seen.get(kk, 0)
        seen[kk] = n + 1
        if kk in pos and n < len(pos[kk]):
            k = pos[kk][n]
            a, b = int(l.split(" ")[3], 16), int(got[k].split(" ")[3], 16)
            # ordered integers of the doubles: distance in ulps (NaNs compare by bits)
            o = lambda u: u if u < 1 << 63 else (1 << 63) - u
            if a == b or abs(o(a) - o(b)) <= int(kk[1]):
                out[k] = l
    return out


def png(path, w, h, rows):
    raw = b"".join(b"\0" + bytes(r) for r in rows)
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def image_diff(name, want, got, png_dir, probe):
    w, h, tol, wr = want
    if got is None:
        return f"image {name}: missing"
    if got[:2] != (w, h) or len(got[3]) != h or len(wr) != h:
        return f"image {name}: size {got[0]}x{got[1]} (want {w}x{h}) or truncated"
    gr = got[3]
    bad, worst, first = 0, 0, None
    for y in range(h):
        if len(wr[y]) != w * 6 or len(gr[y]) != w * 6:
            return f"image {name}: row {y} truncated"
        a, b = bytes.fromhex(wr[y]), bytes.fromhex(gr[y])
        for x in range(w):
            d = max(abs(a[3 * x + c] - b[3 * x + c]) for c in range(3))
            if d > tol:
                bad += 1
                worst = max(worst, d)
                if first is None:
                    first = (x, y, a[3 * x:3 * x + 3].hex(), b[3 * x:3 * x + 3].hex())
    if not bad:
        return None
    if png_dir:
        os.makedirs(png_dir, exist_ok=True)
        sep = [64, 64, 64]
        rows = []
        for y in range(h):
            a, b = bytes.fromhex(wr[y]), bytes.fromhex(gr[y])
            d = bytes(min(255, abs(a[k] - b[k]) * 4) for k in range(len(a)))
            rows.append(list(a) + sep + list(b) + sep + list(d))
        png(os.path.join(png_dir, f"{probe}.{name}.png"), 3 * w + 2, h, rows)
    return f"image {name}: {bad} pixels off by more than {tol} (worst {worst}; first at {first[0]},{first[1]} want {first[2]} got {first[3]})"


def main():
    args = sys.argv[1:]
    opt = {}
    for o in ("--known", "--png", "--report"):
        if o in args:
            k = args.index(o)
            opt[o] = args[k + 1]
            del args[k:k + 2]
    ref, got = args
    stale = set()
    if os.path.exists(os.path.join(ref, "..", "golden.py")) or any(
            os.path.exists(os.path.join(ref, p, "src")) for p in os.listdir(ref)):
        import tempfile
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import golden
        stale = {p for p in os.listdir(ref) if os.path.isdir(os.path.join(ref, p))
                 and open(os.path.join(ref, p, "src")).read().strip() != golden.src_hash(p)}
        tmp = tempfile.mkdtemp()
        golden.GOLDEN = os.path.abspath(ref)
        golden.expand(tmp)
        ref = tmp
    known = []
    if "--known" in opt:
        for l in open(opt["--known"], encoding="utf-8"):
            l = l.split("  #")[0].strip()
            if l and not l.startswith("#"):
                g, rx = l.split(None, 1)
                known.append((g, re.compile(rx)))
    report, failed, totals = [], [], {"ok": 0, "known": 0, "fail": 0, "skip": 0, "stale": 0, "new": 0}
    for probe in sorted(set(os.listdir(got)) - set(os.listdir(ref))):
        if os.path.isdir(os.path.join(got, probe)):
            totals["new"] += 1
            report.append(f"- {probe}: new, no Windows answer yet")
    for probe in sorted(os.listdir(ref)):
        rdir = os.path.join(ref, probe)
        if not os.path.isdir(rdir):
            continue
        if not os.path.isdir(os.path.join(got, probe)):
            continue  # (not run: a filtered run.sh)
        wl, wi = parse(os.path.join(rdir, "stdout.txt"))
        gl, gi = parse(os.path.join(got, probe, "stdout.txt"))
        if wl and wl[0].startswith("skip"):
            totals["skip"] += 1
            report.append(f"- {probe}: skipped on Windows ({wl[0]})")
            continue
        diffs = []
        if gl is None:
            diffs.append("no output from sake")
            gl = []
        rexit = open(os.path.join(rdir, "exit")).read().strip() if os.path.exists(os.path.join(rdir, "exit")) else "?"
        gpath = os.path.join(got, probe, "exit")
        gexit = open(gpath).read().strip() if os.path.exists(gpath) else "?"
        if rexit != gexit:
            diffs.append(f"exit code {gexit} (Windows {rexit})")
        gl = ulp_fix(wl, gl)
        for d in difflib.unified_diff(wl, gl, lineterm="", n=0):
            if d.startswith(("---", "+++", "@@")):
                continue
            # an image header line that matches is compared below
            diffs.append(("want " if d[0] == "-" else "got  ") + d[1:])
        for name, im in wi.items():
            d = image_diff(name, im, gi.get(name), opt.get("--png"), probe)
            if d:
                diffs.append(d)
        if probe in stale:
            totals["stale"] += 1
            report.append(f"- {probe}: stale, its source changed since Windows' answer ({len(diffs)} differences)")
            continue
        is_known = lambda d: any(fnmatch.fnmatch(probe, g) and rx.search(d) for g, rx in known)
        unknown = [d for d in diffs if not is_known(d)]
        if not diffs:
            totals["ok"] += 1
            report.append(f"- {probe}: ok")
        elif not unknown:
            totals["known"] += 1
            report.append(f"- {probe}: {len(diffs)} known differences")
        else:
            totals["fail"] += 1
            failed.append(probe)
            report.append(f"- {probe}: **{len(unknown)} differences** ({len(diffs) - len(unknown)} known)")
            report.extend("      " + d[:300] for d in unknown[:40])
            if len(unknown) > 40:
                report.append(f"      ... {len(unknown) - 40} more")
    summary = f"winref: {totals['ok']} ok, {totals['known']} with known differences, {totals['fail']} differ, {totals['skip']} skipped, {totals['stale']} stale, {totals['new']} new"
    text = summary + "\n\n" + "\n".join(report) + "\n"
    print(text)
    if "--report" in opt:
        open(opt["--report"], "w", encoding="utf-8").write(text)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
