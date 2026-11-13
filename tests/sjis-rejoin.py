#!/usr/bin/env python3
"""Mends the paths extractors get wrong on Japanese Windows archives (tests/corpus-prep.sh):

1. Shift-JIS names split at a 0x5C trail byte. Shift-JIS puts 0x5C, the backslash, in the
   second byte of many characters (十 = 8F 5C, ソ = 83 5C, 表 = 95 5C). An archiver that reads
   such a name as a Windows path makes a directory of the bytes before it: 7-Zip on
   izayoi_taiken_hontai.exe (an LZH self-extractor) writes "十六夜の花嫁・体験版/x" as
   "\\x8f/\\x98Z...". A path component that ends inside a two-byte character (a lead byte not yet
   followed by its trail byte) cannot be a name of its own: it and the next component were one
   name with a 0x5C between them. (Only names that are not UTF-8: unar writes UTF-8 names.)
2. Windows separators kept in a name. unar writes togime_trial2.zip's
   "『俺の上であがく六人の伽女』 体験版２\\AdvHD.exe" as one file whose name holds the backslash.
   A backslash in a UTF-8 (or ASCII) name, or outside a two-byte character of a Shift-JIS one,
   is a separator; a 0-byte file whose name ends in one was a folder entry.

Files are moved under their mended paths (Shift-JIS names stay Shift-JIS bytes: corpus-prep.sh
converts names to UTF-8 afterwards), empty folders the archive had under a split name are made
whole, and folders left empty by the moves are removed. Nothing existing is overwritten.

    sjis-rejoin.py DIR      prints how many paths changed
"""
import os
import sys


def lead(b):
    return 0x81 <= b <= 0x9f or 0xe0 <= b <= 0xfc


def utf8(name):
    try:
        name.decode('utf-8')
        return True
    except UnicodeDecodeError:
        return False


def open_lead(name):
    """True when NAME (bytes), a Shift-JIS name, ends with a lead byte that has no trail byte.
    A UTF-8 name never does: most UTF-8 characters end in a byte Shift-JIS calls a lead byte
    (２ = EF BC 92), and an extractor that wrote UTF-8 names knew their separators."""
    if utf8(name): return False
    i = 0
    while i < len(name):
        if lead(name[i]):
            if i + 1 == len(name): return True
            i += 2
        else:
            i += 1
    return False


def seps(name):
    """NAME split at its separator backslashes: all of them in UTF-8, those outside a two-byte
    character in Shift-JIS."""
    if utf8(name): return name.split(b'\\')
    out, cur, i = [], bytearray(), 0
    while i < len(name):
        b = name[i]
        if lead(b) and i + 1 < len(name):
            cur += name[i:i + 2]
            i += 2
        elif b == 0x5c:
            out.append(bytes(cur))
            cur = bytearray()
            i += 1
        else:
            cur.append(b)
            i += 1
    out.append(bytes(cur))
    return out


def rejoin(path):
    """PATH (bytes, '/'-separated) mended: a component that ends in an open lead byte joined to
    the next by the 0x5C it lost, then separator backslashes made separators. Empty components
    (a trailing separator) are dropped."""
    out = []
    for c in path.split(b'/'):
        if out and open_lead(out[-1]): out[-1] += b'\\' + c
        else: out.append(c)
    parts = []
    for c in out: parts += seps(c)
    return b'/'.join(p for p in parts if p)


def fix(root):
    root = os.fsencode(root)
    moves, folders = [], []
    for d, ds, fs in os.walk(root):
        for f in fs:
            rel = os.path.relpath(os.path.join(d, f), root)
            new = rejoin(rel)
            if new == rel: continue
            # (a folder entry an extractor made a file of: "Game\\MonoBleedingEdge\\", 0 bytes)
            if rel.endswith(b'\\') and os.path.getsize(os.path.join(root, rel)) == 0: folders.append((rel, new, False))
            else: moves.append((rel, new))
        # (an archive's empty folders under a split name, "十六夜の花嫁\\save\\": made again whole)
        if d != root and not ds and not fs:
            rel = os.path.relpath(d, root)
            new = rejoin(rel)
            if new != rel: folders.append((rel, new, True))
    for a, b in moves:
        dst = os.path.join(root, b)
        if os.path.lexists(dst): continue  # (never overwrite a file the archive really had)
        try: os.makedirs(os.path.dirname(dst), exist_ok=True)
        except (FileExistsError, NotADirectoryError): continue
        os.rename(os.path.join(root, a), dst)
    for a, b, isdir in folders:
        try: os.makedirs(os.path.join(root, b), exist_ok=True)
        except (FileExistsError, NotADirectoryError): continue
        if isdir: os.rmdir(os.path.join(root, a))
        else: os.remove(os.path.join(root, a))
    # (only the directories the moved paths came from: an archive's own empty ones, and the
    # folders made whole above, stay)
    dirs, keep = set(), {os.path.join(root, f[1]) for f in folders}
    for a in [m[0] for m in moves] + [f[0] for f in folders]:
        a = os.path.dirname(a)
        while a: dirs.add(a); a = os.path.dirname(a)
    for d in sorted(dirs, key=len, reverse=True):
        p = os.path.join(root, d)
        if p not in keep and os.path.isdir(p) and not os.listdir(p): os.rmdir(p)
    return len(moves) + len(folders)


if __name__ == '__main__':
    print(fix(sys.argv[1]))
