#!/usr/bin/env python3
"""tests/corpus/winspeed_check.py's verdict and tests/corpus/cpu_attrib.py (property, seeded):
a row is compared only when both sides stopped at the same title screen (and the row's
title_sig, when it has one); a warning page on one side is caught; cores of different story
screens are marked; cpu_attrib's buckets add up to the log's totals whatever the log holds.

  python3 tests/corpus/winspeed_check_test.py"""
import os, random, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cpu_attrib, shots, winspeed_check


def sig_of(r, base=None, noise=0):
    """a random signature, or `base` moved by up to `noise` steps per channel"""
    if base is None:
        return ''.join('%x' % r.randrange(16) for _ in range(shots.SIG_X * shots.SIG_Y * 3))
    return ''.join('%x' % max(0, min(15, int(c, 16) + r.randrange(-noise, noise + 1))) for c in base)


class Verdict(unittest.TestCase):
    def test_property(self):
        r = random.Random(17)
        for run in range(1500):
            title, warning, story = sig_of(r), sig_of(r), sig_of(r)
            # (Windows: 1-2 runs; each at the title, or at the warning page, or nowhere)
            win = []
            for _ in range(1 + r.randrange(2)):
                at = r.choice(['title', 'title', 'warning', None])
                res = None if at is None else {'title': round(r.uniform(2, 30), 1), 'leftTitle': r.random() < 0.8,
                                               'storyCores': 0.1, 'storySigs': [story] if r.random() < 0.8 else [title]}
                win.append((res, {'title': sig_of(r, title, 1), 'warning': warning, None: None}[at]))
            m_at = r.choice(['title', 'title', 'warning', None])
            mres = None if m_at is None else {'title': round(r.uniform(5, 90), 1), 'leftTitle': r.random() < 0.8,
                                              'storyCores': 0.9, 'storySigs': [story] if r.random() < 0.8 else [title]}
            msig = {'title': sig_of(r, title, 1), 'warning': warning, None: None}[m_at]
            row = {'title_sig': title if r.random() < 0.5 else '', 'story_sig': story if r.random() < 0.5 else ''}
            v = winspeed_check.verdict(row, win, (mres, msig))
            ctx = (run, [w[1] == warning for w in win], m_at, bool(row['title_sig']), v)
            wins_at = [w for w in win if w[0] is not None]
            same = any(w[1] is not None and shots.sig_same(w[1], msig) for w in wins_at)
            want_title = mres is not None and same and (not row['title_sig'] or shots.sig_same(title, msig))
            self.assertEqual(v['title'], want_title, ctx)
            # (a ratio only for the same screens; a warning page on masu with a title_sig is never "the same")
            self.assertEqual(v['ratio'] is not None, want_title, ctx)
            if row['title_sig'] and m_at == 'warning':
                self.assertFalse(v['title'], ctx)
            if v['story']:
                self.assertTrue(v['title'] and mres['leftTitle'], ctx)
                if row['story_sig']:
                    self.assertIn(story, mres['storySigs'], ctx)
            if not v['title']:
                self.assertTrue(v['why'], ctx)

    def test_cli(self):
        import io, json, contextlib
        r = random.Random(5)
        title, warning = sig_of(r), sig_of(r)
        rows = winspeed_check.csv.DictReader(open(os.path.join(winspeed_check.HERE, 'winspeed.tsv'), encoding='utf-8'), delimiter='\t')
        a, b = [x['id'] for x in list(rows)[:2]]
        with tempfile.TemporaryDirectory() as d:
            def put(p, res):
                os.makedirs(p)
                with open(os.path.join(p, 'result.json'), 'w') as f: json.dump(res, f)
            put(f'{d}/win/{a}-1', {'title': 3.0, 'titleSig': title, 'leftTitle': True, 'storyCores': 0.01})
            put(f'{d}/masu/{a}', {'title': 15.0, 'titleSig': sig_of(r, title, 1), 'leftTitle': True, 'storyCores': 0.03})
            put(f'{d}/win/{b}-1', {'title': 5.0, 'titleSig': title, 'leftTitle': True})
            put(f'{d}/masu/{b}', {'title': 4.0, 'titleSig': warning, 'leftTitle': False})
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = winspeed_check.main([f'{d}/win', f'{d}/masu', a, b])
            t = out.getvalue()
            self.assertEqual(rc, 1)
            self.assertRegex(t, rf'\| {a} \|[^\n]*\| yes \| yes \| 3.0 \| 15.0 \| 5.0 \|')
            self.assertRegex(t, rf'\| {b} \|[^\n]*\| NO \|[^\n]*title screens differ')


