#!/usr/bin/env python3
"""Checks what tests/corpus-prep.sh makes of trial archives as the VN Mirror Archive has them
(found by the mirror sweep of 2026-11-13):

- tests/sjis-rejoin.py: Shift-JIS names an extractor split at a 0x5C trail byte. Property
  test: random trees of Shift-JIS names (many with 十/ソ/表-style 0x5C trail bytes), split the way
  7-Zip splits them, come back as they were; trees with nothing split are left alone, an
  archive's own empty folders stay, and existing files are never overwritten.
- the game exe chosen by the file's name, not its folders: an unpacked Inno Setup installer
  leaves the game in "setup.exe.inno/app/" (ni-duma_taiken.zip, RJ023059_trial.zip had "no exe"),
  while tools/ folders still don't count.
- installers inside installers (kagura_r_trial010a.exe: a self-extracting zip of an installer):
  unpacked round after round.
- the split Shift-JIS name through corpus-prep.sh end to end.
- a WinRAR self-extractor inside the archive unpacked (koikata_ichika_trial.zip), a program with
  only data in its embedded archive left alone.
- an installer no extractor knows (Gentee CreateInstall) kept and run as it is, and an archive
  with only setup programs in it giving the biggest of them rather than "no exe".

    python3 tests/test_corpus_prep.py [SEED] [CASES]
"""
import importlib.util
import os
import random
import shutil
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('sjis_rejoin', os.path.join(HERE, 'sjis-rejoin.py'))
sj = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sj)

SEED = int(sys.argv[1]) if len(sys.argv) > 1 else 1113
CASES = int(sys.argv[2]) if len(sys.argv) > 2 else 400
fails = 0


def check(what, got, want):
    global fails
    if got != want:
        fails += 1
        print(f'FAIL {what}:\n  got  {got!r}\n  want {want!r}')
        return False
    return True


# characters whose Shift-JIS trail byte is 0x5C (the ones archivers split at), others, ASCII
TRAIL5C = [c for c in '十ソ表能予申貼ЫАЭ構噂暴竹笠圭' if c.encode('cp932')[-1:] == b'\\']
OTHER = list('体験版花嫁ゲームの・（）ＨＰ説明一二三あいうえお')
ASCII = list('abcxyzABC019_-. ')
assert len(TRAIL5C) >= 8, TRAIL5C


def name(rng):
    n = rng.randint(1, 8)
    s = ''.join(rng.choice(rng.choice([TRAIL5C, OTHER, ASCII])) for _ in range(n)).strip(' .') or 'f'
    # (a name ending in 0x5C would end in a separator once split: archives don't hold those)
    if s[-1] in TRAIL5C: s += rng.choice(OTHER)
    return s.encode('cp932')


def tree(rng):
    """{relative bytes path: content} with Shift-JIS names"""
    out = {}
    for i in range(rng.randint(1, 6)):
        parts = [name(rng) for _ in range(rng.randint(1, 3))]
        out[b'/'.join(parts)] = b'%d' % i
    # (no file may also be a folder of another)
    paths = sorted(out)
    return {p: v for p, v in out.items() if not any(q.startswith(p + b'/') for q in paths)}


def split(path):
    """the path as 7-Zip writes it: every 0x5C, trail byte or not, a separator"""
    return path.replace(b'\\', b'/')


def write(root, files):
    root = os.fsencode(root)
    for p, v in files.items():
        full = os.path.join(root, p)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        open(full, 'wb').write(v)


def read(root):
    out, empty = {}, set()
    root = os.fsencode(root)
    for d, dirs, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            out[os.path.relpath(p, root)] = open(p, 'rb').read()
        for x in dirs:
            if not os.listdir(os.path.join(d, x)): empty.add(os.path.relpath(os.path.join(d, x), root))
    return out, empty


def unit():
    ok = True
    ok &= check('open_lead 十 first byte', sj.open_lead('十'.encode('cp932')[:1]), True)
    ok &= check('open_lead 十', sj.open_lead('十'.encode('cp932')), False)
    ok &= check('open_lead ascii', sj.open_lead(b'abc'), False)
    ok &= check('open_lead kana half-width', sj.open_lead('ｱｲ'.encode('cp932')), False)
    # (a trail byte that looks like a lead byte: 能 = 94 5C, ソ = 83 5C; "ソ" + "\x83" ends open)
    ok &= check('open_lead after pair', sj.open_lead('ソ'.encode('cp932') + b'\x83'), True)
    ok &= check('rejoin', sj.rejoin(b'\x8f/\x98Z/a.txt'), b'\x8f\\\x98Z/a.txt')
    ok &= check('rejoin none', sj.rejoin(b'a/b/c'), b'a/b/c')
    return ok


