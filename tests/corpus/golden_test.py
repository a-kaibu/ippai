#!/usr/bin/env python3
"""Tests of tests/corpus/golden.py and of check.py's `look` grading, end to end on made-up runs:
Windows' runs imported (halved pictures, trimmed result), sake's shots compared, the levels
none/title/steps, regressions below the `look` column, --update raising it, the look.json cache
standing in for PNGs the corpus artifact does not carry, and `todo`.

  python3 tests/corpus/golden_test.py
"""
import csv, json, os, random, shutil, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import golden, shots
from shots_test import rand_pic, paint


def put(d, tid, result, pics):
    os.makedirs(f'{d}/{tid}', exist_ok=True)
    json.dump(result, open(f'{d}/{tid}/result.json', 'w'))
    for name, p in pics.items(): shots.write(f'{d}/{tid}/{name}.png', *p)


TITLE = rand_pic(random.Random(1), 640, 480, True)
STEPS = rand_pic(random.Random(2), 640, 480, True)
BLACK = (640, 480, bytes(640 * 480 * 3))
OK = {'title': 3.0, 'leftTitle': True, 'storyFrames': 2, 'crashed': False}

HEAD = ['id', 'title', 'engine', 'url', 'exe_rel', 'steps', 'expect', 'smoke', 'look']


def titles(path, rows):
    with open(path, 'w', encoding='utf-8', newline='') as f:
        w = csv.DictWriter(f, fieldnames=HEAD, delimiter='\t', lineterminator='\n')
        w.writeheader()
        for r in rows: w.writerow({k: r.get(k, '') for k in HEAD})


def check(tsv, out, g, *extra):
    p = subprocess.run([sys.executable, f'{HERE}/check.py', tsv, out, *extra], capture_output=True, text=True,
                       env=dict(os.environ, CORPUS_GOLDEN=g))
    return p.returncode, p.stdout + p.stderr


