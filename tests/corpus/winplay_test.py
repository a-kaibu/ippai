#!/usr/bin/env python3
"""Tests of the platform-free parts of tests/corpus/winplay.py: the steps grammar (every row of
titles.tsv must parse, and parse as corpus.mjs splits it), client coordinates, the title-screen
statistics, and the activeCodePage manifest rewrite (well-formed XML, exactly one
activeCodePage ja-JP, the embedded manifest's other settings kept).

  python3 tests/corpus/winplay_test.py
"""
import csv, os, random, sys, unittest
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import winplay

NS = '{' + winplay.MANIFEST_NS + '}'


class Steps(unittest.TestCase):
    def test_every_title_parses(self):
        rows = list(csv.DictReader(open(f'{HERE}/titles.tsv', encoding='utf-8'), delimiter='\t'))
        n = 0
        for r in rows:
            s = r.get('steps') or ''
            plan = winplay.parse_steps(s)
            # (as corpus.mjs: split on blanks and ';', op before the first ':')
            toks = [t for t in s.replace(';', ' ').split() if t]
            self.assertEqual([op for op, _ in plan], [t.split(':')[0] for t in toks], r['id'])
            n += len(plan)
            for k in ('pre', 'story_at'):
                if r.get(k): self.assertEqual(len([float(v) for v in r[k].split(',')]), 2, (r['id'], k))
        self.assertGreater(n, 50)

    def test_grammar(self):
        self.assertEqual(winplay.parse_steps('click:1,2 wait:3.5;key:Return  move:0.5,0.25 shot:a'),
                         [('click', (1.0, 2.0)), ('wait', 3.5), ('key', 'Return'), ('move', (0.5, 0.25)), ('shot', 'a')])
        self.assertEqual(winplay.parse_steps(''), [])
        for bad in ('tap:1,2', 'click:1', 'wait:x', 'click'):
            with self.assertRaises(ValueError): winplay.parse_steps(bad)

    def test_client_xy(self):
        self.assertEqual(winplay.client_xy(0.385, 0.9, 800, 600), (308, 540))
        self.assertEqual(winplay.client_xy(120, 316, 800, 600), (120, 316))
        r = random.Random(1)
        for _ in range(200):
            w, h = r.randrange(100, 2000), r.randrange(100, 2000)
            fx, fy = r.random(), r.random()
            x, y = winplay.client_xy(fx, fy, w, h)
            self.assertTrue(0 <= x <= w and 0 <= y <= h)


class Meta(unittest.TestCase):
    def test_meta_of_rows(self):
        m = winplay.meta(f'{HERE}/titles.tsv', 'b2015_guilty', 'a/b.exe')
        self.assertEqual(m['steps'], 'click:0.385,0.9 wait:6')
        self.assertEqual(m['exe'], 'a/b.exe')
        # a row without steps gets corpus-run.py's default
        rows = list(csv.DictReader(open(f'{HERE}/titles.tsv', encoding='utf-8'), delimiter='\t'))
        bare = next(r['id'] for r in rows if not r['steps'])
        self.assertEqual(winplay.meta(f'{HERE}/titles.tsv', bare, 'x.exe')['steps'], 'key:Return wait:3')
        with self.assertRaises(SystemExit): winplay.meta(f'{HERE}/titles.tsv', 'nope', 'x.exe')


