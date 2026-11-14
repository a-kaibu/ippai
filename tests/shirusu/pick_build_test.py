#!/usr/bin/env python3
"""tests/shirusu/pick_build.py's choice over random histories of main (property, seeded): the
build tested is the commit's own when it has a good one; else, once its own was cancelled, the
nearest good later build that has it in it; never an earlier commit's; a failed own build is
not hidden; and it waits while an answer may still come."""
import os, random, sys, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pick_build


class Choose(unittest.TestCase):
    def test_property(self):
        rnd = random.Random(2026)
        for run in range(3000):
            n = 2 + rnd.randrange(10)
            # (a straight line of commits on main: c0 oldest; a contains b when b is not newer)
            shas = [f'{i:040x}' for i in range(n)]
            pos = {s: i for i, s in enumerate(shas)}
            contains = lambda a, b: pos[a] >= pos[b]
            runs = []
            for i in range(n):
                for _ in range(rnd.choice([0, 1, 1, 1, 2])):
                    st = rnd.choice(['completed', 'completed', 'completed', 'in_progress'])
                    concl = rnd.choice(['success', 'cancelled', 'cancelled', 'failure']) if st == 'completed' else ''
                    runs.append({'databaseId': len(runs), 'headSha': shas[i], 'status': st, 'conclusion': concl})
            runs.reverse()
            sha = shas[rnd.randrange(n)]
            got, how = pick_build.choose(runs, sha, contains)
            own = [r for r in runs if r['headSha'] == sha]
            good_own = [r for r in own if r['conclusion'] == 'success']
            later = [r for r in runs if pos[r['headSha']] > pos[sha] and r['conclusion'] == 'success']
            ctx = (run, sha[-2:], how, [(r['headSha'][-2:], r['status'], r['conclusion']) for r in runs])
            if good_own:
                self.assertEqual((got['headSha'], how), (sha, 'exact'), ctx)
                continue
            if got not in ('wait', None):
                # (never an earlier commit's, never one without sha in it; the nearest later)
                self.assertEqual(how, 'later', ctx)
                self.assertGreater(pos[got['headSha']], pos[sha], ctx)
                self.assertEqual(pos[got['headSha']], min(pos[r['headSha']] for r in later), ctx)
                self.assertFalse(own and any(r['status'] != 'completed' for r in own), ctx)
            elif got is None:
                self.assertTrue(own and all(r['status'] == 'completed' for r in own), ctx)
                self.assertIn('failure', [r['conclusion'] for r in own], ctx)
                self.assertIn('failed', how, ctx)
            else:
                # (waiting: its own still running, or nothing good yet)
                self.assertTrue(any(r['status'] != 'completed' for r in own) or not later, ctx)


if __name__ == '__main__':
    unittest.main()