class Golden(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.d = self.tmp.name
        self.g, self.win, self.out = f'{self.d}/golden', f'{self.d}/win', f'{self.d}/out'
        os.makedirs(self.g)
        # Windows: a reached its story, b showed no title (nothing to compare), c only a title
        put(self.win, 'a', dict(OK, size=[640, 480], junk='x' * 100), {'title': TITLE, 'steps': STEPS, 'story-20': STEPS})
        put(self.win, 'b', {'title': None}, {'notitle': BLACK})
        put(self.win, 'c', dict(OK), {'title': TITLE})
        self.assertEqual(golden.import_runs(self.win, self.g), ['a', 'c'])

    def tearDown(self):
        self.tmp.cleanup()

    def test_import(self):
        ra = json.load(open(f'{self.g}/a/result.json'))
        self.assertEqual(ra['shots'], ['title', 'steps'])
        self.assertNotIn('junk', ra)
        self.assertEqual(sorted(os.listdir(f'{self.g}/a')), ['result.json', 'steps.png', 'title.png'])
        self.assertEqual(shots.read(f'{self.g}/a/title.png'), shots.half(*TITLE))
        # no title screen on Windows: no answer (played again next time)
        self.assertFalse(os.path.exists(f'{self.g}/b'))
        # imported again: the new answer replaces the old one; a failed replay keeps the old one
        shutil.rmtree(f'{self.win}/a')
        put(self.win, 'a', dict(OK), {'title': STEPS})
        golden.import_runs(self.win, self.g)
        self.assertEqual(sorted(os.listdir(f'{self.g}/a')), ['result.json', 'title.png'])
        self.assertEqual(shots.read(f'{self.g}/a/title.png'), shots.half(*STEPS))
        put(self.win, 'a', {'title': None}, {})
        golden.import_runs(self.win, self.g)
        self.assertEqual(shots.read(f'{self.g}/a/title.png'), shots.half(*STEPS))

    def test_look_levels(self):
        put(self.out, 'a', OK, {'title': TITLE, 'steps': STEPS})
        self.assertEqual(golden.look('a', self.out, self.g)['level'], 'steps')
        put(self.out, 'a', OK, {'title': TITLE, 'steps': BLACK})
        r = golden.look('a', self.out, self.g)
        self.assertEqual(r['level'], 'title')
        self.assertIn('steps:', r['why'])
        put(self.out, 'a', OK, {'title': paint(TITLE, 0, 0, 640, 200, (255, 0, 0)), 'steps': STEPS})
        self.assertEqual(golden.look('a', self.out, self.g)['level'], 'none')
        # no shot on sake at all
        put(self.out, 'z', {'title': None}, {})
        os.makedirs(f'{self.g}/z'); json.dump({'shots': ['title']}, open(f'{self.g}/z/result.json', 'w'))
        self.assertEqual(golden.look('z', self.out, self.g)['level'], 'none')
        # nothing to compare with (an answer without pictures, or none)
        put(self.out, 'b', OK, {'title': TITLE})
        os.makedirs(f'{self.g}/b'); json.dump({'shots': []}, open(f'{self.g}/b/result.json', 'w'))
        self.assertIsNone(golden.look('b', self.out, self.g))
        self.assertIsNone(golden.look('nope', self.out, self.g))

    def test_cache_stands_in_for_pngs(self):
        put(self.out, 'a', OK, {'title': TITLE, 'steps': STEPS})
        first = golden.look('a', self.out, self.g)
        for n in ('title', 'steps'): os.remove(f'{self.out}/a/{n}.png')
        self.assertEqual(golden.look('a', self.out, self.g), first)

    def test_check_grades_and_updates(self):
        tsv = f'{self.d}/titles.tsv'
        rows = [{'id': 'a', 'expect': 'story', 'look': 'steps'}, {'id': 'c', 'expect': 'title', 'look': ''},
                {'id': 'b', 'expect': 'story'}]
        titles(tsv, rows)
        put(self.out, 'a', OK, {'title': TITLE, 'steps': STEPS})
        put(self.out, 'b', OK, {'title': TITLE, 'steps': STEPS})
        put(self.out, 'c', OK, {'title': TITLE, 'steps': BLACK})
        code, text = check(tsv, self.out, self.g)
        self.assertEqual(code, 0, text)
        self.assertIn('⬆ looks better', text)  # c: the title matches, the column says nothing yet
        # a's steps screen broke: a regression though it still reaches the story
        put(self.out, 'a', OK, {'title': TITLE, 'steps': BLACK})
        code, text = check(tsv, self.out, self.g, '--regressions', f'{self.d}/re.txt')
        self.assertEqual(code, 1, text)
        self.assertEqual(open(f'{self.d}/re.txt').read(), 'a\n')
        # --update raises c to title, never lowers a
        check(tsv, self.out, self.g, '--update')
        got = {r['id']: r['look'] for r in csv.DictReader(open(tsv, encoding='utf-8'), delimiter='\t')}
        self.assertEqual(got, {'a': 'steps', 'c': 'title', 'b': ''})
        # --update set takes them as they are
        check(tsv, self.out, self.g, '--update', 'set')
        got = {r['id']: r['look'] for r in csv.DictReader(open(tsv, encoding='utf-8'), delimiter='\t')}
        self.assertEqual(got, {'a': 'title', 'c': 'title', 'b': ''})

    def test_todo(self):
        tsv = f'{self.d}/titles.tsv'
        titles(tsv, [{'id': 'a', 'smoke': '1'}, {'id': 'x', 'smoke': '1'}, {'id': 'y'}])
        self.assertEqual(golden.todo(tsv, 'smoke', golden=self.g), ['x'])
        # b had no title screen on Windows: still to play
        titles(tsv, [{'id': 'b', 'smoke': '1'}])
        self.assertEqual(golden.todo(tsv, 'smoke', golden=self.g), ['b'])
        titles(tsv, [{'id': 'a', 'smoke': '1'}, {'id': 'x', 'smoke': '1'}, {'id': 'y'}])
        self.assertEqual(golden.todo(tsv, 'full', golden=self.g), ['x', 'y'])
        self.assertEqual(golden.todo(tsv, 'full', again=True, golden=self.g), ['a', 'x', 'y'])
        self.assertEqual(golden.todo(tsv, only=['a', 'y'], golden=self.g), ['y'])
        with self.assertRaises(SystemExit): golden.todo(tsv, only=['q'], golden=self.g)


if __name__ == '__main__':
    unittest.main()
