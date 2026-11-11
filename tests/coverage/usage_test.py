#!/usr/bin/env python3
"""Tests of tests/coverage/usage.py: a game's log counts by its last tally line only, a tests'
file by every line (a process each: IPPAI_API_COUNTS_OUT appends one at each exit), and the
report's tested column is the keys both sides have (a property test over random tallies,
fixed seed); end to end through the command line on files as CI writes them.

  python3 tests/coverage/usage_test.py
"""
import os, random, subprocess, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import usage


def line(t):
    return 'api-counts' + ''.join(f' {k}={n}' for k, n in sorted(t.items())) + '\n'


class Usage(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp(prefix='usage-test-')

    def write(self, rel, text):
        p = os.path.join(self.d, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, 'w') as f:
            f.write(text)
        return p

    def test_game_log_last_line(self):
        p = self.write('g/a/console.log', 'sake: x\n' + line({'api:k!A': 1}) + 'noise\nsake: api-counts api:k!A=5 api:k!B=2\n')
        self.assertEqual(usage.tally(p), {'api:k!A': 5, 'api:k!B': 2})
        self.assertIsNone(usage.tally(self.write('g/b/console.log', 'nothing\n')))

    def test_tests_file_every_line(self):
        p = self.write('t/host-tests.txt', line({'api:k!A': 1}) + line({'api:u!B': 3}) + line({}))
        self.assertEqual(usage.reached(p), {'api:k!A', 'api:u!B'})

    def test_random_tallies(self):
        r = random.Random(1)
        keys = [f'api:k32!F{i}' for i in range(40)] + [f'd3d9:IDirect3DDevice9::M{i}' for i in range(10)]
        for _ in range(50):
            procs = [{k: r.randrange(1, 9) for k in r.sample(keys, r.randrange(0, 8))} for _ in range(r.randrange(1, 6))]
            p = self.write('t/x.txt', ''.join(line(t) for t in procs))
            want = set().union(*map(set, procs))
            self.assertEqual(usage.reached(p), want)

    def test_report(self):
        self.write('g/one/console.log', line({'api:k!A': 4, 'api:k!B': 1, 'd3d9:D::Present': 9}))
        self.write('g/two/console.log', line({'api:k!A': 2, 'stub:k!C': 1}))
        self.write('t/ci/api-counts-host-tests/host-tests.txt', line({'api:k!B': 1}) + line({'api:k!A': 7}))
        self.write('t/sake.txt', line({'d3d9:D::Present': 1}))
        out = subprocess.run([sys.executable, '-I', os.path.join(HERE, 'usage.py'), '--games', os.path.join(self.d, 'g'), '--tests', os.path.join(self.d, 't')],
                             capture_output=True, text=True, check=True).stdout
        self.assertIn('2 game runs, 4 entry points they call, 3 the tests call.', out)
        self.assertIn('| api | 2 | 2 | 100.0% | 100.0% |', out)
        self.assertIn('| d3d9 | 1 | 1 | 100.0% | 100.0% |', out)
        self.assertIn('| stub | 1 | 0 | 0.0% | 0.0% |', out)


if __name__ == '__main__':
    unittest.main()
