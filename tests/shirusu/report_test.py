#!/usr/bin/env python3
"""tests/shirusu/report.py's reading of shirusu's JSON (shapes as shirusu-conformance and
shirusu-compare --json print them) and of the plays' result.json"""
import json, os, sys, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import report


class Report(unittest.TestCase):
    def test_reached(self):
        self.assertEqual(report.reached(None), '-')
        self.assertEqual(report.reached({'title': None}), 'none')
        self.assertEqual(report.reached({'title': 12.0, 'leftTitle': False}), 'title')
        self.assertEqual(report.reached({'title': 12.0, 'leftTitle': True}), 'story')

    def test_conformant(self):
        ok = {'conformant': True, 'checks': [{'id': 'clean-scan', 'status': 'pass'}, {'id': 'x', 'status': 'warn'}]}
        bad = {'conformant': False, 'checks': [{'id': 'enter-leave-pairing', 'status': 'fail'}]}
        self.assertEqual(report.conformant(ok), (True, []))
        self.assertEqual(report.conformant(bad), (False, ['enter-leave-pairing']))
        self.assertEqual(report.conformant(None), (None, []))

    def test_api_table(self):
        cmp = {'api_counts': {'kernel32!ReadFile': {'left': 3, 'right': 1}, 'odd': 5}}
        self.assertEqual(report.api_table(cmp), {'kernel32!ReadFile': (3, 1)})
        self.assertEqual(report.api_table(None), {})

    def test_recurring(self):
        rows = [{'id': 'a', 'candidates': [{'kind': 'missing-call', 'api': 'kernel32!ReadFile', 'severity': 'high'},
                                           {'kind': 'missing-call', 'api': 'kernel32!ReadFile', 'severity': 'high'}]},
                {'id': 'b', 'candidates': [{'kind': 'missing-call', 'api': 'kernel32!ReadFile', 'severity': 'low'},
                                           {'kind': 'extra-call', 'api': None, 'severity': 'info'}]}]
        rec, sev = report.summarize(rows)
        self.assertEqual(rec[0], {'kind': 'missing-call', 'api': 'kernel32!ReadFile', 'titles': ['a', 'b']})
        self.assertEqual(rec[1]['api'], '-')
        self.assertEqual(sev, {'high': 2, 'low': 1, 'info': 1})
        md = report.markdown([dict(r, win_reached='title', sake_reached='story', win_conformant=True, sake_conformant=None, notes=[]) for r in rows], rec, sev)
        self.assertIn('| a | - | title | story | - | - | ok/- | 2 | 0 | 0 | 0 |', md)

    def test_bits(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, 'meta.txt')
            open(p, 'w').write('d3d9\n64\ngame.exe\n')
            self.assertEqual(report.bits_of(p), 64)
            open(p, 'w').write('none\n32\ngame.exe\n')
            self.assertEqual(report.bits_of(p), 32)
            self.assertEqual(report.bits_of(os.path.join(d, 'none')), 32)


class Gate(unittest.TestCase):
    """report.gate against report.baseline over random runs (property, seeded): a run is never
    held against its own baseline; a sake that got less far, or a new high candidate, always is;
    a title without a baseline only needs a result."""
    def rows(self, rnd, n):
        kinds = ['missing-call', 'extra-call', 'result-differs', 'bytes-differ']
        apis = ['kernel32!ReadFile', 'kernel32!CreateFileW', 'user32!GetMessageW', None]
        out = []
        for i in range(n):
            out.append({'id': f't{i}', 'sake_reached': rnd.choice(['none', 'title', 'story']),
                        'win_reached': rnd.choice(['title', 'story']), 'win_conformant': True,
                        'candidates': [{'kind': rnd.choice(kinds), 'api': rnd.choice(apis),
                                        'severity': rnd.choice(['high', 'medium', 'low', 'info'])} for _ in range(rnd.randrange(6))]})
        return out

    def test_property(self):
        import copy, random
        rnd = random.Random(14)
        for run in range(400):
            rows = self.rows(rnd, 1 + rnd.randrange(5))
            base = report.baseline(rows)
            self.assertEqual(report.gate(rows, base), [], run)
            # (fewer candidates, or one more of a lower severity: still fine)
            fine = copy.deepcopy(rows)
            for r in fine:
                r['candidates'] = r['candidates'][:rnd.randrange(len(r['candidates']) + 1)]
                r['candidates'].append({'kind': 'extra-call', 'api': 'x!Y', 'severity': 'low'})
            self.assertEqual(report.gate(fine, base), [], run)
            worse = copy.deepcopy(rows)
            r = rnd.choice(worse)
            if r['sake_reached'] != 'none' and rnd.random() < 0.5:
                r['sake_reached'] = 'none'
                want = 'sake reached none'
            else:
                r['candidates'].append({'kind': 'missing-call', 'api': 'gdi32!NewThing', 'severity': 'high'})
                want = 'new high candidates missing-call|gdi32!NewThing'
            bad = report.gate(worse, base)
            self.assertTrue(any(r['id'] in b and want in b for b in bad), (run, bad))
            # (a Windows side that did not run says nothing of sake's candidates)
            if 'candidates' in want:
                r['win_reached'] = '-'
                self.assertEqual(report.gate(worse, base), [], run)
            # (no baseline: only a result is needed)
            self.assertEqual(report.gate(worse, {}), [], run)
            gone = copy.deepcopy(rows)
            gone[0]['sake_reached'] = '-'
            self.assertIn('t0: sake has no result', report.gate(gone, {}))

    def test_baseline_cli(self):
        import tempfile, subprocess
        rows = [{'id': 'a', 'sake_reached': 'story', 'candidates': [{'kind': 'k', 'api': None, 'severity': 'high'}]}]
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, 'summary.json')
            json.dump({'titles': rows}, open(p, 'w'))
            out = subprocess.run([sys.executable, '-I', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'report.py'), '--baseline', p],
                                 capture_output=True, text=True, check=True).stdout
            self.assertEqual(json.loads(out), {'a': {'sake_reached': 'story', 'high': ['k|-']}})


class Probes(unittest.TestCase):
    def test_probes_are_games(self):
        here = os.path.dirname(os.path.abspath(__file__))
        ids = lambda f: [l.split('#')[0].split()[0] for l in open(os.path.join(here, f), encoding='utf-8') if l.split('#')[0].strip()]
        probes = ids('probes.txt')
        self.assertTrue(3 <= len(probes) <= 8, probes)
        self.assertEqual(sorted(set(probes) - set(ids('games.txt'))), [])
        self.assertIsInstance(json.load(open(os.path.join(here, 'probe-baseline.json'))), dict)


if __name__ == '__main__':
    unittest.main()
