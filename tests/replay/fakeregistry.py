"""A registry for games_test: what of the OCI distribution API games.py uses, as ghcr.io answers
it (a Bearer challenge, its token endpoint wanting Basic for a push, blob uploads checked
against their digest, a blob's GET redirected to a URL that refuses an Authorization header)."""
import base64
import hashlib
import http.server
import json
import re
import threading
import urllib.parse


class Fake:
    def __init__(self, secret="s3cret", public=True):
        self.secret, self.public = secret, public
        self.blobs, self.manifests, self.uploads = {}, {}, {}
        self.gets = 0
        self.puts = 0
        self.leaked = 0
        fake = self

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def reply(self, code, body=b"", headers=None):
                self.send_response(code)
                for k, v in (headers or {}).items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                if self.command != "HEAD":
                    self.wfile.write(body)

            def body(self):
                return self.rfile.read(int(self.headers.get("Content-Length") or 0))

            def authed(self, push):
                a = self.headers.get("Authorization", "")
                if a == "Bearer push" or (a == "Bearer pull" and not push) or (not push and fake.public and a == "Bearer anon"):
                    return True
                scope = "repository:t/traces:" + ("pull,push" if push else "pull")
                self.reply(401, b"", {"WWW-Authenticate": f'Bearer realm="{fake.url}/token",service="fake",scope="{scope}"'})
                return False

            def do_GET(self):
                u = urllib.parse.urlparse(self.path)
                if u.path == "/token":
                    q = urllib.parse.parse_qs(u.query)
                    a = self.headers.get("Authorization", "")
                    good = a == "Basic " + base64.b64encode(b"x:" + fake.secret.encode()).decode()
                    if "push" in q["scope"][0]:
                        return self.reply(200, json.dumps({"token": "push"}).encode()) if good else self.reply(401)
                    return self.reply(200, json.dumps({"token": "pull" if good else "anon"}).encode())
                if u.path.startswith("/cdn/"):
                    # (a signed URL: an Authorization header is refused, as a CDN's is)
                    if "Authorization" in self.headers:
                        fake.leaked += 1
                        return self.reply(400)
                    d = u.path[5:]
                    fake.gets += 1
                    return self.reply(200, fake.blobs[d]) if d in fake.blobs else self.reply(404)
                m = re.fullmatch(r"/v2/t/traces/blobs/(sha256:[0-9a-f]{64})", u.path)
                if m:
                    if not self.authed(False):
                        return
                    return self.reply(307, b"", {"Location": f"{fake.url}/cdn/{m[1]}"}) if m[1] in fake.blobs else self.reply(404)
                self.reply(404)

            def do_HEAD(self):
                m = re.fullmatch(r"/v2/t/traces/blobs/(sha256:[0-9a-f]{64})", self.path)
                if not m or not self.authed(True):
                    return self.reply(404) if not m else None
                self.reply(200 if m[1] in fake.blobs else 404)

            def do_POST(self):
                self.body()
                if self.path != "/v2/t/traces/blobs/uploads/" or not self.authed(True):
                    return None if self.path == "/v2/t/traces/blobs/uploads/" else self.reply(404)
                k = str(len(fake.uploads))
                fake.uploads[k] = True
                # (relative, as ghcr's is)
                self.reply(202, b"", {"Location": f"/v2/t/traces/blobs/uploads/{k}?state=x"})

            def do_PUT(self):
                u = urllib.parse.urlparse(self.path)
                data = self.body()
                if not self.authed(True):
                    return
                m = re.fullmatch(r"/v2/t/traces/blobs/uploads/(\d+)", u.path)
                if m:
                    d = urllib.parse.parse_qs(u.query)["digest"][0]
                    if m[1] not in fake.uploads or "sha256:" + hashlib.sha256(data).hexdigest() != d:
                        return self.reply(400)
                    del fake.uploads[m[1]]
                    fake.blobs[d] = data
                    fake.puts += 1
                    return self.reply(201, b"", {"Location": f"/v2/t/traces/blobs/{d}"})
                m = re.fullmatch(r"/v2/t/traces/manifests/([\w.-]+)", u.path)
                if m:
                    j = json.loads(data)
                    # (a manifest whose blobs are not there is refused)
                    if any(x["digest"] not in fake.blobs for x in [j["config"]] + j["layers"]):
                        return self.reply(400)
                    if self.headers.get("Content-Type") != j["mediaType"]:
                        return self.reply(400)
                    fake.manifests[m[1]] = data
                    return self.reply(201, b"", {"Docker-Content-Digest": "sha256:" + hashlib.sha256(data).hexdigest()})
                self.reply(404)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.ref = f"127.0.0.1:{self.server.server_port}/t/traces"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
