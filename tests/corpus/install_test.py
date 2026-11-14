#!/usr/bin/env python3
"""Tests of tests/corpus/install.py (setup programs run in sake before the game) and of
pickexe.not_game (downloads that are no game at all).

Cases: the trials it was written for, as trees (DGCA / Gentee / YU-RIS self-extractors by their
marks, BLUEGALE's and CatSystem2's install media with autorun.inf, an uninstaller never), the game
among the files a setup made, and the sample movies and patches of the mirror. Properties over
random trees: choose() names a file of the tree that is a setup program or the exe picked, never
an uninstaller; game_of() never answers a tool, setup or uninstall program and does not depend on
listing order. End to end (when node's sake.js is there, SAKE_NODE or target/sake-out, and
i686-w64-mingw32-gcc): host/tests/autoinstall.c as a trial's setup, through install.py run and
through corpus-prep.sh (the store's prefix32 holds what it installed, when masu is on PATH).

  python3 tests/corpus/install_test.py
"""
import os, random, shutil, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import install, pickexe


# (the start of a program image: MZ, e_lfanew = 64, "PE\0\0")
PE = b'MZ' + b'\0' * 58 + (64).to_bytes(4, 'little') + b'PE\0\0'


def tree(files, order=None):
    """A temporary folder with `files` ({path: bytes or size})."""
    d = tempfile.mkdtemp()
    items = list(files.items())
    if order: order.shuffle(items)
    for p, v in items:
        full = os.path.join(d, p)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, 'wb') as f:
            # (a size as a negative number: a Windows program image of that many bytes, its
            # headers and then a sparse file, for MiB-sized trees)
            if isinstance(v, int) and v < 0: f.write(PE[:-v]); f.truncate(-v)
            else: f.write(v if isinstance(v, bytes) else b'M' * v)
    return d


MZ = b'MZ' + b'\0' * 62


