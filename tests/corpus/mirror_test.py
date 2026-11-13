#!/usr/bin/env python3
"""Tests of tests/corpus/mirror.py (the VN Mirror Archive's trials) and of tests/corpus-prep.sh
downloading through it: URL matching (unit cases and random URLs as properties), the catalog cut
to the mirror's trials, conditional fetches against a local HTTP server (gzip, ETag, 304, schema),
the lookup command, and corpus-prep.sh end to end on a local server standing in for both the
mirror and the title's own site (mirror copy used, sha256 mismatch -> own site, MIRROR_ONLY,
no MIRROR). MIRROR_LIVE=1 also reads the real catalog.

  python3 tests/corpus/mirror_test.py
"""
import csv, gzip, hashlib, http.server, io, json, os, random, shutil, string, subprocess, sys
import tempfile, threading, unittest, zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import mirror

SHA = 'ab' * 32


def entry(**kw):
    f = {'id': 'f1', 'kind': 'trial', 'filename': 'x_trial.zip', 'size': 10, 'sha256': SHA,
         'download_url': mirror.ORIGIN + 'vn-archive/mirrors/resolve/files/' + SHA,
         'sources': [{'mirrors': ['http://suezou.dyndns.org/dl2012/k/x/x_trial.zip'], 'source_url': None,
                      'fields': {}}]}
    f.update(kw)
    return f


class Norm(unittest.TestCase):
    def test_cases(self):
        n = mirror.norm
        self.assertEqual(n('http://suezou.dyndns.org/dl2006/pulltop/haruka/Haruka_Trial.exe'),
                         'suezou.dyndns.org/dl2006/pulltop/haruka/haruka_trial.exe')
        self.assertEqual(n('https://Trial.DLsite.com/a/B.zip'), n('http://trial.dlsite.com/a/b.zip'))
        self.assertEqual(n('https://web.archive.org/web/20130628203950id_/http://kokoron.madoka.org/m/s.zip'),
                         'kokoron.madoka.org/m/s.zip')
        self.assertEqual(n('http://web.archive.org/web/2013/https://web.archive.org/web/2014id_/http://a.jp/x'), 'a.jp/x')
        self.assertEqual(n('http://a.jp:80/x'), 'a.jp/x')
        self.assertEqual(n('https://a.jp:443/x'), 'a.jp/x')
        # (another port is another server)
        self.assertNotEqual(n('http://mirror1.lapistan.jp:8000/x'), n('http://mirror1.lapistan.jp/x'))
        self.assertNotEqual(n('http://a.jp/x.zip'), n('http://a.jp/x.zip2'))
        self.assertEqual(n('  http://a.jp/x \n'), 'a.jp/x')

    def test_properties(self):
        rnd = random.Random(1113)
        chars = string.ascii_letters + string.digits + '-_.~%'
        for _ in range(3000):
            host = ''.join(rnd.choice(string.ascii_letters + string.digits) for _ in range(rnd.randint(1, 12))) + '.jp'
            path = '/'.join(''.join(rnd.choice(chars) for _ in range(rnd.randint(1, 10))) for _ in range(rnd.randint(1, 4)))
            port = rnd.choice(['', ':80', ':443', ':8000'])
            u = f'{rnd.choice(["http", "https", "HTTP"])}://{host}{port}/{path}'
            k = mirror.norm(u)
            self.assertEqual(mirror.norm(k), k, u)  # idempotent
            self.assertEqual(k, k.lower(), u)
            self.assertNotIn('://', k, u)
            # scheme, case and a Wayback prefix don't change the key
            self.assertEqual(mirror.norm(u.swapcase()), k, u)
            self.assertEqual(mirror.norm(f'https://web.archive.org/web/{rnd.randint(1, 10 ** 14)}id_/{u}'), k, u)
            # the path does
            self.assertNotEqual(mirror.norm(u + 'x'), k, u)


