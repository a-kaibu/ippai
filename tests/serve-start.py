#!/usr/bin/env python3
"""ippai serve answers before it has compressed the web app (serve.rs precompress): zstd -19
over a web app with several sake builds took longer than tests/winetest/run-browser.mjs waits
for the server (winetest-gpu, run 42445713214). A stale .zst is never served: it is removed
before the server listens; the fresh one appears later and is then sent zstd-encoded.

    tests/serve-start.py [path to ippai]
"""
import http.client, os, random, socket, subprocess, sys, tempfile, time

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/ippai')


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def get(port, path, zstd):
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
    c.request('GET', path, headers={'Accept-Encoding': 'zstd'} if zstd else {})
    r = c.getresponse()
    return r.status, r.getheader('content-encoding'), r.read()


def fail(what):
    raise SystemExit(f'serve-start: {what}')


with tempfile.TemporaryDirectory() as home:
    web = os.path.join(home, 'web')
    os.makedirs(web)
    with open(os.path.join(web, 'play.html'), 'w') as f:
        f.write('<!doctype html><title>play</title>' + 'x' * 2000)
    # ~18 MiB of script that compresses (words drawn at random): seconds at level 19
    rnd = random.Random(7)
    words = [f'w{i}' for i in range(5000)]
    with open(os.path.join(web, 'big.js'), 'w') as f:
        for _ in range(16):
            f.write(' '.join(rnd.choice(words) for _ in range(200000)) + '\n')
    big = open(os.path.join(web, 'big.js'), 'rb').read()
    # a stale .zst from before (older than its source) must not be sent
    stale = os.path.join(web, 'play.html.zst')
    with open(stale, 'wb') as f:
        f.write(b'stale')
    os.utime(stale, (time.time() - 100, time.time() - 100))

    port = free_port()
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR='', XDG_CONFIG_HOME=f'{home}/.config')
    p = subprocess.Popen([BIN, 'serve', '--foreground', '--listen', f'127.0.0.1:{port}', '--no-prerun', '--web', web,
                          '--store', os.path.join(home, 'store')], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        t0 = time.time()
        for _ in range(300):
            try:
                st, enc, body = get(port, '/play.html', True)
                break
            except OSError:
                time.sleep(0.05)
        else:
            fail('no answer in 15 s')
        took = time.time() - t0
        if st != 200 or body == b'stale' or (enc == 'zstd' and body == b'stale'):
            fail(f'play.html: {st} {enc} {body[:20]!r}')
        print(f'ok   answers in {took:.1f} s, no stale .zst')
        st, enc, body = get(port, '/big.js', True)
        if os.path.exists(os.path.join(web, 'big.js.zst')):
            fail('big.js.zst was written before the server answered')
        if st != 200 or enc is not None or body != big:
            fail(f'big.js before its .zst: {st} {enc} {len(body)}')
        else:
            print('ok   big.js sent as it is before its .zst exists')
        for _ in range(3000):
            if os.path.exists(os.path.join(web, 'big.js.zst')):
                break
            time.sleep(0.1)
        else:
            fail('big.js.zst not written in 300 s')
        st, enc, body = get(port, '/big.js', True)
        if st != 200 or enc != 'zstd' or len(body) >= len(big):
            fail(f'big.js after: {st} {enc} {len(body)}')
        print(f'ok   then zstd-encoded ({len(body)} of {len(big)} bytes)')
    finally:
        p.kill()
