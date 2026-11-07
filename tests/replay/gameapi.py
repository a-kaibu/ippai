#!/usr/bin/env python3
"""Which Direct3D a game draws with, for apitrace's --api, and its width:

    gameapi.py <game dir> <exe inside it>     prints "<api> <bits>": api d3d8, d3d9, dxgi (D3D11
                                              and DXGI) or none; bits 32 or 64

The exe's own names of the D3D DLLs (its imports, or a LoadLibrary string), else those of the
DLLs beside it (an engine's renderer plugin, krkr's or Unity's); among several, the newest API
(a game that can do D3D11 and D3D9 picks D3D11 where it can). The width is the exe's PE machine."""
import os
import re
import struct
import sys

# (newest first; d3d10 games are not replayed: none)
APIS = [("dxgi", rb"d3d11\.dll|dxgi\.dll"), ("d3d9", rb"d3d9\.dll"), ("d3d8", rb"d3d8\.dll")]


def bits(data):
    """the PE's width (32 / 64), None when it is not a PE"""
    if data[:2] != b"MZ" or len(data) < 0x40:
        return None
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        return None
    return {0x14C: 32, 0x8664: 64}.get(struct.unpack_from("<H", data, pe + 4)[0])


def apis(data):
    """the D3D APIs whose DLL the bytes name (ASCII or UTF-16, any case)"""
    low = data.lower()
    wide = low.replace(b"\0", b"")  # (UTF-16 names: the NULs of ASCII characters dropped)
    return {a for a, pat in APIS if re.search(pat, low) or re.search(pat, wide)}


def pick(found):
    for a, _ in APIS:
        if a in found:
            return a
    return "none"


def game(root, exe):
    data = open(os.path.join(root, exe), "rb").read()
    found = apis(data)
    if not found:
        d = os.path.dirname(os.path.join(root, exe))
        for f in sorted(os.listdir(d)):
            p = os.path.join(d, f)
            if f.lower().endswith(".dll") and os.path.isfile(p):
                found |= apis(open(p, "rb").read())
    return pick(found), bits(data)


if __name__ == "__main__":
    a, b = game(sys.argv[1], sys.argv[2])
    if b is None:
        sys.exit(f"{sys.argv[2]}: not a 32- or 64-bit PE")
    print(a, b)
