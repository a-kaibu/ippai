#!/usr/bin/env python3
"""Run the real corpus grader against successful, unavailable and regressed titles."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent

class PreparationCoverage(unittest.TestCase):
    def grade(self, prepared, unavailable, regression=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            rows = ['id\tengine\texpect\tlook']
            for i in range(prepared + unavailable):
                name = f'coverage-fixture-{i}'
                rows.append(f'{name}\ttest\ttitle\t')
                d = root / 'results' / name
                d.mkdir(parents=True)
                r = {'prep_error': 'download timeout'} if i >= prepared else {'title': None if regression and i == 0 else 1.0}
                (d / 'result.json').write_text(json.dumps(r))
            # Keep a title table even when the output directory is empty.
            if len(rows) == 1:
                rows.append('coverage-fixture-0\ttest\ttitle\t')
            titles = root / 'titles.tsv'
            titles.write_text('\n'.join(rows) + '\n')
            return subprocess.run([sys.executable, str(HERE / 'check.py'), str(titles), str(root / 'results'), '--min-prepared-ratio', '0.8'], capture_output=True, text=True)

    def test_smoke_with_three_of_sixteen_is_inconclusive(self):
        p = self.grade(3, 13)
        self.assertEqual(p.returncode, 2, p.stderr)
        self.assertIn('3/16', p.stdout)
        self.assertIn('INCONCLUSIVE', p.stdout)
        self.assertIn('regressions 0', p.stdout)

    def test_threshold_boundary(self):
        self.assertEqual(self.grade(4, 1).returncode, 0)
        self.assertEqual(self.grade(3, 1).returncode, 2)

    def test_empty_and_all_unavailable_are_inconclusive(self):
        self.assertEqual(self.grade(0, 0).returncode, 2)
        self.assertEqual(self.grade(0, 4).returncode, 2)

    def test_regression_is_not_downgraded_to_inconclusive(self):
        p = self.grade(1, 4, regression=True)
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn('regressions 1', p.stdout)

if __name__ == '__main__':
    unittest.main()
