#!/usr/bin/env python3
"""Checks tests/unzip-cp932.py (the corpus' zip extractor when unar fails) on zips as Japanese
trial versions come: backslash separators and directory entries ("Game\\MonoBleedingEdge\\",
which unar turns into a file), Shift-JIS names without the UTF-8 flag and UTF-8 ones with it,
names that leave the output directory, a relative output directory, and corpus-prep.sh
taking it when unar fails.

    python3 tests/test_unzip_cp932.py
"""
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))


# (zipfile writes an ASCII name as it is and any other as UTF-8 with the flag: a Shift-JIS
# name goes in as an ASCII stand-in of its length, replaced by its bytes afterwards)
SJIS = '体験版.txt'.encode('cp932')
STAND = b'Q' * len(SJIS)


def entry(name, data=b''):
    return zipfile.ZipInfo(name), data


def make(path, entries):
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as z:
        for i, data in entries:
            z.writestr(i, data)
    b = open(path, 'rb').read()
    open(path, 'wb').write(b.replace(STAND, SJIS))


def files(root):
    out = {}
    for d, dirs, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            out[os.path.relpath(p, root).replace(os.sep, '/')] = open(p, 'rb').read()
        for x in dirs:
            p = os.path.join(d, x)
            if not os.listdir(p):
                out[os.path.relpath(p, root).replace(os.sep, '/') + '/'] = None
    return out


def run(zip_path, out, cwd=None):
    subprocess.run([sys.executable, '-I', os.path.join(HERE, 'unzip-cp932.py'), zip_path, out], check=True, cwd=cwd)


fails = 0


def check(what, got, want):
    global fails
    if got != want:
        fails += 1
        print(f'FAIL {what}:\n  got  {got}\n  want {want}')
    else:
        print(f'ok   {what}')


with tempfile.TemporaryDirectory() as t:
    z = os.path.join(t, 'game.zip')
    make(z, [
        entry('Game\\'),
        entry('Game\\MonoBleedingEdge\\'),
        entry('Game\\MonoBleedingEdge\\EmbedRuntime\\mono-2.0-bdwgc.dll', b'MZ mono'),
        entry('Game\\MonoBleedingEdge\\etc\\'),
        entry('Game/Game.exe', b'MZ game'),
        entry('Game\\' + STAND.decode(), b'sjis'),
        entry('Game\\ユニコード.txt', b'utf8'),
        entry('..\\evil.txt', b'out'),
        entry('Game\\..\\..\\evil2.txt', b'out'),
    ])
    want = {
        'Game/MonoBleedingEdge/EmbedRuntime/mono-2.0-bdwgc.dll': b'MZ mono',
        'Game/MonoBleedingEdge/etc/': None,
        'Game/Game.exe': b'MZ game',
        'Game/体験版.txt': b'sjis',
        'Game/ユニコード.txt': b'utf8',
    }
    out = os.path.join(t, 'abs')
    run(z, out)
    check('absolute output directory', files(out), want)
    os.makedirs(os.path.join(t, 'rel'))
    run(z, '.', cwd=os.path.join(t, 'rel'))
    check('relative output directory', files(os.path.join(t, 'rel')), want)
    check('nothing outside', sorted(os.listdir(t)), ['abs', 'game.zip', 'rel'])

    # corpus-prep.sh: unar fails on the backslash directories, the fallback extracts them all
    # (the download already there, a store already made: only the extraction runs)
    if shutil.which('unar'):
        trials = os.path.join(t, 'trials')
        os.makedirs(os.path.join(trials, 'g1'))
        shutil.copy(z, os.path.join(trials, 'g1', 'dl.zip'))
        store = os.path.join(t, 'store')
        os.makedirs(store)
        r = subprocess.run(['bash', os.path.join(HERE, 'corpus-prep.sh'), 'g1', 'http://example.invalid/g1.zip', 'Game/Game.exe', store],
                           env=dict(os.environ, TRIALS=trials), capture_output=True, text=True)
        check('corpus-prep exe', (r.returncode, r.stdout.strip()), (0, 'Game/Game.exe'))
        check('corpus-prep files', files(os.path.join(trials, 'g1', 'x')), want)
    else:
        print('skip corpus-prep (no unar)')

print('unzip-cp932:', 'FAILED' if fails else 'ok')
sys.exit(1 if fails else 0)