class Stats(unittest.TestCase):
    def test_flat_and_busy(self):
        w, h = 640, 480
        flat = bytes([10, 20, 30]) * (w * h)
        self.assertEqual(winplay.stats(w, h, flat)[0], 1)
        r = random.Random(2)
        busy = bytes(r.randrange(256) for _ in range(w * h * 3))
        self.assertGreater(winplay.stats(w, h, busy)[0], 300)
        # only the middle third counts
        top = bytearray(flat)
        top[:w * (h // 3) * 3] = busy[:w * (h // 3) * 3]
        self.assertEqual(winplay.stats(w, h, bytes(top))[0], 1)

    def test_greys_count_as_many(self):
        w, h = 640, 480
        px = bytearray()
        for i in range(w * h):
            g = (i * 7) % 256
            px += bytes([g, g, g])
        n, _ = winplay.stats(w, h, bytes(px))
        self.assertGreaterEqual(n, 301)

    def test_hash_sees_changes(self):
        w, h = 320, 240
        a = bytes(w * h * 3)
        b = bytearray(a); b[0] = 1
        self.assertNotEqual(winplay.stats(w, h, a)[1], winplay.stats(w, h, bytes(b))[1])
        self.assertEqual(winplay.stats(w, h, a)[1], winplay.stats(w, h, bytes(a))[1])

    def test_small(self):
        self.assertTrue(winplay.small(390, 453))
        self.assertFalse(winplay.small(640, 480))


def acps(xml):
    root = ET.fromstring(xml.split('?>', 1)[1] if xml.startswith('<?xml') else xml)
    return root, [e.text for e in root.iter(NS + 'activeCodePage')]


class Manifest(unittest.TestCase):
    def test_same_value_as_winref(self):
        # (the value the winref probes carry, which Windows' answers show working)
        _, found = acps(open(f'{HERE}/../winref/ja.manifest', encoding='utf-8').read())
        self.assertEqual(found, ['ja-JP'])

    CASES = {
        'none': None,
        'empty': '',
        'comctl': '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency>
<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security></trustInfo>
</assembly>''',
        'settings': '''<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings><dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware></windowsSettings></application>
</assembly>''',
        'application only': '''<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application></compatibility>
<asmv3:application xmlns:asmv3="urn:schemas-microsoft-com:asm.v3"><asmv3:windowsSettings xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings"><dpiAware>true</dpiAware></asmv3:windowsSettings></asmv3:application>
</assembly>''',
        'compatibility only': '''<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application></compatibility>
</assembly>''',
        'has one': f'''<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings><activeCodePage xmlns="{winplay.MANIFEST_NS}">UTF-8</activeCodePage></windowsSettings></application>
</assembly>''',
    }

    def test_one_codepage_and_settings_kept(self):
        for name, src in self.CASES.items():
            out = winplay.with_codepage(src)
            root, found = acps(out)
            self.assertEqual(found, ['ja-JP'], name)
            # (in an asm.v3 windowsSettings, never in compatibility's <application>)
            parents = {c: p for p in root.iter() for c in p}
            e = next(root.iter(NS + 'activeCodePage'))
            self.assertTrue(parents[e].tag.endswith('windowsSettings'), name)
            self.assertEqual(parents[parents[e]].tag, '{urn:schemas-microsoft-com:asm.v3}application', name)
            if src:
                # everything else of the original is still there
                old = ET.fromstring(src.split('?>', 1)[1] if src.startswith('<?xml') else src)
                tags = lambda r: sorted(e.tag for e in r.iter() if not e.tag.endswith('activeCodePage'))
                new_tags = tags(root)
                for t in tags(old): self.assertIn(t, new_tags, (name, t))

    def test_idempotent(self):
        for name, src in self.CASES.items():
            once = winplay.with_codepage(src)
            self.assertEqual(acps(winplay.with_codepage(once))[1], ['ja-JP'], name)


class End(unittest.TestCase):
    """end(): closed first when asked, a "quit?" box answered once, the rest killed"""
    def run_end(self, grace, exits_after=None, exits_on_confirm=False):
        t, log, state = [0.0], [], {'up': True}
        def sleep(s):
            t[0] += s
            if exits_after is not None and t[0] >= exits_after: state['up'] = False
        def confirm():
            log.append(('confirm', t[0]))
            if exits_on_confirm: state['up'] = False
        pids = lambda: {1, 2} if state['up'] else set()
        ended = winplay.end(pids, grace, lambda pid: log.append(('close', pid)), confirm,
                            lambda pid: log.append(('kill', pid)), lambda: state['up'], sleep, lambda: t[0])
        return ended, log, t[0]

    def test_no_grace_kills(self):
        ended, log, t = self.run_end(0)
        self.assertEqual((ended, sorted(log), t), (False, [('kill', 1), ('kill', 2)], 0.0))

    def test_closes_and_waits(self):
        ended, log, t = self.run_end(20, exits_after=1.5)
        self.assertEqual((ended, sorted(log), t), (True, [('close', 1), ('close', 2)], 1.5))

    def test_quit_box_answered_once(self):
        ended, log, t = self.run_end(20, exits_on_confirm=True)
        self.assertEqual((ended, [x for x in log if x[0] != 'close'], t), (True, [('confirm', 3.0)], 3.0))

    def test_never_closes_killed_at_grace(self):
        ended, log, t = self.run_end(10)
        self.assertEqual((ended, [x[0] for x in log].count('confirm'), t, sorted(x for x in log if x[0] == 'kill')),
                         (False, 1, 10.0, [('kill', 1), ('kill', 2)]))



class StoryShots(unittest.TestCase):
    def shots(self, story):
        return [s + 2 for s in range(0, int(story), 2) if winplay.story_shot(s, story)]

    def test_two_screens_at_the_default(self):
        self.assertEqual(self.shots(30), [20, 30])
        self.assertEqual(self.shots(60), [20, 40, 60])
        self.assertEqual(self.shots(40), [20, 40])
        self.assertEqual(self.shots(10), [10])
        for story in range(4, 200, 2):
            got = self.shots(story)
            self.assertEqual(got[-1], story, story)
            self.assertGreaterEqual(len(got), 2 if story > 20 else 1, story)

    def test_the_same_rule_as_corpus_mjs(self):
        # (differential: the page's rule, tests/corpus/story.mjs, under node)
        import json, shutil, subprocess
        node = shutil.which('node')
        if not node:
            self.skipTest('no node')
        cases = [(s, story) for story in range(2, 130, 2) for s in range(0, story, 2)] + [(s, 25) for s in range(0, 26, 2)]
        js = ('import { storyShot } from %s; const c = JSON.parse(process.argv[1]);'
              'console.log(JSON.stringify(c.map(([s, t]) => storyShot(s, t))));') % json.dumps('file://' + os.path.join(HERE, 'story.mjs'))
        out = subprocess.run([node, '--input-type=module', '-e', js, json.dumps(cases)], capture_output=True, text=True, check=True).stdout
        self.assertEqual(json.loads(out), [winplay.story_shot(s, t) for s, t in cases])


if __name__ == '__main__':
    unittest.main()
