#!/usr/bin/env python3
"""Writes fdi_data.h: the cabinets the fdi probe unpacks through cabinet.dll's FDI. A stored
folder and an MSZIP folder whose 32 KB blocks reach back into the previous block's output
(zlib with the last 32 KB as its dictionary, as makecab writes them), a file spanning blocks,
an empty file, an executable attribute, a Shift-JIS name, a reserve area, and the same
cabinet with a newer version number. Run again only to change the set (the bytes are what
Windows' answers were recorded from)."""
import os, struct, zlib

def text(n, seed):
    return bytes(b"abcabcabd xyz\n"[(i * 7 + seed) % 14] ^ ((i // 5000) & 1) for i in range(n))

def noise(n, seed):
    x, out = seed * 2654435761 + 1, bytearray()
    for _ in range(n):
        x = (x * 1103515245 + 12345) & 0x7fffffff
        out.append(x >> 16 & 0xff)
    return bytes(out)

def cab(folders, reserve=None, version=(3, 1), set_id=0x1234, i_cab=0):
    files, blocks = [], []
    for kind, items in folders:
        data = b"".join(d for _, d, _ in items)
        chunks = [data[i:i + 0x8000] for i in range(0, len(data), 0x8000)] or [b""]
        enc, hist = [], b""
        for c in chunks:
            if kind == 1:
                z = zlib.compressobj(9, zlib.DEFLATED, -15, zdict=hist) if hist else zlib.compressobj(9, zlib.DEFLATED, -15)
                enc.append((b"CK" + z.compress(c) + z.flush(), len(c)))
                hist = (hist + c)[-0x8000:]
            else:
                enc.append((c, len(c)))
        blocks.append((kind, enc))
        off = 0
        for name, d, attr in items:
            files.append((len(d), off, len(blocks) - 1, name, attr))
            off += len(d)
    flags = 4 if reserve is not None else 0
    head = 36 + (4 + len(reserve[0]) if reserve is not None else 0)
    fres = len(reserve[1]) if reserve is not None else 0
    dres = len(reserve[2]) if reserve is not None else 0
    coff_files = head + (8 + fres) * len(folders)
    ftab = b"".join(struct.pack("<IIHHHH", n, o, f, 0x5821 + i, 0x6000 + 3 * i, attr) + nm + b"\0"
                    for i, (n, o, f, nm, attr) in enumerate(files))
    data_at = coff_files + len(ftab)
    fol, dat = b"", b""
    for kind, enc in blocks:
        fol += struct.pack("<IHH", data_at + len(dat), len(enc), kind) + (reserve[1] if reserve else b"")
        for e, un in enc:
            dat += struct.pack("<IHH", 0, len(e), un) + (reserve[2] if reserve else b"") + e
    total = data_at + len(dat)
    hdr = b"MSCF" + struct.pack("<IIIIIBBHHHHH", 0, total, 0, coff_files, 0, version[0], version[1],
                                len(folders), len(files), flags, set_id, i_cab)
    if reserve is not None:
        hdr += struct.pack("<HBB", len(reserve[0]), fres, dres) + reserve[0]
    return hdr + fol + ftab + dat

A_RDONLY, A_HIDDEN, A_ARCH, A_EXEC = 1, 2, 0x20, 0x40
main = cab([
    (0, [(b"readme.txt", text(300, 1), A_ARCH), (b"empty.dat", b"", A_ARCH), (b"sub\\noise.bin", noise(5000, 2), A_RDONLY)]),
    (1, [(b"game.exe", text(70000, 3) + noise(3000, 4), A_ARCH | A_EXEC), (b"\x83\x51\x81\x5b\x83\x80.txt", text(40000, 5), A_HIDDEN),
         (b"last.bin", noise(100, 6), 0)]),
])
reserved = cab([(1, [(b"r1", text(33000, 7), A_ARCH), (b"r2", noise(10, 8), A_ARCH)])],
               reserve=(b"HDRRESV!", b"FR", b"D"), set_id=7, i_cab=0)
newer = bytearray(main)
newer[24] = 4  # version 1.4

def arr(name, b):
    lines = [", ".join(f"0x{x:02x}" for x in b[i:i + 20]) for i in range(0, len(b), 20)]
    return f"static const unsigned char {name}[{len(b)}] = {{\n    " + ",\n    ".join(lines) + "\n};\n"

out = "/* tests/winref/probes/fdi_gen.py */\n" + arr("cab_main", main) + arr("cab_reserved", reserved) + arr("cab_newer", bytes(newer))
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "fdi_data.h"), "w").write(out)
print(len(main), len(reserved))
