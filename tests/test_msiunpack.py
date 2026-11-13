#!/usr/bin/env python3
"""Checks tests/msiunpack.py (corpus-prep's Windows Installer unpacker when msiextract fails) on
packages built here with msibuild: the cabinet beside the package (as InstallShield ships
Data1.cab next to its .msi), embedded in it ("#name"), split into a set of two cabinets (the
case msiextract answers with "Invalid folder index"), names in another case than the Media
table's, Japanese names and folders. Property: for random trees (folders, names, sizes, contents)
and every layout, the files come out at their install paths with their contents.

    python3 tests/test_msiunpack.py        (needs msibuild/msiinfo (msitools) and 7z)
"""
import os
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def cab_set(files, names, split=None):
    """Uncompressed cabinets of [(key, data)] in one folder: one cabinet, or with split=n a set of
    two whose first holds n bytes of the folder's data. -> [(name, bytes)]
    (MS-CAB: a file's uoffFolderStart counts from the start of the folder, which begins in the
    first cabinet; a file crossing the split is listed in both, iFolder 0xFFFE "continued to the
    next" in the first and 0xFFFD "continued from the previous" in the second; the CFDATA block
    cut by the split has cbUncomp 0 in the first and its whole size in the second.)"""
    blob = b''.join(d for _, d in files)
    total = len(blob)
    cut = total if split is None else split
    bounds = [(0, cut)] if split is None else [(0, cut), (cut, total)]
    blocks = [(k, min(k + 0x8000, total)) for k in range(0, total, 0x8000)] or [(0, 0)]

    def one(i):
        start, end = bounds[i]
        prv, nxt = i > 0, i + 1 < len(bounds)
        cfdata = b''
        nblocks = 0
        for lo, hi in blocks:
            a, b = max(lo, start), min(hi, end)
            if a >= b and not (lo == hi == 0):
                continue
            whole = hi - lo
            uncomp = 0 if b < hi else whole  # cut here: the rest is in the next cabinet
            cfdata += struct.pack('<IHH', 0, b - a, uncomp) + blob[a:b]
            nblocks += 1
        cffiles, at, n = b'', 0, 0
        for key, d in files:
            lo, hi = at, at + len(d)
            at = hi
            # the files that have bytes in this cabinet (an empty one in the cabinet its offset is in)
            if not (lo < end and hi > start or lo == hi and start <= lo and (lo < end or not nxt)):
                continue
            if lo < start:
                ifolder = 0xFFFD
            elif hi > end:
                ifolder = 0xFFFE
            else:
                ifolder = 0
            cffiles += struct.pack('<IIHHHH', len(d), lo, ifolder, 0x5A21, 0, 0x20) + key.encode() + b'\0'
            n += 1
        extra = b''
        if prv:
            extra += names[i - 1].encode() + b'\0' + b'DISK1\0'
        if nxt:
            extra += names[i + 1].encode() + b'\0' + b'DISK1\0'
        coff_files = 36 + len(extra) + 8
        coff_data = coff_files + len(cffiles)
        size = coff_data + len(cfdata)
        flags = (1 if prv else 0) | (2 if nxt else 0)
        header = struct.pack('<4sIIIIIBBHHHHH', b'MSCF', 0, size, 0, coff_files, 0, 3, 1, 1, n, flags, 0x1234, i)
        folder = struct.pack('<IHH', coff_data, nblocks, 0)
        return header + extra + folder + cffiles + cfdata

    return [(names[i], one(i)) for i in range(len(bounds))]


def idt(path, cols, types, table_keys, rows):
    with open(path, 'w', encoding='utf-8', newline='\r\n') as f:
        f.write('\t'.join(cols) + '\n' + '\t'.join(types) + '\n' + '\t'.join(table_keys) + '\n')
        for r in rows:
            f.write('\t'.join(r) + '\n')


