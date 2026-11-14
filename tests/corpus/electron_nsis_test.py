#!/usr/bin/env python3
"""Tests of tests/corpus/electron_nsis.py and of corpus-prep.sh on electron-builder installers
(#234, あにまるめいど!'s animal_maid-1.0.0-x64.exe: an NSIS installer whose app is
$PLUGINSDIR/app-64.7z). Real installers made with makensis around real 7z archives:

- the app comes out byte for byte as the 7z holds it (random trees: nested folders, Japanese
  and space names, empty files and files over a 7z block), whichever of app-64 / app-32 the
  installer carries, app-64 first when it has both;
- an NSIS installer without an app 7z (or with one under another name, or outside
  $PLUGINSDIR), a 7z archive that is no installer and a file that is no archive are left
  alone, OUT untouched;
- corpus-prep.sh end to end on a trial zip as the mirror has it (installer + .dmg + readme):
  the installer is replaced by its app and the Electron exe is what runs.

Needs makensis (apt nsis) and 7z (p7zip-full).

  python3 tests/corpus/electron_nsis_test.py
"""
import os, random, shutil, subprocess, sys, tempfile, unittest, zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import electron_nsis

PREP = os.path.join(os.path.dirname(HERE), 'corpus-prep.sh')
ENV = dict(os.environ, LC_ALL='C.UTF-8')


def write_tree(root, files):
    for rel, data in files.items():
        p = os.path.join(root, *rel.split('/'))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, 'wb') as f: f.write(data)


def read_tree(root):
    out = {}
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            with open(p, 'rb') as h: out[os.path.relpath(p, root).replace(os.sep, '/')] = h.read()
    return out


def seven(path, files):
    """A 7z archive at PATH of FILES ({relative path: bytes})."""
    src = tempfile.mkdtemp()
    try:
        write_tree(src, files)
        subprocess.run(['7z', 'a', '-y', '-mx=1', path, '.'], cwd=src, check=True, capture_output=True, env=ENV)
    finally:
        shutil.rmtree(src)


def installer(path, plugins, other=None):
    """An NSIS installer at PATH carrying PLUGINS ({name: local file}) in $PLUGINSDIR and OTHER
    ({name: local file}) in $INSTDIR, as electron-builder's (InitPluginsDir, File)."""
    d = os.path.dirname(path)
    lines = ['Unicode true', f'OutFile "{path}"', 'RequestExecutionLevel user', 'InstallDir "$LOCALAPPDATA\\Programs\\t"', 'Section']
    if plugins:
        lines += ['InitPluginsDir', 'SetOutPath "$PLUGINSDIR"'] + [f'File "/oname={n}" "{f}"' for n, f in plugins.items()]
    if other:
        lines += ['SetOutPath "$INSTDIR"'] + [f'File "/oname={n}" "{f}"' for n, f in other.items()]
    lines += ['SectionEnd']
    nsi = os.path.join(d, 'i.nsi')
    with open(nsi, 'w', encoding='utf-8') as f: f.write('\n'.join(lines) + '\n')
    subprocess.run(['makensis', '-V1', '-INPUTCHARSET', 'UTF8', nsi], check=True, capture_output=True)


def app_files(rnd):
    names = ['resources/app.asar', 'あにまるめいど！ 体験版.exe', 'locales/ja.pak', 'resources/app.asar.unpacked/build/include/readme.txt']
    files = {n: rnd.randbytes(rnd.choice([0, 1, 300, 70_000])) for n in names}
    for _ in range(rnd.randrange(0, 6)):
        depth = rnd.randrange(1, 4)
        n = '/'.join(rnd.choice(['a', 'b c', 'データ', 'x.y']) + str(rnd.randrange(9)) for _ in range(depth))
        files[n + '.bin'] = rnd.randbytes(rnd.choice([0, 5, 4096, 1 << 17]))
    return files


