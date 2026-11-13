#!/usr/bin/env python3
"""Checks how tests/corpus-prep.sh unpacks a downloaded .exe when the extractors only half
succeed: 7z exits 2 on an EXEpress CX self-extractor that names a cabinet volume it doesn't
carry (helter_taikenban_fubuki.exe, the VN Mirror Archive: "Can't open volume: helt_t2.cab")
although it wrote every file, and unar crashes on it. What 7z left is the game; 7z failing with
nothing left still goes on to unrar, and when nothing unpacks it the exe itself is kept and run.

The extractors are stand-ins on PATH (each case says what they do), so the order and the exit
codes are what is checked, every combination of them:

    python3 tests/test_corpus_prep_exe.py
"""
import itertools
import os
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
# crashes (SIGSEGV, as unar did) when CODE is 'segv'
def stub(path, files, code):
    lines = ['#!/bin/sh']
    for name, data in files.items():
        d = os.path.dirname(name)
        if d: lines.append(f'mkdir -p "{d}"')
        lines.append(f"printf '%s' '{data}' > \"{name}\"")
    lines.append('kill -SEGV $$' if code == 'segv' else f'exit {code}')
    with open(path, 'w') as f: f.write('\n'.join(lines) + '\n')
    os.chmod(path, 0o755)


GAME = {'HELT_T2.exe': 'MZgame', 'HELT_T2T.WAR': 'data'}
# (what each extractor does: files written, exit code)
SEVENZ = {'ok': (GAME, 0), 'partial': (GAME, 2), 'nothing': ({}, 2)}
UNAR = {'segv': ({}, 'segv'), 'fail': ({}, 1)}
UNRAR = {'ok': ({'rar/Game.exe': 'MZrar'}, 0), 'fail': ({}, 3)}

for (zk, (zf, zc)), (uk, (uf, uc)), (rk, (rf, rc)) in itertools.product(SEVENZ.items(), UNAR.items(), UNRAR.items()):
    case = f'7z {zk}, unar {uk}, unrar {rk}'
    with tempfile.TemporaryDirectory() as t:
        bin_ = os.path.join(t, 'bin')
        os.makedirs(bin_)
        stub(os.path.join(bin_, '7z'), zf, zc)
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
        if zf: want = (0, 'HELT_T2.exe')  # 7z's files, whatever its exit code
        elif rc == 0: want = (0, 'rar/Game.exe')  # 7z left nothing: unrar's turn
        else: want = (0, 'h.exe')  # nothing unpacks it: the exe itself, under its URL's name
        check(case, got, want)
        if True:
            x = os.path.join(trials, 'h', 'x')
            check(case + ': downloaded exe removed', os.path.exists(os.path.join(trials, 'h', 'dl.exe')), False)
            check(case + ': only one extractor\'s files', sorted(os.listdir(x)) if os.path.isdir(x) else None,
                  sorted(GAME) if zf else ['rar'] if rc == 0 else ['h.exe'])

print('corpus-prep exe:', 'FAILED' if fails else 'ok')
sys.exit(1 if fails else 0)
