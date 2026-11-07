#!/usr/bin/env python3
"""ippai serve's sign-in (auth.rs). By default this machine's browser and `tailscale serve` need
nothing, a proxy that says it forwards needs the token. With --strict-auth, a reverse proxy on
the same machine that connects from loopback, sets `Host: 127.0.0.1:<port>` and adds no
forwarding header (nginx's proxy_pass defaults) must not let anyone in without the token; then
the ways in: a bearer token, the sign-in page's POST (a session cookie, not the token).

    tests/auth-proxy.py [path to ippai]
"""
import http.client, http.server, os, socket, subprocess, sys, tempfile, threading, time, urllib.parse

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/ippai')


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def proxy(upstream, add_forwarded, tailscale=False):
    """A reverse proxy to 127.0.0.1:upstream on a port of its own: Host rewritten to the
    upstream's, the client's headers otherwise passed on (and X-Forwarded-For if asked)."""
    class H(http.server.BaseHTTPRequestHandler):
        def go(self):
            body = self.rfile.read(int(self.headers.get('content-length') or 0))
            h = {k: v for k, v in self.headers.items() if k.lower() not in ('host', 'connection')}
            h['Host'] = f'127.0.0.1:{upstream}'
            if add_forwarded:
                h['X-Forwarded-For'] = self.client_address[0]
            if tailscale:
                h['Tailscale-User-Login'] = 'kota@example.com'
            c = http.client.HTTPConnection('127.0.0.1', upstream, timeout=10)
            c.request(self.command, self.path, body=body or None, headers=h)
            r = c.getresponse()
            data = r.read()
            self.send_response(r.status)
            for k, v in r.getheaders():
                if k.lower() not in ('transfer-encoding', 'connection', 'content-length'):
                    self.send_header(k, v)
            self.send_header('content-length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        do_GET = do_POST = do_PUT = go

        def log_message(self, *a):
            pass
    s = http.server.ThreadingHTTPServer(('127.0.0.1', 0), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s.server_address[1]


def req(port, method='GET', path='/games', headers=None, body=None):
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
    c.request(method, path, body=body, headers=headers or {})
    r = c.getresponse()
    r.read()
    return r.status, dict((k.lower(), v) for k, v in r.getheaders())


def serve(home, port, *extra):
    env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR='', XDG_CONFIG_HOME=f'{home}/.config')
    p = subprocess.Popen([BIN, 'serve', '--foreground', '--listen', f'127.0.0.1:{port}', '--no-prerun', *extra],
                         env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(600):
        try:
            req(port, path='/login')
            break
        except OSError:
            time.sleep(0.1)
    else:
        raise SystemExit('ippai serve did not start')
    token = subprocess.check_output([BIN, 'token'], env=env, stderr=subprocess.DEVNULL, text=True).strip()
    return p, token


def check(what, got, want):
    if got != want:
        raise SystemExit(f'auth-proxy: {what}: HTTP {got}, want {want}')
    print(f'ok   {what}: {got}')


home = tempfile.mkdtemp(prefix='ippai-auth-')
port = free_port()
srv, token = serve(home, port)
try:
    check('this machine', req(port)[0], 200)
    check('tailscale serve', req(proxy(port, True, tailscale=True))[0], 200)
    check('a proxy that says it forwards', req(proxy(port, True))[0], 401)
finally:
    srv.kill()
    srv.wait()

port = free_port()
srv, token = serve(home, port, '--strict-auth')
try:
    plain = proxy(port, False)
    check('strict: through a proxy with no forwarding headers, no token', req(plain)[0], 401)
    check('strict: this machine, no token', req(port, headers={'Host': f'127.0.0.1:{port}'})[0], 401)
    check('strict: tailscale serve, no token', req(proxy(port, True, tailscale=True))[0], 401)
    check('through the proxy, a wrong bearer token', req(plain, headers={'Authorization': 'Bearer ' + '0' * 32})[0], 401)
    check('through the proxy, the bearer token', req(plain, headers={'Authorization': f'Bearer {token}'})[0], 200)
    check('the token in the query signs nothing in', 'set-cookie' in req(plain, path=f'/login?token={token}')[1], False)
    form = urllib.parse.urlencode({'token': token, 'next': '/games'})
    status, h = req(plain, 'POST', '/login', {'Content-Type': 'application/x-www-form-urlencoded'}, form)
    check('sign-in through the proxy', status, 303)
    cookie = h['set-cookie'].split(';')[0]
    check('the session cookie is not the token', token in cookie, False)
    check('signed in through the proxy', req(plain, headers={'Cookie': cookie})[0], 200)
    status, h = req(plain, 'POST', '/login', {'Content-Type': 'application/x-www-form-urlencoded'}, 'token=nope')
    check('a wrong token', (status, 'set-cookie' in h), (401, False))
finally:
    srv.kill()
    srv.wait()

print('auth-proxy: ok')