def prop(rng, t):
    """split tree -> fix -> the original tree"""
    want = tree(rng)
    root = os.path.join(t, 'p')
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root)
    # (the split can make two of the names one path, or a file a folder of another: such trees
    # aren't what an archive holds)
    if len({split(p) for p in want}) != len(want): return True
    if any(p != q and split(q).startswith(split(p) + b'/') for p in want for q in want): return True
    write(root, {split(p): v for p, v in want.items()})
    sj.fix(root)
    got, empty = read(root)
    return check(f'tree {sorted(want)}', (got, empty), (want, set()))


def keeps(t):
    ok = True
    root = os.path.join(t, 'k')
    os.makedirs(os.path.join(root, 'empty'))
    write(root, {b'a/b.txt': b'1'})
    sj.fix(root)
    ok &= check('nothing split: untouched', read(root), ({b'a/b.txt': b'1'}, {b'empty'}))
    # a file already at the joined name is not overwritten (the split copy stays)
    root = os.path.join(t, 'o')
    write(root, {b'\x8f\\\x98Z.txt': b'real', b'\x8f/\x98Z.txt': b'split'})
    sj.fix(root)
    ok &= check('no overwrite', read(root)[0], {b'\x8f\\\x98Z.txt': b'real', b'\x8f/\x98Z.txt': b'split'})
    return ok


def zipof(path, entries, stub=b''):
    """a zip (behind STUB: a self-extractor) of {bytes name: data}; names written as raw bytes"""
    tmp = path + '.tmp'
    stands = {}
    with zipfile.ZipFile(tmp, 'w', zipfile.ZIP_DEFLATED) as z:
        for i, (n, data) in enumerate(entries.items()):
            s = (b'Q%d' % i).ljust(len(n), b'Q')
            assert len(s) == len(n)
            stands[s] = n
            z.writestr(s.decode(), data)
    b = open(tmp, 'rb').read()
    for s, n in stands.items(): b = b.replace(s, n)
    os.remove(tmp)
    open(path, 'wb').write(stub + b)


def crc16(b):
    """LHA's CRC-16 (ARC: poly 0xA001 reflected, init 0)"""
    c = 0
    for x in b:
        c ^= x
        for _ in range(8): c = (c >> 1) ^ 0xa001 if c & 1 else c >> 1
    return c


def lzh(path, entries):
    """an LZH archive of stored (-lh0-) level-0 members: {bytes name with \\ separators: data}"""
    out = b''
    for n, data in entries.items():
        h = b'-lh0-' + len(data).to_bytes(4, 'little') * 2 + (0x5a6d0000).to_bytes(4, 'little') + b'\x20\x00'
        h += bytes([len(n)]) + n + crc16(data).to_bytes(2, 'little')
        out += bytes([len(h), sum(h) & 0xff]) + h + data
    open(path, 'wb').write(out + b'\0')


def rar4(files):
    """A RAR 4 archive of stored files (names in Shift-JIS, "\\" separators)."""
    import struct, zlib
    def hdr(t, flags, body):
        h = struct.pack('<BHH', t, flags, 7 + len(body)) + body
        return struct.pack('<H', zlib.crc32(h) & 0xffff) + h
    out = b'Rar!\x1a\x07\x00' + hdr(0x73, 0, b'\0' * 6)
    for n, data in files:
        n = n.encode('cp932')
        out += hdr(0x74, 0x8000, struct.pack('<IIBIIBBHI', len(data), len(data), 2, zlib.crc32(data), 0x5a6d0000, 29, 0x30, len(n), 0x20) + n) + data
    return out + b'\xc4\x3d\x7b\x00\x40\x07\x00'


def prep(t, gid, archive, ext):
    trials = os.path.join(t, 'trials')
    os.makedirs(os.path.join(trials, gid), exist_ok=True)
    shutil.copy(archive, os.path.join(trials, gid, 'dl.' + ext))
    r = subprocess.run(['bash', os.path.join(HERE, 'corpus-prep.sh'), gid, f'http://example.invalid/{gid}.{ext}', '', ''],
                       env=dict(os.environ, TRIALS=trials), capture_output=True)
    return r.returncode, r.stdout.decode(errors='surrogateescape').strip(), os.path.join(trials, gid, 'x')