class Trials(unittest.TestCase):
    def test_filter(self):
        cat = {'files': [
            entry(id='ok'),
            entry(id='movie', kind='movie'),
            entry(id='notyet', download_url=None),
            entry(id='nosha', sha256=None),
            entry(id='badsha', sha256='xyz'),
            entry(id='elsewhere', download_url='https://evil.example/' + SHA),
            entry(id='nourls', sources=[{'mirrors': [], 'fields': {}}]),
            entry(id='upper', sha256=SHA.upper(), sources=[{'mirrors': [], 'source_url': 'https://A.jp/T.zip',
                                                           'fields': {'published_link': 'http://b.jp/t.zip'}}]),
        ]}
        t = mirror.trials(cat)
        self.assertEqual([x['id'] for x in t], ['ok', 'upper'])
        self.assertEqual(t[0]['urls'], ['suezou.dyndns.org/dl2012/k/x/x_trial.zip'])
        self.assertEqual(t[1]['sha256'], SHA)
        self.assertEqual(t[1]['urls'], ['a.jp/t.zip', 'b.jp/t.zip'])

    def test_lookup(self):
        data = {'trials': mirror.trials({'files': [entry()]})}
        self.assertEqual(mirror.lookup(data, 'https://SUEZOU.dyndns.org/dl2012/k/x/X_trial.zip')['id'], 'f1')
        self.assertIsNone(mirror.lookup(data, 'http://suezou.dyndns.org/dl2012/k/x/y_trial.zip'))

    def test_every_corpus_url_has_one_key(self):
        # (two titles of titles.tsv on one key would play the same mirror file)
        rows = list(csv.DictReader(open(f'{HERE}/titles.tsv', encoding='utf-8'), delimiter='\t'))
        keys = {}
        for r in rows:
            self.assertTrue(r['url'].startswith(('http://', 'https://')), r['id'])
            keys.setdefault(mirror.norm(r['url']), []).append(r['id'])
        self.assertEqual({k: v for k, v in keys.items() if len(v) > 1}, {})


class Server:
    """A local HTTP server: routes[path] = (status, headers, body); log of (path, headers)."""
    def __init__(self):
        self.routes, self.log = {}, []
        outer = self

        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                outer.log.append((self.path, dict(self.headers)))
                st, hd, body = outer.routes.get(self.path, (404, {}, b''))
                if callable(body): st, hd, body = body(self)
                self.send_response(st)
                for k, v in hd.items(): self.send_header(k, v)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a): pass
        self.httpd = http.server.ThreadingHTTPServer(('127.0.0.1', 0), H)
        self.base = f'http://127.0.0.1:{self.httpd.server_address[1]}'
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


class Fetch(unittest.TestCase):
    def setUp(self):
        self.s = Server()
        self.tmp = tempfile.mkdtemp()
        self.out = os.path.join(self.tmp, 'trials.json')

    def tearDown(self):
        self.s.close()
        shutil.rmtree(self.tmp)

    def catalog(self, etag, files):
        body = gzip.compress(json.dumps({'schema_version': 1, 'generated_at': etag, 'files': files}).encode())

        def answer(h):
            if h.headers.get('If-None-Match') == etag: return 304, {'ETag': etag}, b''
            return 200, {'ETag': etag, 'Content-Encoding': 'gzip'}, body
        self.s.routes['/manifest.json'] = (200, {}, answer)

    def test_conditional(self):
        url = self.s.base + '/manifest.json'
        self.catalog('W/"1"', [entry(id='a')])
        self.assertTrue(mirror.fetch(self.out, url))
        d = json.load(open(self.out))
        self.assertEqual((d['etag'], d['source'], [t['id'] for t in d['trials']]), ('W/"1"', url, ['a']))
        self.assertEqual(self.s.log[-1][1].get('Accept-Encoding'), 'gzip')
        self.assertNotIn('If-None-Match', self.s.log[-1][1])
        # unchanged: 304, the file stays
        before = open(self.out).read()
        self.assertFalse(mirror.fetch(self.out, url))
        self.assertEqual(self.s.log[-1][1].get('If-None-Match'), 'W/"1"')
        self.assertEqual(open(self.out).read(), before)
        # changed
        self.catalog('W/"2"', [entry(id='a'), entry(id='b')])
        self.assertTrue(mirror.fetch(self.out, url))
        self.assertEqual([t['id'] for t in json.load(open(self.out))['trials']], ['a', 'b'])
        # (another catalog URL: no etag of the old one sent)
        self.s.routes['/other.json'] = self.s.routes['/manifest.json']
        self.assertTrue(mirror.fetch(self.out, self.s.base + '/other.json'))
        self.assertNotIn('If-None-Match', self.s.log[-1][1])

    def test_plain_body_and_errors(self):
        url = self.s.base + '/manifest.json'
        self.s.routes['/manifest.json'] = (200, {}, json.dumps({'schema_version': 1, 'files': [entry()]}).encode())
        self.assertTrue(mirror.fetch(self.out, url))
        self.assertEqual(len(json.load(open(self.out))['trials']), 1)
        good = open(self.out).read()
        self.s.routes['/manifest.json'] = (200, {}, json.dumps({'schema_version': 2, 'files': []}).encode())
        with self.assertRaises(SystemExit): mirror.fetch(self.out, url)
        self.s.routes['/manifest.json'] = (500, {}, b'')
        with self.assertRaises(Exception): mirror.fetch(self.out, url)
        # (a failed fetch leaves the last good file)
        self.assertEqual(open(self.out).read(), good)

    def test_cli(self):
        json.dump({'trials': mirror.trials({'files': [entry(size=None)]})}, open(self.out, 'w'))
        r = subprocess.run([sys.executable, '-I', f'{HERE}/mirror.py', 'lookup', self.out,
                            'https://suezou.dyndns.org/dl2012/k/x/x_trial.zip'], capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (0, f"{entry()['download_url']}\t{SHA}\t\n"))
        r = subprocess.run([sys.executable, '-I', f'{HERE}/mirror.py', 'lookup', self.out, 'http://a.jp/none.zip'],
                           capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout), (1, ''))
        r = subprocess.run([sys.executable, '-I', f'{HERE}/mirror.py', 'report', self.out, f'{HERE}/titles.tsv'],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout, r'[0-9]+/[0-9]+ corpus titles on the mirror')