@unittest.skipUnless(shutil.which('makensis') and shutil.which('7z'), 'needs makensis and 7z')
class Unpack(unittest.TestCase):
    def setUp(self):
        self.t = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.t)

    def p(self, *a):
        return os.path.join(self.t, *a)

    def test_random_apps_come_out_whole(self):
        for seed in range(6):
            rnd = random.Random(seed)
            files = app_files(rnd)
            name = rnd.choice(['app-64.7z', 'app-32.7z'])
            seven(self.p(f'{seed}.7z'), files)
            installer(self.p(f'{seed}.exe'), {name: self.p(f'{seed}.7z'), 'nsis7z.dll': self.p(f'{seed}.7z')})
            out = self.p(f'out{seed}')
            self.assertEqual(electron_nsis.unpack(self.p(f'{seed}.exe'), out), name, seed)
            self.assertEqual(read_tree(out), files, seed)
            self.assertFalse(os.path.exists(out + '.part'))

    def test_app_64_first(self):
        seven(self.p('64.7z'), {'which': b'64'})
        seven(self.p('32.7z'), {'which': b'32'})
        installer(self.p('i.exe'), {'app-32.7z': self.p('32.7z'), 'app-64.7z': self.p('64.7z')})
        self.assertEqual(electron_nsis.unpack(self.p('i.exe'), self.p('o')), 'app-64.7z')
        self.assertEqual(read_tree(self.p('o')), {'which': b'64'})

    def test_others_are_left_alone(self):
        seven(self.p('a.7z'), {'x.txt': b'x'})
        installer(self.p('plain.exe'), {'System.dll': self.p('a.7z')})
        installer(self.p('named.exe'), {'data.7z': self.p('a.7z')})
        installer(self.p('instdir.exe'), {}, {'app-64.7z': self.p('a.7z')})
        with open(self.p('junk.exe'), 'wb') as f: f.write(b'MZ' + b'\0' * 2000)
        for name in ['plain.exe', 'named.exe', 'instdir.exe', 'a.7z', 'junk.exe']:
            out = self.p('o-' + name)
            os.makedirs(out)
            with open(os.path.join(out, 'keep'), 'wb') as f: f.write(b'k')
            self.assertIsNone(electron_nsis.unpack(self.p(name), out), name)
            self.assertEqual(read_tree(out), {'keep': b'k'}, name)
            r = subprocess.run([sys.executable, '-I', os.path.join(HERE, 'electron_nsis.py'), self.p(name), self.p('cli-' + name)], capture_output=True)
            self.assertEqual(r.returncode, 1, name)
            self.assertFalse(os.path.exists(self.p('cli-' + name)), name)

    def test_corpus_prep_runs_the_electron_exe(self):
        rnd = random.Random(99)
        files = app_files(rnd)
        files['resources/elevate.exe'] = b'MZ' + b'e' * 100
        files['あにまるめいど！ 体験版.exe'] = b'MZ' + b'g' * 300_000
        seven(self.p('app.7z'), files)
        os.makedirs(self.p('w'))
        installer(self.p('w', 'animal_maid-1.0.0-x64.exe'), {'app-64.7z': self.p('app.7z'), 'nsis7z.dll': self.p('app.7z')})
        trials = self.p('trials')
        os.makedirs(os.path.join(trials, 'am'))
        with zipfile.ZipFile(os.path.join(trials, 'am', 'dl.zip'), 'w') as z:
            z.write(self.p('w', 'animal_maid-1.0.0-x64.exe'), 'animal_maid_e/animal_maid-1.0.0-x64.exe')
            z.writestr('animal_maid_e/animal_maid-1.0.0-x64.dmg', b'koly' * 1000)
            z.writestr('animal_maid_e/readme.txt', b'readme')
        r = subprocess.run(['bash', PREP, 'am', 'https://trial.example.invalid/RJ021285_trial.zip', '', ''],
                           env=dict(ENV, TRIALS=trials), capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        exe = r.stdout.strip().splitlines()[-1]
        x = os.path.join(trials, 'am', 'x')
        self.assertTrue(exe.endswith('animal_maid-1.0.0-x64.exe.app/あにまるめいど！ 体験版.exe'), exe)
        self.assertFalse(os.path.exists(os.path.join(x, 'animal_maid_e', 'animal_maid-1.0.0-x64.exe')))
        app = os.path.join(x, os.path.dirname(exe))
        self.assertEqual(read_tree(app), files)


if __name__ == '__main__':
    unittest.main()
