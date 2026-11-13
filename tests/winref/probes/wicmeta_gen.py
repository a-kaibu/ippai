#!/usr/bin/env python3
"""wicmeta_data.h for wicmeta.c: a plain 2x2 PNG, one with sRGB / gAMA / two tEXt chunks, a BMP,
a JPEG and a GIF (Pillow).  python3 -I tests/winref/probes/wicmeta_gen.py"""
import io, os, struct, zlib
from PIL import Image


def chunk(t, b):
    return struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b))


def png(extra):
    raw = b"".join(b"\0" + bytes([(x * 60 + y * 90) & 255 for x in range(2 * 4)]) for y in range(2))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0)) + b"".join(extra)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def saved(img, kind, **kw):
    b = io.BytesIO()
    img.save(b, kind, **kw)
    return b.getvalue()


def arr(n, d):
    rows = ",\n".join("    " + ",".join("0x%02x" % x for x in d[i:i + 16]) for i in range(0, len(d), 16))
    return "static const unsigned char %s[] = {\n%s\n};\n" % (n, rows)


pics = [
    ("png_plain", png([])),
    ("png_meta", png([chunk(b"sRGB", b"\1"), chunk(b"gAMA", struct.pack(">I", 45455)),
                      chunk(b"tEXt", b"Software\0paint"), chunk(b"tEXt", b"Title\0hello")])),
    ("bmp24", saved(Image.new("RGB", (2, 2), (10, 200, 30)), "BMP")),
    ("jpeg", saved(Image.new("RGB", (8, 8), (10, 200, 30)), "JPEG", quality=90)),
    ("gif", saved(Image.new("P", (2, 2), 1), "GIF")),
]
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "wicmeta_data.h")
open(out, "w").write("/* made by wicmeta_gen.py: tiny pictures for wicmeta.c */\n" + "".join(arr(n, d) for n, d in pics))
