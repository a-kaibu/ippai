#!/usr/bin/env python3
"""Writes the unpack target's starting inputs to seeds/unpack/ (not in git): small zips (one
self-extracting, one holding a setup), InstallShield setups (ISSetupStream 2 and 4, the old
header, the plain one) and a zlib-stream setup, as unpack.rs reads them. The first byte picks
the file's extension (even: .zip, odd: .exe).

    python3 -I seeds.py
"""
import io, os, struct, zipfile, zlib

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "seeds", "unpack")
MASK = [0x13, 0x35, 0x86, 0x07]


def pe(overlay):
    d = bytearray(0x400)
    d[0:2] = b"MZ"
    d[0x3C:0x40] = struct.pack("<I", 0x80)
    d[0x80:0x84] = b"PE\0\0"
    d[0x86:0x88] = struct.pack("<H", 1)
    d[0x94:0x96] = struct.pack("<H", 0xE0)
    sec = 0x80 + 24 + 0xE0
    d[sec + 16 : sec + 24] = struct.pack("<II", 0x200, 0x200)
    return bytes(d) + overlay


def mask(data, name):
    key = [c ^ MASK[i % 4] for i, c in enumerate(name)]
    out = bytearray()
    for i, y in enumerate(data):
        x = (~y ^ key[i % 1024 % len(key)]) & 0xFF
        out.append(((x << 4) | (x >> 4)) & 0xFF)
    return bytes(out)


FILES = [("Game.exe", b"MZ the game" * 20), ("data\\a.dat", bytes(range(256)) * 4), ("readme.txt", b"hello\r\n")]


def stream(ver):
    o = b"ISSetupStream\0" + struct.pack("<HH", len(FILES), ver)
    o = o.ljust(46, b"\0")
    for name, data in FILES:
        d = mask(zlib.compress(data, 6), name.encode())
        n = name.encode("utf-16-le")
        o += struct.pack("<II", len(n), 6) + b"\0\0"
        o += struct.pack("<Q", len(d)) + b"\0" * 30 if ver >= 4 else struct.pack("<I", len(d)) + b"\0" * 10
        o += n + d
    return pe(o)


def old():
    o = (b"InstallShield\0" + struct.pack("<H", len(FILES))).ljust(46, b"\0")
    for name, data in FILES:
        raw = name.encode("cp932")
        d = mask(zlib.compress(data, 6), raw)
        a = bytearray(312)
        a[: len(raw)] = raw
        a[260:264] = struct.pack("<I", 6)
        a[268:272] = struct.pack("<I", len(d))
        o += bytes(a) + d
    return pe(o)


def plain():
    o = b""
    for name, data in FILES:
        o += b"file.bin\0" + name.encode("cp932") + b"\x001.0\0" + str(len(data)).encode() + b"\0" + data
    return pe(o)


def zsfx():
    ini = "[Setup]\r\nVer=1\r\nAppName=テスト\r\nFileCount=%d\r\n" % len(FILES)
    for i, (n, d) in enumerate(FILES):
        ini += "File%d=%s,APP,1,%d,0\r\n" % (i, n.replace("\\", "_"), len(d))
    o = pe(b"junk\x78\xda\xff\xffmore") + zlib.compress(ini.encode("cp932"), 9)
    for _, d in FILES:
        o += b"pad" + zlib.compress(d, 9)
    return o


def zipped(files, prefix=b""):
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w", zipfile.ZIP_DEFLATED) as z:
        for n, d in files:
            z.writestr(n, d)
    return prefix + b.getvalue()


def main():
    os.makedirs(ROOT, exist_ok=True)
    seeds = {
        "zip": b"\0" + zipped([(n.replace("\\", "/"), d) for n, d in FILES] + [("dir/", b"")]),
        "zip-setup": b"\0" + zipped([("setup.exe", stream(4)), ("trial/trial.exe", zipped([("Game.exe", b"MZ")], pe(b"")))]),
        "sfx": b"\1" + zipped([("Game.exe", b"MZ"), ("data.xp3", b"XP3")], pe(b"")),
        "is2": b"\1" + stream(2),
        "is4": b"\1" + stream(4),
        "is-old": b"\1" + old(),
        "is-plain": b"\1" + plain(),
        "zsfx": b"\1" + zsfx(),
    }
    for k, v in seeds.items():
        open(os.path.join(ROOT, k), "wb").write(v)


main()