class Choose(unittest.TestCase):
    def choose(self, files, picked):
        d = tree(files)
        try: return install.choose(d, picked)
        finally: shutil.rmtree(d)

    def test_self_extractors_by_their_marks(self):
        for mark, name in [(b'DGCA SelfExtractor', 'fusikano_try.exe'), (b'Gentee Launcher', 'yumekui_trial_1_setup.exe'),
                           (b'-YU-RIS Self-EXE Extracter', '待雪の花 WEB体験版.exe'), (b"LHA's SFX ", 'mrsjs.exe'),
                           # (巣作りドラゴン's packed DGCA stub: the archive's first chunk header only)
                           (b'DGCA\x20\x00\x00\x00\x70\xd2\xa7\x01', 'DragonT.exe')]:
            self.assertEqual(self.choose({name: MZ + b'x' * 99 + mark + b'y' * 50}, name), name)
        # (a game without a mark is left alone, and so is one that only names DGCA in its text)
        self.assertIsNone(self.choose({'game.exe': MZ + b'game'}, 'game.exe'))
        self.assertIsNone(self.choose({'game.exe': MZ + b'made with DGCA 1.0\0'}, 'game.exe'))

    def test_install_media(self):
        # BLUEGALE's ミセス・ジャンキー: autorun.inf, setup.exe and the game in exe/
        r = 'ミセス・ジャンキー体験版/'
        self.assertEqual(self.choose({r + 'autorun.inf': b'[autorun]\r\nOPEN=setup.exe\r\n', r + 'setup.exe': 245,
                                      r + 'UINSTamp.exe': 32, r + 'exe/mrsjs.exe': 262}, r + 'exe/mrsjs.exe'), r + 'setup.exe')
        # CatSystem2's 恋ニ、甘味ヲソエテ: autorun.inf opens bootmenu.exe, setup/INSTALL.exe installs
        r = 'koiama_trial/恋ニ、甘味ヲソエテ-体験版-/'
        self.assertEqual(self.choose({r + 'autorun.inf': b'[autorun]\r\nopen=bootmenu.exe\r\n', r + 'bootmenu.exe': 98,
                                      r + 'setup/INSTALL.exe': 131, r + 'setup/UNINST.exe': 103, r + 'setup/cs2_open.exe': 892},
                                     r + 'setup/cs2_open.exe'), r + 'setup/INSTALL.exe')
        # (a Japanese name: YU-RIS's インストール.EXE beside アンインストール.exe)
        self.assertEqual(self.choose({'s/autorun.inf': b'', 's/インストール.EXE': 61, 's/Files/アンインストール.exe': 61,
                                      's/Files/game.exe': 349}, 's/Files/game.exe'), 's/インストール.EXE')

    def test_no_media_no_setup(self):
        # (a setup.exe beside the game without autorun.inf: the game runs as it is, ensemble's)
        self.assertIsNone(self.choose({'g/AdvHD.exe': 165, 'g/setup.exe': 222, 'g/uninst.exe': 204}, 'g/AdvHD.exe'))
        # (only an uninstaller on the media)
        self.assertIsNone(self.choose({'autorun.inf': b'', 'uninstall.exe': 5, 'game.exe': 9}, 'game.exe'))

    def test_msi_launchers(self):
        # 天極姫: InstallShield's Setup Launcher beside its .msi and Data1.cab; pickexe falls back
        # to a Windows Installer redistributable (nothing else but tools)
        r = '天極姫【体験版第二弾】/インストールデータ/'
        isl = MZ + b'x' * 300 + b'I\0n\0' + b'InstallShieldMSIDelete10' + b'y' * 40
        self.assertEqual(self.choose({r + 'setup.exe': isl, r + 'instmsiw.exe': 900, r + 'instmsia.exe': 800,
                                      r + 'Data1.cab': 500, r + 'x.msi': 400}, r + 'instmsiw.exe'), r + 'setup.exe')
        # マスクド上海: one exe, the package inside it (picked as the biggest)
        self.assertEqual(self.choose({'MShanghai_trial080111.exe': isl}, 'MShanghai_trial080111.exe'), 'MShanghai_trial080111.exe')
        # 揺れるバスガイド: Visual Studio's MSI loader
        r = '揺れるバスガイド（体験版）/'
        self.assertEqual(self.choose({r + 'setup.exe': MZ + b'MSILoader' + b'z' * 30, r + 'InstMsiW.Exe': 900,
                                      r + 'Setup.Ini': b'[MSILoader]\r\n', r + 'x.msi': 300}, r + 'InstMsiW.Exe'), r + 'setup.exe')
        # (the shallowest of two)
        self.assertEqual(self.choose({'a/b/setup.exe': isl, 'a/setup.exe': isl, 'a/g.exe': 99}, 'a/g.exe'), 'a/setup.exe')

    def test_keep(self):
        isl = MZ + b'InstallShieldMSIDelete10'
        d = tree({'is/setup.exe': isl, 'is/x.msi': 10, 'vs/setup.exe': MZ + b'MSILoader', 'vs/y.MSI': 10,
                  'plain/setup.exe': MZ + b'Inno Setup', 'plain/z.msi': 10, 'alone.msi': 10, 'is/sub/w.msi': 10})
        try:
            k = lambda p: install.keep(os.path.join(d, p))
            self.assertTrue(k('is/setup.exe') and k('is/x.msi') and k('vs/setup.exe') and k('vs/y.MSI'))
            self.assertFalse(k('plain/setup.exe') or k('plain/z.msi') or k('alone.msi') or k('is/sub/w.msi'))
            self.assertFalse(k('missing/q.msi') or k('missing.exe'))
            self.assertEqual(subprocess.run([sys.executable, '-I', os.path.join(HERE, 'install.py'), 'keep',
                                             os.path.join(d, 'is/x.msi')]).returncode, 0)
            self.assertEqual(subprocess.run([sys.executable, '-I', os.path.join(HERE, 'install.py'), 'keep',
                                             os.path.join(d, 'alone.msi')]).returncode, 1)
        finally: shutil.rmtree(d)

    def test_installer_named_kept_whole(self):
        # an installer nothing unpacks, kept under its own name and picked (nothing but tools)
        self.assertEqual(self.choose({'anetea_trial_install.exe': MZ}, 'anetea_trial_install.exe'), 'anetea_trial_install.exe')