def build(work, tree, layout, rng):
    """A package of `tree` ({relpath: bytes}) at work/pkg.msi; layout: beside | embedded | set.
    -> the install paths msiunpack should give ({relpath: bytes})"""
    dirs = {'TARGETDIR': ('', 'SourceDir'), 'INSTALLDIR': ('TARGETDIR', 'Game')}
    comps, files, want = [], [], {}
    for n, (rel, data) in enumerate(sorted(tree.items())):
        parts = rel.split('/')
        parent = 'INSTALLDIR'
        for k in range(len(parts) - 1):
            did = 'D' + str(abs(hash('/'.join(parts[:k + 1]))) % 10**8)
            dirs.setdefault(did, (parent, parts[k]))
            parent = did
        key = f'f{n}'
        comps.append((f'c{n}', parent))
        files.append((key, f'c{n}', parts[-1], str(len(data)), str(n + 1)))
        want['Game/' + rel] = data
    t = os.path.join(work, 't')
    os.makedirs(t)
    idt(os.path.join(t, 'Directory.idt'), ['Directory', 'Directory_Parent', 'DefaultDir'], ['s72', 'S72', 'l255'],
        ['Directory', 'Directory'], [(d, p, n) for d, (p, n) in dirs.items()])
    idt(os.path.join(t, 'Component.idt'), ['Component', 'ComponentId', 'Directory_', 'Attributes', 'Condition', 'KeyPath'],
        ['s72', 'S38', 's72', 'i2', 'S255', 'S72'], ['Component', 'Component'], [(c, '', d, '0', '', '') for c, d in comps])
    idt(os.path.join(t, 'File.idt'), ['File', 'Component_', 'FileName', 'FileSize', 'Version', 'Language', 'Attributes', 'Sequence'],
        ['s72', 's72', 'l255', 'i4', 'S72', 'S20', 'I2', 'i2'], ['File', 'File'],
        [(k, c, fn, sz, '', '', '0', seq) for k, c, fn, sz, seq in files])
    payload = [(k, tree_data) for (k, _, _, _, _), tree_data in zip(files, [tree[r] for r in sorted(tree)])]
    total = sum(len(d) for _, d in payload)
    if layout == 'set' and total > 1:
        names = ['Data1.cab', 'Data11.cab']
        cabs = cab_set(payload, names, split=rng.randrange(1, total))
    else:
        names = ['Data1.cab']
        cabs = cab_set(payload, names)
    media = [(str(i + 1), str(len(files)) if i == len(cabs) - 1 else '0', '1',
              ('#' if layout == 'embedded' else '') + (n.upper() if layout == 'beside' else n), 'DISK1', '')
             for i, (n, _) in enumerate(cabs)]
    idt(os.path.join(t, 'Media.idt'), ['DiskId', 'LastSequence', 'DiskPrompt', 'Cabinet', 'VolumeLabel', 'Source'],
        ['i2', 'i2', 'L64', 'S255', 'S32', 'S72'], ['Media', 'DiskId'], media)
    msi = os.path.join(work, 'pkg.msi')
    # (msibuild takes -s only on its own)
    subprocess.run(['msibuild', msi, '-s', 'test', 'test', 'Intel;1041', '{11111111-2222-3333-4444-555555555555}'], check=True, capture_output=True)
    cmd = ['msibuild', msi]
    # (a UTF-8 database: msibuild drops the Japanese names of a 932 one)
    with open(os.path.join(t, '_ForceCodepage.idt'), 'w', newline='\r\n') as f:
        f.write('\n\n65001\t_ForceCodepage\n')
    for n in ('_ForceCodepage', 'Directory', 'Component', 'File', 'Media'):
        cmd += ['-i', os.path.join(t, n + '.idt')]
    for n, data in cabs:
        p = os.path.join(work, n)
        open(p, 'wb').write(data)
        if layout == 'embedded':
            cmd += ['-a', n, p]
    subprocess.run(cmd, check=True, capture_output=True)
    if layout == 'embedded':
        for n, _ in cabs:
            os.remove(os.path.join(work, n))
    return want


def unpacked(out):
    got = {}
    for d, _, fs in os.walk(out):
        for f in fs:
            p = os.path.join(d, f)
            got[os.path.relpath(p, out).replace(os.sep, '/')] = open(p, 'rb').read()
    return got


fails = 0


def check(what, got, want):
    global fails
    if got != want:
        fails += 1
        print(f'FAIL {what}:\n  got  {sorted(got)[:8]}\n  want {sorted(want)[:8]}')
    else:
        print(f'ok   {what}')


def run(tree, layout, rng, what):
    with tempfile.TemporaryDirectory() as w:
        want = build(w, tree, layout, rng)
        out = os.path.join(w, 'out')
        r = subprocess.run([sys.executable, '-I', os.path.join(HERE, 'msiunpack.py'), os.path.join(w, 'pkg.msi'), out], capture_output=True, text=True)
        check(f'{what} ({layout})', unpacked(out) if r.returncode == 0 else {'exit': r.stderr}, want)


rng = random.Random(1)
fixed = {'Game.exe': b'MZ' + bytes(range(256)) * 300, 'data/体験版.dat': os.urandom(70000), 'セーブ/readme.txt': b'hello\r\n'}
for layout in ('beside', 'embedded', 'set'):
    run(fixed, layout, rng, 'Japanese names, a file over 64 KiB')
# property: random trees
NAMES = ['a.txt', 'Game.exe', 'システム.dat', 'bgm.ogg', 'x', 'README.TXT', '画像.png']
for case in range(int(os.environ.get('CASES', '24'))):
    tree = {}
    for _ in range(rng.randint(1, 6)):
        depth = rng.randint(0, 2)
        rel = '/'.join(rng.choice(['sys', 'データ', 'Save', 'b']) for _ in range(depth)) + ('/' if depth else '') + rng.choice(NAMES)
        size = rng.choice([0, 1, 100, 0x8000, 0x8001, rng.randint(0, 200000)])
        tree[rel] = bytes(rng.getrandbits(8) for _ in range(size))
    # (no file and folder of the same name)
    if any(any(o.startswith(r + '/') for o in tree) for r in tree):
        continue
    run(tree, rng.choice(['beside', 'embedded', 'set']), rng, f'random tree {case}')
print('failures:', fails)
sys.exit(1 if fails else 0)
