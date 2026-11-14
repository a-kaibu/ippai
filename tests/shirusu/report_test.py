#!/usr/bin/env python3
"""tests/shirusu/report.py's reading of shirusu's JSON (shapes as shirusu-conformance and
shirusu-compare --json print them) and of the plays' result.json"""
import os, sys, unittest
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
        self.assertIn('| a | title | story | - | - | ok/- | 2 | 0 | 0 | 0 |', md)

    def test_bits(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, 'meta.txt')
            open(p, 'w').write('d3d9\n64\ngame.exe\n')
            self.assertEqual(report.bits_of(p), 64)
            open(p, 'w').write('none\n32\ngame.exe\n')
            self.assertEqual(report.bits_of(p), 32)
            self.assertEqual(report.bits_of(os.path.join(d, 'none')), 32)


if __name__ == '__main__':
    unittest.main()