class Attrib(unittest.TestCase):
    APIS = ['user32!PeekMessageA', 'user32!PeekMessageW', 'winmm!timeGetTime', 'kernel32!Sleep',
            'kernel32!WaitForSingleObject', 'gdi32!BitBlt', 'd3d9!Present', 'kernel32!ReadFile',
            'kernel32!EnterCriticalSection', 'dsound!DirectSoundCreate', 'user32!DefWindowProcA', 'user32!GetMessageW']

    def test_buckets(self):
        b = cpu_attrib.bucket
        self.assertEqual([b(a) for a in self.APIS],
                         ['poll', 'poll', 'poll', 'wait', 'wait', 'present', 'present', 'file', 'sync', 'sound', 'other', 'wait'])

    def test_property(self):
        r = random.Random(23)
        for run in range(300):
            lines, tot_calls, tot_ms, secs = [], 0, 0, 0.0
            for _ in range(1 + r.randrange(5)):
                s = r.choice([5.0, 2.0, 10.0])
                calls = {a: r.randrange(1, 5000) for a in r.sample(self.APIS, r.randrange(1, len(self.APIS)))}
                ms = {a: r.randrange(0, 3000) for a in calls if r.random() < 0.7}
                extra_c, extra_m = r.randrange(500), r.randrange(100)
                tc, tm = sum(calls.values()) + extra_c, sum(ms.values()) + extra_m
                lines.append(f"sake api {tc} calls/{s:g}s: " + ' '.join(f'{a}={n}' for a, n in calls.items()))
                if r.random() < 0.3: lines.append('page stats/5.0s: noise=1 x=2ms')
                lines.append(f"sake api time {tm}ms/{s:g}s: " + ' '.join(f'{a}={v}ms' for a, v in ms.items()))
                tot_calls, tot_ms, secs = tot_calls + tc, tot_ms + tm, secs + s
            ws = cpu_attrib.windows(lines)
            a = cpu_attrib.attribute(ws, cores=1.0)
            self.assertAlmostEqual(sum(v[0] for v in a['buckets'].values()), round(tot_calls / secs, 1), delta=1, msg=run)
            self.assertAlmostEqual(sum(v[1] for v in a['buckets'].values()), round(tot_ms / secs, 1), delta=1, msg=run)
            self.assertGreaterEqual(a['guest'], 0)
            busy = sum(v[1] for k, v in a['buckets'].items() if k != 'wait')
            self.assertAlmostEqual(a['guest'], max(0.0, 1000 - busy), delta=0.2)

    def test_polling_waits_and_cli(self):
        lines = ['sake api 10000 calls/5s: kernel32!Sleep=6000 user32!PeekMessageA=4000',
                 'sake api time 900ms/5s: kernel32!Sleep=800ms user32!PeekMessageA=100ms']
        a = cpu_attrib.attribute(cpu_attrib.windows(lines))
        self.assertEqual(a['polling_waits'], ['kernel32!Sleep'])
        self.assertEqual(a['buckets']['poll'], [800.0, 20.0])
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, 'console.log')
            with open(p, 'w') as f: f.write('\n'.join(lines * 3))
            self.assertEqual(cpu_attrib.main([p, '--cores', '0.9', '--from', '5']), 0)
            with open(p, 'w') as f: f.write('nothing here\n')
            self.assertEqual(cpu_attrib.main([p]), 1)


if __name__ == '__main__':
    unittest.main()