class GameOf(unittest.TestCase):
    def game(self, made):
        d = tree({k: v for k, v in made.items()})
        try:
            st = {k: (os.path.getsize(os.path.join(d, k)), 0) for k in made}
            return install.game_of(st, lambda r: os.path.join(d, r))
        finally: shutil.rmtree(d)

    def test_installed_games(self):
        self.assertEqual(self.game({'Program Files/夢喰い体験版１/夢喰い体験版１.exe': 900, 'Program Files/夢喰い体験版１/uninstall.exe': 400,
                                    'Program Files/夢喰い体験版１/wuvorbis.dll': 50, 'users/user/Desktop/夢喰い体験版１.lnk': 1}),
                         'Program Files/夢喰い体験版１/夢喰い体験版１.exe')
        # (DGCA's 不思議の国のカノジョ: RealLive.exe, not its Start.exe launcher nor KineticDelete.exe)
        r = 'Program Files/不思議の国のカノジョ 体験版/'
        self.assertEqual(self.game({r + 'Start.exe': 300, r + 'RealLive.exe': 2000, r + 'KINETICDATA/KineticDelete.exe': 100}), r + 'RealLive.exe')
        # (sake's own msiexec.exe and setup programs a setup copied are not games)
        self.assertIsNone(self.game({'Windows/System32/msiexec.exe': 9, 'BLUEGALE/mrsjs/UINSTamp.exe': 32, 'temp/setup.exe': 50}))

    def test_random_new_files(self):
        rng = random.Random(int(os.environ.get('SEED', '1')))
        names = ['game', 'start', 'setup', 'uninst', 'unins000', 'config', 'インストール', 'アンインストール', 'readme', 'KineticDelete', 'cs2_open']
        for _ in range(150):
            made = {}
            for _ in range(rng.randint(0, 6)):
                folder = rng.choice(['', 'Program Files/x/', 'x/sub/', 'Windows/System32/', 'temp/'])
                made[f'{folder}{rng.choice(names)}{rng.randint(0, 2)}.{rng.choice(["exe", "exe", "dll", "txt"])}'] = rng.randint(1, 200)
            answers = set()
            for k in range(3):
                items = list(made.items())
                random.Random(k).shuffle(items)
                answers.add(self.game(dict(items)))
            self.assertEqual(len(answers), 1, made)
            g = answers.pop()
            if g is None: continue
            self.assertIn(g, made)
            self.assertTrue(g.lower().endswith('.exe'))
            b = g.rsplit('/', 1)[-1]
            self.assertFalse(pickexe.TOOL.search(b) or install.is_setup_name(b) or install.NOT_SETUP.search(b), g)


class ChooseProperties(unittest.TestCase):
    def test_random_trees(self):
        rng = random.Random(int(os.environ.get('SEED', '1')))
        names = ['game', 'setup', 'INSTALL', 'uninst', 'アンインストール', 'インストール', 'bootmenu', 'config']
        for _ in range(150):
            files = {}
            for _ in range(rng.randint(1, 6)):
                folder = rng.choice(['', 'a/', 'a/setup/', 'b/'])
                files[f'{folder}{rng.choice(names)}{rng.randint(0, 1)}.exe'] = rng.randint(1, 100)
            if rng.random() < 0.5: files[rng.choice(['', 'a/', 'b/']) + 'autorun.inf'] = b''
            d = tree(files)
            try:
                picked = pickexe.pick(d)
                s = install.choose(d, picked)
            finally: shutil.rmtree(d)
            if s is None: continue
            self.assertIn(s, files)
            self.assertFalse(install.NOT_SETUP.search(s), s)
            self.assertTrue(s == picked or install.is_setup_name(s), (s, picked, files))