def e2e(t):
    ok = True
    if not shutil.which('unar'):
        print('skip corpus-prep (no unar)')
        return True
    MZ = b'MZ' + b'\0' * 510
    # the game in an unpacked installer's folder; a bigger tool next to it and an uninstaller
    z = os.path.join(t, 'inno.zip')
    zipof(z, {b'setup.exe.inno/app/game.exe': MZ * 4, b'setup.exe.inno/app/tools/krkr.exe': MZ * 9,
              b'setup.exe.inno/app/unins000.exe': MZ * 20})
    ok &= check('exe in setup.exe.inno/app', prep(t, 'inno', z, 'zip')[:2], (0, 'setup.exe.inno/app/game.exe'))
    # a self-extracting zip in a self-extracting zip: rounds until the game is out
    inner = os.path.join(t, 'inner.exe')
    zipof(inner, {b'game/game.exe': MZ * 4}, stub=MZ)
    outer = os.path.join(t, 'outer.zip')
    zipof(outer, {b'outer.exe': b''})
    zo = os.path.join(t, 'outer.exe.bin')
    zipof(zo, {b'inner.exe': open(inner, 'rb').read()}, stub=MZ)
    zipof(outer, {b'outer.exe': open(zo, 'rb').read()})
    rc, exe, x = prep(t, 'nested', outer, 'zip')
    ok &= check('nested self-extractors', (rc, exe), (0, 'outer.exe.zip/inner.exe.zip/game/game.exe'))
    # a WinRAR self-extractor of the game inside the zip (koikata_ichika_trial.zip): unpacked, not
    # run in the page (its 375 MB filled the browser's memory). A hand-made RAR 4 archive (stored
    # files, Shift-JIS names) behind an MZ stub stands in for WinRAR's SFX module.
    sfx = MZ * 8 + rar4([('恋する気持ち\\AdvHD.exe', MZ * 3), ('恋する気持ち\\Rio.arc', b'data')])
    z = os.path.join(t, 'rarsfx.zip')
    zipof(z, {b'trial.exe': sfx, b'readme.txt': b'r'})
    rc, exe, x = prep(t, 'rarsfx', z, 'zip')
    ok &= check('WinRAR SFX in a zip', (rc, exe), (0, 'dl/trial.exe.sfx/恋する気持ち/AdvHD.exe'))
    # a program carrying an archive of data only (NW.js-like): left alone
    z = os.path.join(t, 'dataonly.zip')
    zipof(z, {b'game.exe': MZ * 8 + rar4([('data\\a.png', b'png')])})
    ok &= check('embedded data archive left alone', prep(t, 'dataonly', z, 'zip')[:2], (0, 'game.exe'))
    # an installer exe none of the extractors knows (michishio_trial_taming2_install.exe, Gentee
    # CreateInstall: "exit 10" from the last extractor tried): kept as it is, under its own name,
    # and chosen although its name says install (sake runs the installer)
    gentee = os.path.join(t, 'gentee.exe')
    open(gentee, 'wb').write(MZ * 8 + b'.gentee\0' + bytes(range(256)) * 64)
    ok &= check('installer nothing unpacks', prep(t, 'ci_install', gentee, 'exe')[:2], (0, 'ci_install.exe'))
    # only an installer and a tool in an archive: the bigger one, not "no exe"
    z = os.path.join(t, 'onlysetup.zip')
    zipof(z, {b'trial/setup.exe': MZ * 9, b'trial/config.exe': MZ * 2, b'trial/readme.txt': b'r'})
    ok &= check('only setup programs', prep(t, 'onlysetup', z, 'zip')[:2], (0, 'trial/setup.exe'))
    # a Shift-JIS name that 7-Zip splits at its 0x5C (十), as in izayoi_taiken_hontai.exe's LZH:
    # whole again, then UTF-8
    if not shutil.which('7z'):
        print('skip split Shift-JIS name (no 7z)')
        return ok
    sjis = '十六夜の花嫁'.encode('cp932')
    z = os.path.join(t, 'split.lzh')
    lzh(z, {sjis + b'\\' + sjis + b'.exe': MZ * 4, sjis + b'\\data.bin': b'd'})
    rc, exe, x = prep(t, 'split', z, 'lzh')
    ok &= check('split Shift-JIS name', (rc, exe), (0, '十六夜の花嫁/十六夜の花嫁.exe'))
    ok &= check('split Shift-JIS files', sorted(read(x)[0]), sorted(['十六夜の花嫁/十六夜の花嫁.exe'.encode(), '十六夜の花嫁/data.bin'.encode()]))
    return ok


with tempfile.TemporaryDirectory() as t:
    unit()
    rng = random.Random(SEED)
    n = sum(1 for _ in range(CASES) if not prop(rng, t))
    print(f'{"FAIL" if n else "ok  "} property: {CASES} random trees (seed {SEED}), {n} failed')
    keeps(t)
    e2e(t)

print('corpus-prep:', 'FAILED' if fails else 'ok')
sys.exit(1 if fails else 0)
