#!/usr/bin/env python3
"""Tests of tests/corpus/pickexe.py (the game's exe in an unpacked trial): the layouts of the
trials it was written for (SuccubusTemptationW's Unity build with a bigger ffmpeg.exe in
StreamingAssets, AAA's Ren'Py build with lib/windows-i686/zsync.exe), the older rules (tools,
installer folders, nothing but tools), and properties over random trees: the answer is an .exe
of the tree, the same whatever order the files were made in, never a helper inside a Unity
*_Data or Ren'Py lib/ folder when there is anything else, and helpers added to a game's tree
(any size) never change the answer. Also corpus-prep.sh end to end on an extracted tree.

  python3 tests/corpus/pickexe_test.py
"""
import os, random, shutil, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pickexe


def tree(files, order=None):
    """A temporary folder with `files` ({path: size}; a path ending in / is a folder)."""
    d = tempfile.mkdtemp()
    items = list(files.items())
    if order: order.shuffle(items)
    for p, n in items:
        full = os.path.join(d, p)
        if p.endswith('/'):
            os.makedirs(full, exist_ok=True)
            continue
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, 'wb') as f: f.write(b'M' * n)
    return d


class Cases(unittest.TestCase):
    def pick(self, files):
        d = tree(files)
        try: return pickexe.pick(d)
        finally: shutil.rmtree(d)

    def test_unity_over_its_ffmpeg(self):
        r = 'SuccubusTemptationW Trial 1.0.0/'
        self.assertEqual(self.pick({
            r + 'SuccubusTemptationW.exe': 650,
            r + 'UnityCrashHandler64.exe': 1200,
            r + 'SuccubusTemptationW_Data/Managed/x.dll': 5,
            r + 'SuccubusTemptationW_Data/StreamingAssets/FFmpegOut/Windows/ffmpeg.exe': 8000,
        }), r + 'SuccubusTemptationW.exe')

    def test_renpy_over_its_lib(self):
        r = 'AAA_0.99-all/'
        self.assertEqual(self.pick({
            r + 'AAA.exe': 100,
            r + 'AAA.py': 3,
            r + 'renpy/__init__.py': 3,
            r + 'lib/windows-i686/zsync.exe': 900,
            r + 'lib/windows-i686/python.exe': 300,
            r + 'lib/windows-i686/pythonw.exe': 300,
        }), r + 'AAA.exe')

    def test_engine_over_its_launcher(self):
        # ensemble's お嬢様は素直になれない trial: launcher.exe (bigger) greys out its start
        # button until setup.exe has installed the game; AdvHD.exe is the game
        r = 'OJONAI_WKB_TRIAL/'
        self.assertEqual(self.pick({
            r + 'AdvHD.exe': 1659392,
            r + 'launcher.exe': 1932800,
            r + 'setup.exe': 2226176,
            r + 'uninst.exe': 2042368,
        }), r + 'AdvHD.exe')
        # (nothing but tools, a launcher among them: the biggest, as before)
        self.assertEqual(self.pick({r + 'Launcher.exe': 10, r + 'setup.exe': 20}), r + 'setup.exe')

    def test_biggest_not_a_tool(self):
        self.assertEqual(self.pick({'g/game.exe': 10, 'g/config.exe': 50, 'g/uninst.exe': 60, 'g/small.exe': 5}), 'g/game.exe')

    def test_ags_inst_beside_the_game(self):
        # 8人のM娘 体験版 (8mz_TR): game/inst.exe (2.1 MB) is the installer, game/ags.exe (1 MB) the game
        r = '8mz_TR/'
        self.assertEqual(self.pick({r + 'setup.exe': 2440, r + 'game/ags.exe': 1069, r + 'game/inst.exe': 2152,
                                    r + 'game/uninst.exe': 2139}), r + 'game/ags.exe')
        self.assertEqual(self.pick({'first.exe': 10, 'firstinst.exe': 20}), 'firstinst.exe')

    def test_installed_by_inno(self):
        self.assertEqual(self.pick({'setup.exe.inno/app/game.exe': 10, 'setup.exe.inno/app/tiny.exe': 1}), 'setup.exe.inno/app/game.exe')

    def test_only_tools(self):
        self.assertEqual(self.pick({'setup.exe': 10, 'config.exe': 50}), 'config.exe')

    def test_none(self):
        self.assertIsNone(self.pick({'readme.txt': 1}))


class Properties(unittest.TestCase):
    NAMES = ['game', 'start', 'GAME_SYS', 'advhd', 'siglus', 'config', 'setup', 'ffmpeg', 'zsync', 'unins000', 'python']

    def random_tree(self, rng):
        files = {}
        for _ in range(rng.randint(0, 8)):
            folder = rng.choice(['', 'g/', 'g/sub/', 'Game_Data/StreamingAssets/', 'lib/windows-i686/', 'x.zip.x/'])
            files[f'{folder}{rng.choice(self.NAMES)}{rng.randint(0, 2)}.exe'] = rng.randint(1, 300)
        if rng.random() < 0.3:
            files['Game.exe'] = rng.randint(1, 300)
            files['Game_Data/'] = 0
        return files

    def test_random_trees(self):
        rng = random.Random(int(os.environ.get('SEED', '1')))
        for _ in range(300):
            files = self.random_tree(rng)
            answers = set()
            for k in range(3):
                d = tree(files, random.Random(k))
                try: answers.add(pickexe.pick(d))
                finally: shutil.rmtree(d)
            self.assertEqual(len(answers), 1, files)
            e = answers.pop()
            exes = [p for p in files if p.endswith('.exe')]
            if not exes:
                self.assertIsNone(e)
                continue
            self.assertIn(e, exes)
            outside = [p for p in exes if not pickexe.HELPER_DIR.search(p)]
            if outside: self.assertIn(e, outside, files)
            # helpers of any size added to the tree change nothing
            more = dict(files)
            more['Game_Data/StreamingAssets/FFmpegOut/Windows/ffmpeg.exe'] = 10 ** 4
            more['lib/windows-i686/zsync.exe'] = 10 ** 4
            if outside:
                d = tree(more)
                try: self.assertEqual(pickexe.pick(d), e, files)
                finally: shutil.rmtree(d)


class Prep(unittest.TestCase):
    def test_prep_picks_unity_exe(self):
        """corpus-prep.sh on an already extracted Unity trial prints the game's exe."""
        t = tempfile.mkdtemp()
        try:
            x = os.path.join(t, 'u', 'x')
            os.makedirs(os.path.join(x, 'G/G_Data/StreamingAssets'))
            for p, n in (('G/G.exe', 10), ('G/G_Data/StreamingAssets/ffmpeg.exe', 99)):
                with open(os.path.join(x, p), 'wb') as f: f.write(b'M' * n)
            r = subprocess.run(['bash', os.path.join(os.path.dirname(HERE), 'corpus-prep.sh'), 'u', 'http://127.0.0.1:9/u.zip', '', ''],
                               env=dict(os.environ, TRIALS=t), capture_output=True, text=True, timeout=60)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], 'G/G.exe')
        finally:
            shutil.rmtree(t)


if __name__ == '__main__':
    unittest.main()