class NotGame(unittest.TestCase):
    def why(self, files, name):
        d = tree(files)
        try: return pickexe.not_game(d, name)
        finally: shutil.rmtree(d)

    def test_movies_and_patches(self):
        # DLsite's sample movie zips, DMM's (xero_0038m.zip), a zip of nothing but an mpg
        self.assertEqual(self.why({'demomovie_high.mpg': 9, 'readme.txt': 1}, 'rsp_0061m.zip'), 'movie')
        self.assertEqual(self.why({'lmt3_cm_01.mpg': 9}, 'VJ009245_trial_mov.zip'), 'movie')
        self.assertEqual(self.why({'a.wmv': 9, 'b.jpg': 2}, 'whatever.zip'), 'movie')
        self.assertEqual(self.why({'data/patch.xp3': 9}, 'noble_trial_patch101.zip'), 'patch')
        self.assertEqual(self.why({'x.dat': 9}, 'xero_0038m.zip'), 'movie')
        # (a game is a game, whatever its name; data without a name that says anything: unknown)
        self.assertIsNone(self.why({'game.exe': 9, 'op.mpg': 99}, 'VJ1_trial_mov.zip'))
        self.assertIsNone(self.why({'x.dat': 9, 'op.mpg': 2}, 'ggs_0150t.zip'))

    def test_an_updater_and_the_new_exe(self):
        # #199-2 はたはに: hatahani_try_v101.zip holds update.exe and the new hatahani_try.exe only
        r = 'hatahani_try_v101/'
        self.assertEqual(self.why({r + 'hatahani_try.exe': 64, r + 'update.exe': 4}, 'hatahani_try_v101.zip'), 'patch')
        self.assertEqual(self.why({'アップデート.exe': 4, 'readme.txt': 1, 'krkr.dll': 3}, 'x.zip'), 'patch')
        # (a trial with its data beside, or one that ships an updater of its own, is a game; a
        # program of a few MB alone may be a game too)
        self.assertIsNone(self.why({r + 'hatahani_try.exe': 64, r + 'update.exe': 4, r + 'data.xp3': 900}, 'hatahani_try_v101.zip'))
        self.assertIsNone(self.why({'game.exe': -(4 << 20), 'updater_config.ini': 1}, 'x.zip'))
        self.assertIsNone(self.why({'game.exe': -(4 << 20), 'notupdate.exe': 1}, 'x.zip'))

    def test_data_makes_a_game(self):
        """Property: a tree with exes and no download name saying patch or movie is 'patch' exactly
        when it holds nothing but programs and readmes and either an updater or under NO_DATA bytes
        in all; any data file added makes it unknown (None)."""
        rng = random.Random(int(os.environ.get('SEED', '1')))
        progs = ['game.exe', 'update.exe', 'Patch101.exe', 'config.exe', 'a/updater.exe', 'krkr.dll', 'start.exe']
        extras = ['readme.txt', 'icon.ico', 'a/manual.html']
        datas = ['data.xp3', 'a/bgm.ogg', 'arc.pac', 'Game_Data/level0']
        for _ in range(300):
            # (sizes around NO_DATA, both sides of it, as sparse files)
            files = {p: -rng.choice([rng.randint(68, 500), rng.randint(68, 3 * pickexe.NO_DATA)])
                     for p in rng.sample(progs, rng.randint(1, 4))}
            files.update({p: 1 for p in rng.sample(extras, rng.randint(0, 2))})
            if not any(p.endswith('.exe') for p in files): files['game.exe'] = -99
            updater = any(pickexe.UPDATER.search(p.rsplit('/', 1)[-1]) for p in files)
            small = sum(abs(v) for v in files.values()) < pickexe.NO_DATA
            self.assertEqual(self.why(files, 'trial.zip'), 'patch' if updater or small else None, files)
            more = dict(files)
            more[rng.choice(datas)] = rng.randint(1, 500)
            self.assertIsNone(self.why(more, 'trial.zip'), more)

    def test_a_replacement_exe_or_a_joiner(self):
        # #198 シールプリンセス: sp_trial_exe_for_S3.zip is the trial's exe for S3 cards (632 KiB),
        # without the trial's data; ほしフル's Hoshiful_Preview.exe joins split pieces (9 KB)
        self.assertEqual(self.why({'シールプリンセスの欠片.exe': -647168}, 'sp_trial_exe_for_S3.zip'), 'patch')
        self.assertEqual(self.why({'Hoshiful_Preview.exe': -9488}, 'Hoshiful_Preview.exe'), 'patch')
        # (a trial in one big exe, 十六夜の花嫁's 28 MB self-extractor or 巣作りドラゴン's DragonT.exe, is no patch)
        self.assertIsNone(self.why({'DragonT.exe': -28063952}, 'DragonT.exe'))
        self.assertIsNone(self.why({'g.exe': -(1 << 20), 'g.dat': -(1 << 20)}, 'g.zip'))
        # (not a program image, only named .exe: no reason to say patch)
        self.assertIsNone(self.why({'g.exe': b'MZ' + b'\0' * 510}, 'g.zip'))

    @unittest.skipUnless(shutil.which('7z'), '7z')
    def test_a_split_download(self):
        """#198 SinsAbell sinsabell_bm.exe: a joiner with the first 11 MB of an LZH, the rest in
        .002 to .008. A program whose archive ends early is 'split'; the same one whole is a game."""
        import io, zipfile
        b = io.BytesIO()
        with zipfile.ZipFile(b, 'w', zipfile.ZIP_STORED) as z: z.writestr('bm/battle1.wdt', os.urandom(3 << 20))
        sfx = b'MZ' + b'\0' * 4094 + b.getvalue()
        self.assertIsNone(self.why({'sinsabell_bm.exe': sfx}, 'sinsabell_bm.exe'))
        for cut in (len(sfx) // 2, len(sfx) - 100, 4096 + 1000):
            if cut < pickexe.NO_DATA: continue
            self.assertEqual(self.why({'sinsabell_bm.exe': sfx[:cut]}, 'sinsabell_bm.exe'), 'split', cut)
            # (with data beside it, or inside a download of another name, it is a game whatever its archive)
            self.assertIsNone(self.why({'sinsabell_bm.exe': sfx[:cut], 'bm/a.wdt': 9}, 'sinsabell_bm.exe'))
            self.assertIsNone(self.why({'sinsabell_bm.exe': sfx[:cut]}, 'sinsabell.zip'))

    def test_prep_says_patch(self):
        """corpus-prep.sh on the はたはに patch's tree ends with exit 5 (not a game)."""
        t = tempfile.mkdtemp()
        try:
            x = os.path.join(t, 'hh', 'x', 'hatahani_try_v101')
            os.makedirs(x)
            for f, n in (('hatahani_try.exe', 64), ('update.exe', 4)):
                with open(os.path.join(x, f), 'wb') as o: o.write(b'MZ' + b'\0' * n)
            r = subprocess.run(['bash', os.path.join(os.path.dirname(HERE), 'corpus-prep.sh'), 'hh',
                                'http://127.0.0.1:9/hatahani_try_v101.zip', '', ''],
                               env={k: v for k, v in dict(os.environ, TRIALS=t).items() if k != 'SAKE_NODE'},
                               capture_output=True, text=True, timeout=60)
            self.assertEqual(r.returncode, 5, r.stderr)
            self.assertIn('not a game: patch', r.stderr)
        finally:
            shutil.rmtree(t)


def sake_node():
    for p in (os.environ.get('SAKE_NODE'), f'{ROOT}/target/sake-out/sake.js'):
        if p and os.path.exists(p): return p
    return None


@unittest.skipUnless(sake_node() and shutil.which('i686-w64-mingw32-gcc') and shutil.which('node'), 'needs sake.js for node, node and mingw')
class EndToEnd(unittest.TestCase):
    """host/tests/autoinstall.c as a trial's setup program: answered by SAKE_AUTO, installed on
    the trial's C:\\, the installed game chosen."""

    def setUp(self):
        self.t = tempfile.mkdtemp()
        self.setup = os.path.join(self.t, 'test_setup.exe')
        subprocess.run(['i686-w64-mingw32-gcc', '-O2', f'{ROOT}/host/tests/autoinstall.c', '-o', self.setup, '-luser32', '-lgdi32'], check=True)
        self.env = dict(os.environ, SAKE_NODE=sake_node(), INSTALL_TIMEOUT='300', INSTALL_IDLE='30')

    def tearDown(self):
        shutil.rmtree(self.t)

    def test_install_run(self):
        x = os.path.join(self.t, 'x')
        os.makedirs(x)
        shutil.copy(self.setup, x)
        r = subprocess.run([sys.executable, f'{HERE}/install.py', 'run', x, 'test_setup.exe', os.path.join(self.t, 'c')],
                           env=self.env, capture_output=True, text=True, timeout=400)
        self.assertEqual((r.returncode, r.stdout), (0, 'C\tautoinstall/game.exe\n'), r.stderr + open(os.path.join(self.t, 'install.log')).read()[-2000:])

    @unittest.skipUnless(shutil.which('masu'), 'needs masu on PATH for the store')
    def test_corpus_prep(self):
        trials, base = os.path.join(self.t, 'trials'), os.path.join(self.t, 'base')
        fonts = os.path.join(self.t, 'fonts')
        os.makedirs(os.path.join(trials, 'auto'))
        os.makedirs(fonts)
        open(os.path.join(fonts, 'f.ttf'), 'wb').write(b'font')
        p = os.path.join(self.t, 'p', 'drive_c', 'windows', 'temp')
        os.makedirs(p)
        subprocess.run(['masu', '--store', base, 'pack', os.path.join(self.t, 'p'), 'prefix32'], check=True, capture_output=True)
        shutil.copy(self.setup, os.path.join(trials, 'auto', 'dl.exe'))
        store = os.path.join(self.t, 'store')
        r = subprocess.run(['bash', f'{ROOT}/tests/corpus-prep.sh', 'auto', 'http://127.0.0.1:9/test_setup.exe', '', store],
                           env=dict(self.env, TRIALS=trials, BASE_STORE=base, SAKE_FONTS=fonts), capture_output=True, text=True, timeout=600)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip().splitlines()[-1], 'C:\\autoinstall\\game.exe')
        tsv = open(os.path.join(store, 'trees', 'prefix32.tsv'), encoding='utf-8').read()
        self.assertIn('drive_c/autoinstall/game.exe', tsv)
        # (the fonts in the Windows folder sake made, whatever its case: one folder, not two)
        self.assertIn('drive_c/windows/fonts/f.ttf', tsv.lower())
        self.assertEqual(sum(1 for l in tsv.splitlines() if l.lower().split('\t')[-1:] == ['drive_c/windows']), 1)
        # (the base store's own list is untouched: hard links are not written through)
        self.assertNotIn('autoinstall', open(os.path.join(base, 'trees', 'prefix32.tsv'), encoding='utf-8').read())


if __name__ == '__main__':
    unittest.main()
