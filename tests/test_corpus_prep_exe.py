#!/usr/bin/env python3
"""Checks how tests/corpus-prep.sh unpacks a downloaded .exe when the extractors only half
succeed: 7z exits 2 on an EXEpress CX self-extractor that names a cabinet volume it doesn't
carry (helter_taikenban_fubuki.exe, the VN Mirror Archive: "Can't open volume: helt_t2.cab")
although it wrote every file, and unar crashes on it. What 7z left is the game when every file
its listing names is there at its size (tests/7z-complete.py); 7z failing with nothing or a
cut file left still goes on to unrar, and when nothing unpacks it the exe itself is kept and run.

The extractors are stand-ins on PATH (each case says what they do), so the order and the exit
codes are what is checked, every combination of them:

    python3 tests/test_corpus_prep_exe.py
"""
import itertools
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
fails = []


def check(what, got, want):
    if got != want:
        fails.append(what)
        print(f'FAIL {what}: got {got!r}, want {want!r}')


# a stand-in: writes FILES (name -> bytes) into the current directory, then exits CODE, or
# crashes (SIGSEGV, as unar did) when CODE is 'segv'. LISTING (name -> size): what `7z l -slt`
# answers (7-Zip's listing of the archive, not of what it wrote)
def stub(path, files, code, listing=None):
    # (only the download: prep also tries 7z on exes inside the tree, cabinet self-extractors)
    lines = ['#!/bin/sh', 'case "$*" in *dl.exe*) ;; *) exit 2 ;; esac']
    if listing is not None:
        lines.append('if [ "$1" = l ]; then')
        lines.append("printf '%s\\n' 'Path = ../dl.exe' 'Type = Cab' '' '----------'")
        for name, size in listing.items():
            lines.append(f"printf '%s\\n' 'Path = {name}' 'Size = {size}' 'Attributes = A' ''")
        lines.append('exit 0; fi')
    for name, data in files.items():
        d = os.path.dirname(name)
        if d: lines.append(f'mkdir -p "{d}"')
        lines.append(f"printf '%s' '{data}' > \"{name}\"")
    lines.append('kill -SEGV $$' if code == 'segv' else f'exit {code}')
    with open(path, 'w') as f: f.write('\n'.join(lines) + '\n')
    os.chmod(path, 0o755)


GAME = {'HELT_T2.exe': 'MZgame', 'HELT_T2T.WAR': 'data'}
# (what each extractor does: files written, exit code)
LISTED = {n: len(d) for n, d in GAME.items()}
# (cut: a file shorter than the listing says, as when 7-Zip stops part way: not kept)
SEVENZ = {'ok': (GAME, 0), 'partial': (GAME, 2), 'nothing': ({}, 2),
          'cut': ({'HELT_T2.exe': 'MZgame', 'HELT_T2T.WAR': 'da'}, 2)}
UNAR = {'segv': ({}, 'segv'), 'fail': ({}, 1)}
UNRAR = {'ok': ({'rar/Game.exe': 'MZrar'}, 0), 'fail': ({}, 3)}

for (zk, (zf, zc)), (uk, (uf, uc)), (rk, (rf, rc)) in itertools.product(SEVENZ.items(), UNAR.items(), UNRAR.items()):
    case = f'7z {zk}, unar {uk}, unrar {rk}'
    with tempfile.TemporaryDirectory() as t:
        bin_ = os.path.join(t, 'bin')
        os.makedirs(bin_)
        stub(os.path.join(bin_, '7z'), zf, zc, LISTED)
        stub(os.path.join(bin_, 'unar'), uf, uc)
        stub(os.path.join(bin_, 'unrar'), rf, rc)
        stub(os.path.join(bin_, 'innoextract'), {}, 1)
        stub(os.path.join(bin_, 'convmv'), {}, 0)
        trials = os.path.join(t, 'trials')
        os.makedirs(os.path.join(trials, 'h'))
        # (not an InstallShield, zlib-stream or zip self-extractor: isextract.py and zsfx.py say no)
        with open(os.path.join(trials, 'h', 'dl.exe'), 'wb') as f: f.write(b'MZ' + b'\0' * 510)
        r = subprocess.run(['bash', os.path.join(HERE, 'corpus-prep.sh'), 'h', 'http://example.invalid/h.exe', '', ''],
                           env=dict(os.environ, TRIALS=trials, PATH=bin_ + os.pathsep + os.environ['PATH']),
                           capture_output=True, text=True)
        got = (r.returncode, r.stdout.strip().splitlines()[-1] if r.stdout.strip() else '')
        whole = zf == GAME
        if whole: want = (0, 'HELT_T2.exe')  # 7z's files when they are all there, whatever its exit code
        elif rc == 0: want = (0, 'rar/Game.exe')  # 7z left nothing: unrar's turn
        else: want = (0, 'h.exe')  # nothing unpacks it: the exe itself, under its URL's name
        check(case, got, want)
        if True:
            x = os.path.join(trials, 'h', 'x')
            check(case + ': downloaded exe removed', os.path.exists(os.path.join(trials, 'h', 'dl.exe')), False)
            check(case + ': only one extractor\'s files', sorted(os.listdir(x)) if os.path.isdir(x) else None,
                  sorted(GAME) if whole else ['rar'] if rc == 0 else ['h.exe'])

# tests/7z-complete.py itself with the real 7-Zip: an archive unpacked whole, a file cut by a
# byte, one missing, a folder entry (not a file), and not an archive at all
if shutil.which('7z'):
    with tempfile.TemporaryDirectory() as t:
        src = os.path.join(t, 'src')
        os.makedirs(os.path.join(src, 'Game', 'empty'))
        for name, data in {'Game/game.exe': b'MZ' * 100, 'Game/体験版.dat': b'd' * 5000, 'readme.txt': b''}.items():
            with open(os.path.join(src, name), 'wb') as f: f.write(data)
        a = os.path.join(t, 'a.7z')
        subprocess.run(['7z', 'a', '-y', a, '.'], cwd=src, capture_output=True, check=True)
        def complete(d):
            return subprocess.run([sys.executable, '-I', os.path.join(HERE, '7z-complete.py'), a, d]).returncode
        out = os.path.join(t, 'out')
        subprocess.run(['7z', 'x', '-y', '-o' + out, a], capture_output=True, check=True)
        check('7z-complete: whole', complete(out), 0)
        with open(os.path.join(out, 'Game', '体験版.dat'), 'r+b') as f: f.truncate(4999)
        check('7z-complete: cut', complete(out), 1)
        subprocess.run(['7z', 'x', '-y', '-o' + out, a], capture_output=True, check=True)
        os.remove(os.path.join(out, 'readme.txt'))
        check('7z-complete: missing', complete(out), 1)
        junk = os.path.join(t, 'junk.exe')
        with open(junk, 'wb') as f: f.write(b'MZ' + b'\0' * 510)
        check('7z-complete: not an archive', subprocess.run([sys.executable, '-I', os.path.join(HERE, '7z-complete.py'), junk, out]).returncode, 1)
else:
    print('skip 7z-complete (no 7z)')

print('corpus-prep exe:', 'FAILED' if fails else 'ok')
sys.exit(1 if fails else 0)
