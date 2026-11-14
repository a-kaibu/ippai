#!/usr/bin/env python3
"""Writes the bitmaps loadbmp.rc takes into DIR: small DIBs of every kind LoadBitmap reads
(8 and 4 bits with colours, 1 bit black and white, 24 bits, an OS/2 core header), with known
pixels that loadbmp.c checks.

    gen-bmp.py DIR"""
import struct, sys, os

def bmp(path, w, h, bpp, colors, rows, core=False):
    # rows: top to bottom, each a list of pixel values (palette indexes or (r, g, b))
    stride = (w * bpp + 31) // 32 * 4
    data = b""
    for row in reversed(rows):  # bottom-up
        if bpp == 24:
            line = b"".join(bytes((b, g, r)) for (r, g, b) in row)
        else:
            bits = 0
            line = bytearray()
            acc, n = 0, 0
            for v in row:
                acc = (acc << bpp) | v
                n += bpp
                if n == 8:
                    line.append(acc)
                    acc, n = 0, 0
            if n:
                line.append(acc << (8 - n))
            line = bytes(line)
        data += line + b"\0" * (stride - len(line))
    if core:
        hdr = struct.pack("<IHHHH", 12, w, h, 1, bpp)
        pal = b"".join(bytes((b, g, r)) for (r, g, b) in colors)
    else:
        hdr = struct.pack("<IiiHHIIiiII", 40, w, h, 1, bpp, 0, len(data), 0, 0, len(colors) if bpp <= 8 and len(colors) < (1 << bpp) else 0, 0)
        pal = b"".join(bytes((b, g, r, 0)) for (r, g, b) in colors)
    off = 14 + len(hdr) + len(pal)
    with open(path, "wb") as f:
        f.write(b"BM" + struct.pack("<IHHI", off + len(data), 0, 0, off) + hdr + pal + data)

d = sys.argv[1]
os.makedirs(d, exist_ok=True)
pal8 = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (10, 20, 30)]
bmp(f"{d}/pal8.bmp", 4, 2, 8, pal8, [[0, 1, 2, 3], [4, 3, 2, 1]])
pal4 = [(0, 0, 0), (128, 0, 0), (0, 128, 0), (0, 0, 128)]
bmp(f"{d}/pal4.bmp", 3, 3, 4, pal4, [[1, 2, 3], [3, 2, 1], [0, 1, 0]])
bmp(f"{d}/mono.bmp", 9, 2, 1, [(0, 0, 0), (255, 255, 255)], [[1, 0, 1, 0, 1, 0, 1, 0, 1], [0] * 9])
bmp(f"{d}/rgb.bmp", 2, 2, 24, [], [[(1, 2, 3), (250, 128, 7)], [(0, 0, 0), (255, 255, 255)]])
bmp(f"{d}/core.bmp", 5, 1, 4, [(i * 16, 255 - i * 16, i) for i in range(16)], [[0, 5, 10, 15, 7]], core=True)
