#!/usr/bin/env python3
"""Windows' answers kept in the repository (tests/winref/golden), so sake is checked against them
locally and in CI without running Windows each time.

    golden.py import <windows out dir> [probe ...]   run.sh output on Windows -> golden/
    golden.py stale [exe dir]                         probes whose golden is missing or older
                                                      than their source (prints names; exit 0)
    golden.py expand <out dir>                        golden/ -> run.sh's layout with pictures
                                                      inline (what compare.py reads)
    golden.py src <probe>                             the sha256 of the probe's sources here

golden/<probe>/ holds stdout.txt (an "image" line's rows go to <name>.png beside it), exit, and
src: the sha256 of the probe's sources, so a changed probe shows up as stale.
"""
import glob
import hashlib
import os
import re
import struct
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
GOLDEN = os.path.join(HERE, "golden")
# what every probe exe is built with (build.sh)
COMMON = ["probe.h", "fpu53.c", "ja.manifest", "ja.rc", "build.sh"]


def sources(probe):
    base = re.sub(r"-(32|64)$", "", probe)
    if base.startswith("host_"):
        main = os.path.join(ROOT, "host", "tests", base[5:] + ".c")
        # (the C++ ones build-msvc64.sh makes: with it and, against MS STL, its shims)
        cpp = main + "pp"
        if os.path.exists(cpp):
            shim = os.path.join(ROOT, "host", "tests", "msstl-shim")
            extra = [os.path.join(ROOT, "host", "tests", "build-msvc64.sh"), os.path.join(HERE, "build-msvc.sh")]
            if "// msstl" in open(cpp).read():
                extra += [os.path.join(shim, f) for f in sorted(os.listdir(shim))]
            return [cpp] + extra
    elif base.startswith("replay_"):
        # (a recorded D3D trace played back: tests/replay; the converter is part of it)
        rep = os.path.join(ROOT, "tests", "replay")
        conv = os.path.join(ROOT, "crates", "apitrace", "src")
        # (a trial game's trace: its .txt, whose sha256 is the trace's; it is on ghcr.io,
        # tests/replay/games.py)
        game = os.path.join(ROOT, "testdata", "apitrace", base[7:])
        trace = [game + ".txt"] if os.path.exists(game + ".txt") else [os.path.join(rep, "traces", base[7:] + ".trace")]
        return trace + [os.path.join(rep, f) for f in ("replay.c", "replay.h", "d3d9img.c", "d3d8img.c", "d3d11img.c", "build.sh")] + [os.path.join(conv, f) for f in ("replay.rs", "layout.rs", "layouts.txt", "trace.rs", "snappy.rs")] + [os.path.join(HERE, f) for f in COMMON]
    else:
        main = os.path.join(HERE, "probes", base + ".c")
    # (a probe's data header, e.g. imaging_data.h)
    # (and its own resources, e.g. res.rc)
    extra = [os.path.join(HERE, "probes", base + x) for x in ("_data.h", ".rc")]
    return [main] + [f for f in extra if os.path.exists(f)] + [os.path.join(HERE, f) for f in COMMON]


def src_hash(probe):
    h = hashlib.sha256()
    for f in sources(probe):
        if os.path.exists(f):
            b = open(f, "rb").read()
            # (a game's trace by what it is, not where it is kept: the same after it moved)
            if os.path.dirname(os.path.abspath(f)) == os.path.join(ROOT, "testdata", "apitrace"):
                b = b"".join(l + b"\n" for l in b.splitlines() if l.split(b" ")[0] in (b"api", b"exe", b"seconds", b"bytes", b"sha256"))
            h.update(os.path.basename(f).encode() + b"\0" + b)
    return h.hexdigest()


def png_write(path, w, h, rows):
    raw = b"".join(b"\0" + r for r in rows)
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def png_read(path):
    """8-bit RGB PNGs as png_write makes them (filter 0 rows) -> (w, h, rows)."""
    d = open(path, "rb").read()
    i, idat, w, h = 8, b"", 0, 0
    while i < len(d):
        n = struct.unpack(">I", d[i:i + 4])[0]
        t, c = d[i + 4:i + 8], d[i + 8:i + 8 + n]
        if t == b"IHDR":
            w, h = struct.unpack(">II", c[:8])
        elif t == b"IDAT":
            idat += c
        i += 12 + n
    raw = zlib.decompress(idat)
    return w, h, [raw[y * (3 * w + 1) + 1:(y + 1) * (3 * w + 1)] for y in range(h)]


def import_(src, only):
    for probe in sorted(os.listdir(src)):
        d = os.path.join(src, probe)
        if not os.path.isdir(d) or (only and probe not in only):
            continue
        out = os.path.join(GOLDEN, probe)
        os.makedirs(out, exist_ok=True)
        for f in os.listdir(out):
            os.remove(os.path.join(out, f))
        lines = open(os.path.join(d, "stdout.txt"), encoding="latin-1").read().splitlines()
        keep, i = [], 0
        while i < len(lines):
            m = re.match(r"image (\S+) (\d+) (\d+) (\d+)$", lines[i])
            keep.append(lines[i])
            if m:
                w, h = int(m[2]), int(m[3])
                rows = [bytes.fromhex(r) for r in lines[i + 1:i + 1 + h]]
                png_write(os.path.join(out, m[1] + ".png"), w, h, rows)
                i += 1 + h
            else:
                i += 1
        open(os.path.join(out, "stdout.txt"), "w", encoding="latin-1", newline="\n").write("".join(l + "\n" for l in keep))
        ex = os.path.join(d, "exit")
        open(os.path.join(out, "exit"), "w").write(open(ex).read().strip() + "\n" if os.path.exists(ex) else "?\n")
        open(os.path.join(out, "src"), "w").write(src_hash(probe) + "\n")
        print(probe)


def expand(dst):
    for probe in sorted(os.listdir(GOLDEN)):
        g = os.path.join(GOLDEN, probe)
        if not os.path.isdir(g):
            continue
        o = os.path.join(dst, probe)
        os.makedirs(o, exist_ok=True)
        out = []
        for l in open(os.path.join(g, "stdout.txt"), encoding="latin-1").read().splitlines():
            out.append(l)
            m = re.match(r"image (\S+) (\d+) (\d+) (\d+)$", l)
            if m:
                _, _, rows = png_read(os.path.join(g, m[1] + ".png"))
                out.extend(r.hex() for r in rows)
        open(os.path.join(o, "stdout.txt"), "w", encoding="latin-1", newline="\n").write("".join(l + "\n" for l in out))
        open(os.path.join(o, "exit"), "w").write(open(os.path.join(g, "exit")).read())


def stale(exe_dir):
    if exe_dir:
        probes = sorted(f[:-4] for f in os.listdir(exe_dir) if f.endswith(".exe"))
    else:
        probes = sorted(os.listdir(GOLDEN))
    for p in probes:
        f = os.path.join(GOLDEN, p, "src")
        if not os.path.exists(f) or open(f).read().strip() != src_hash(p):
            print(p)


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["import"]:
        import_(a[1], set(a[2:]))
    elif a[:1] == ["expand"]:
        expand(a[1])
    elif a[:1] == ["src"] and len(a) == 2:
        print(src_hash(a[1]))
    elif a[:1] == ["stale"]:
        stale(a[1] if len(a) > 1 else None)
    else:
        sys.exit(__doc__)
