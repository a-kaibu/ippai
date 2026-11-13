#!/usr/bin/env python3
"""Whether 7-Zip, exiting with an error, still wrote every file of ARCHIVE into DIR whole:
each file `7z l -slt ARCHIVE` lists is in DIR with the listed size. tests/corpus-prep.sh keeps
such a tree: 7z exits 2 on an EXEpress CX self-extractor that names a cabinet volume it doesn't
carry (helter_taikenban_fubuki.exe: "Can't open volume: helt_t2.cab") after writing all of its
files. A listing that fails or names no file, or one file missing or of another size: not whole.

  7z-complete.py ARCHIVE DIR      exit 0: whole, 1: not
"""
import os, subprocess, sys


def entries(listing):
    """[(path, size)] of the files (not folders) in a `7z l -slt` listing"""
    out, cur, started = [], {}, False
    for line in listing.splitlines() + ['']:
        if line.startswith('----------'):
            started = True
            continue
        if not started: continue
        if not line.strip():
            if 'Path' in cur:
                if not (cur.get('Folder') == '+' or 'D' in cur.get('Attributes', '').split(' ')[0]):
                    out.append((cur['Path'], cur.get('Size', '')))
            cur = {}
            continue
        k, sep, v = line.partition(' = ')
        if sep: cur[k] = v
    return out


def whole(archive, d):
    r = subprocess.run(['7z', 'l', '-slt', archive], capture_output=True, text=True, errors='replace',
                       env=dict(os.environ, LC_ALL='C.UTF-8'))
    files = entries(r.stdout)
    if not files: return False
    for path, size in files:
        p = os.path.join(d, path.replace('\\', '/'))
        if not size.isdigit() or not os.path.isfile(p) or os.path.getsize(p) != int(size): return False
    return True


if __name__ == '__main__':
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    sys.exit(0 if whole(sys.argv[1], sys.argv[2]) else 1)
