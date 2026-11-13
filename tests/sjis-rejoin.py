#!/usr/bin/env python3
"""Re-joins Shift-JIS names an extractor split at a 0x5C trail byte (tests/corpus-prep.sh).

Shift-JIS puts 0x5C, the backslash, in the second byte of many characters (十 = 8F 5C, ソ = 83 5C,
表 = 95 5C). An archiver that reads such a name as a Windows path makes a directory of the bytes
before it: 7-Zip on izayoi_taiken_hontai.exe (an LZH self-extractor) writes "十六夜の花嫁・体験版/x"
as "\\x8f/\\x98Z...". A path component that ends inside a two-byte character (a lead byte not
yet followed by its trail byte) cannot be a name of its own: it and the next component were one
name with a 0x5C between them. Such files are moved back under their whole names (still
Shift-JIS bytes: corpus-prep.sh converts names to UTF-8 afterwards) and directories left empty
are removed.

    sjis-rejoin.py DIR      prints how many files moved
"""
import os
import sys


def lead(b):
    return 0x81 <= b <= 0x9f or 0xe0 <= b <= 0xfc


def open_lead(name):
    """True when NAME (bytes) ends with a Shift-JIS lead byte that has no trail byte."""
    i = 0
    while i < len(name):
        if lead(name[i]):
            if i + 1 == len(name): return True
            i += 2
        else:
            i += 1
    return False


def rejoin(path):
    """PATH (bytes, '/'-separated) with each component that ends in an open lead byte joined
    to the next by the 0x5C it lost."""
    out = []
    for c in path.split(b'/'):
        if out and open_lead(out[-1]): out[-1] += b'\\' + c
        else: out.append(c)
    return b'/'.join(out)


def fix(root):
    root = os.fsencode(root)
    moves = []
    for d, _, fs in os.walk(root):
        for f in fs:
            rel = os.path.relpath(os.path.join(d, f), root)
            new = rejoin(rel)
            if new != rel: moves.append((rel, new))
    for a, b in moves:
        dst = os.path.join(root, b)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        if os.path.lexists(dst): continue  # (never overwrite a file the archive really had)
        os.rename(os.path.join(root, a), dst)
    # (only the directories the moved files came from: an archive's own empty ones stay)
    dirs = set()
    for a, _ in moves:
        a = os.path.dirname(a)
        while a: dirs.add(a); a = os.path.dirname(a)
    for d in sorted(dirs, key=len, reverse=True):
        p = os.path.join(root, d)
        if os.path.isdir(p) and not os.listdir(p): os.rmdir(p)
    return len(moves)


if __name__ == '__main__':
    print(fix(sys.argv[1]))