class Prep(unittest.TestCase):
    """tests/corpus-prep.sh with MIRROR: a local server is the mirror (/m/...) and the site (/o/...)."""
    def setUp(self):
        self.s = Server()
        self.tmp = tempfile.mkdtemp()
        b = io.BytesIO()
        with zipfile.ZipFile(b, 'w') as z: z.writestr('trial/game.exe', b'MZ' + b'\0' * 64)
        self.zip = b.getvalue()
        self.sha = hashlib.sha256(self.zip).hexdigest()
        self.s.routes['/m/' + self.sha] = (200, {}, self.zip)
        self.s.routes['/o/x_trial.zip'] = (200, {}, self.zip)
        self.url = self.s.base + '/o/x_trial.zip'

    def tearDown(self):
        self.s.close()
        shutil.rmtree(self.tmp)

    def prep(self, sha=None, mirror_only=False, use_mirror=True, tid='t1'):
        m = os.path.join(self.tmp, 'trials.json')
        os.environ['MIRROR_ORIGIN'] = self.s.base + '/m/'
        try:
            f = entry(sha256=sha or self.sha, download_url=self.s.base + '/m/' + self.sha,
                      sources=[{'mirrors': [self.url], 'fields': {}}])
            json.dump({'trials': mirror.trials({'files': [f]})}, open(m, 'w'))
        finally:
            del os.environ['MIRROR_ORIGIN']
        env = dict(os.environ, TRIALS=os.path.join(self.tmp, 'trials'), DL_TRIES='1')
        env.pop('MIRROR', None); env.pop('MIRROR_ONLY', None)
        if use_mirror: env['MIRROR'] = m
        if mirror_only: env['MIRROR_ONLY'] = '1'
        self.s.log.clear()
        r = subprocess.run(['bash', os.path.join(os.path.dirname(HERE), 'corpus-prep.sh'), tid, self.url, 'trial/game.exe', ''],
                           capture_output=True, text=True, env=env, timeout=120)
        src = os.path.join(self.tmp, 'trials', tid, 'source')
        return r, open(src).read().strip() if os.path.exists(src) else None, [p for p, _ in self.s.log]

    def test_mirror(self):
        r, src, paths = self.prep()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip().splitlines()[-1], 'trial/game.exe')
        self.assertEqual((src, paths), ('mirror', ['/m/' + self.sha]))

    def test_sha_mismatch_falls_back(self):
        r, src, paths = self.prep(sha='0' * 64)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('sha256 differs', r.stderr)
        self.assertEqual((src, paths), ('origin', ['/m/' + self.sha, '/o/x_trial.zip']))

    def test_mirror_down_falls_back(self):
        del self.s.routes['/m/' + self.sha]
        r, src, paths = self.prep()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((src, paths[-1]), ('origin', '/o/x_trial.zip'))

    def test_mirror_only(self):
        r, src, paths = self.prep(sha='0' * 64, mirror_only=True)
        self.assertEqual(r.returncode, 4)
        self.assertNotIn('/o/x_trial.zip', paths)
        self.assertIsNone(src)

    def test_no_mirror(self):
        r, src, paths = self.prep(use_mirror=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual((src, paths), ('origin', ['/o/x_trial.zip']))


@unittest.skipUnless(os.environ.get('MIRROR_LIVE') == '1', 'MIRROR_LIVE=1 reads the real catalog')
class Live(unittest.TestCase):
    def test_catalog(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, 't.json')
            mirror.fetch(out)
            d = json.load(open(out))
            self.assertGreater(len(d['trials']), 0)
            for t in d['trials']:
                self.assertTrue(t['download_url'].startswith(mirror.ORIGIN))
                self.assertRegex(t['sha256'], '^[0-9a-f]{64}$')
            rows = list(csv.DictReader(open(f'{HERE}/titles.tsv', encoding='utf-8'), delimiter='\t'))
            idx = mirror.index(d)
            self.assertGreater(sum(1 for r in rows if mirror.lookup(d, r['url'], idx)), 0)


if __name__ == '__main__':
    unittest.main()
