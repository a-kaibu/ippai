#!/usr/bin/env python3
"""Trial games' traces (.github/workflows/gametrace.yml records them): each trace, zstd-compressed,
is an OCI artifact on GitHub Packages (ghcr.io/a-kaibu/ippai-traces:<id>-<bits>, free and
unlimited for a public package: the user, 2026-11-07, "packagesに置くようにして"), and
testdata/apitrace/<id>-<bits>.txt says what was recorded (api, exe, seconds, the trace's size and
sha256) and where it is (ref, the zstd blob's digest and size). Pulled blobs are kept in
TRACES_CACHE (default ~/.cache/masu-traces). A private package (before it is made public)
needs GHCR_TOKEN (CI: the job's GITHUB_TOKEN with packages: read); pushing needs packages: write.

    games.py todo [--again=true] [--max N] [id ...]   the titles to record (at most N): those given, else
                                             tests/replay/games.txt's without a trace yet and not
                                             skipped (--again: all of them)
    games.py skip <id> <reason> [detail]      a title with no trace to keep (none: it names no
                                             D3D8/9/11; ddraw: it drew with DirectDraw; notrace:
                                             it made none; noconvert: its trace
                                             does not convert) in skipped.tsv, not tried again
    games.py took <id>                        a title whose trace was committed: off skipped.tsv
    games.py push <x.trace> <dir/name> --api A --exe E --seconds S [--ref REGISTRY/REPO]
                                             the trace to the registry, <dir/name>.txt written
    games.py unpack <dir/name> <x.trace>      the trace pulled (or from the cache), checked against
                                             its digests
    games.py migrate [dir]                    traces still in parts in the repository (the old
                                             way: <name>.trace.zst.000, ...) pushed, their parts
                                             removed
    games.py names [dir]                      the traces there (<id>-<bits>), for build.sh
"""
import argparse
import base64
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
GAMES = os.path.join(HERE, "..", "..", "testdata", "apitrace")
TITLES = os.path.join(HERE, "..", "corpus", "titles.tsv")
REF = os.environ.get("TRACES_REF", "ghcr.io/a-kaibu/ippai-traces")
SOURCE = "https://github.com/a-kaibu/masu"


def listed(path=os.path.join(HERE, "games.txt")):
    return [l.split("#")[0].split()[0] for l in open(path, encoding="utf-8") if l.split("#")[0].strip()]


def titles(path=TITLES):
    return {l.split("\t")[0] for l in open(path, encoding="utf-8").read().splitlines()[1:] if l}


REASONS = ("none", "ddraw", "notrace", "noconvert")


def skipped(d=GAMES):
    """skipped.tsv: id -> (reason, detail)"""
    f = os.path.join(d, "skipped.tsv")
    if not os.path.exists(f):
        return {}
    out = {}
    for l in open(f, encoding="utf-8").read().splitlines():
        if not l or l.startswith("#"):
            continue
        i, r, *x = l.split("\t")
        if r not in REASONS:
            raise SystemExit(f"{f}: {i}: reason {r!r}")
        out[i] = (r, x[0] if x else "")
    return out


def write_skipped(m, d=GAMES):
    f = os.path.join(d, "skipped.tsv")
    os.makedirs(d, exist_ok=True)
    with open(f, "w", encoding="utf-8") as o:
        o.write("# trial games with no trace, not recorded again unless asked (games.py; gametrace.yml's again)\n")
        for i in sorted(m):
            r, x = m[i]
            o.write(f"{i}\t{r}\t{x}\n" if x else f"{i}\t{r}\n")


def skip(i, reason, detail="", d=GAMES):
    if reason not in REASONS:
        raise SystemExit(f"reason {reason!r}: one of {' '.join(REASONS)}")
    m = skipped(d)
    m[i] = (reason, " ".join(detail.split()))
    write_skipped(m, d)


def took(i, d=GAMES):
    m = skipped(d)
    if m.pop(i, None) is not None:
        write_skipped(m, d)


def names(d=GAMES):
    return sorted(os.path.basename(m)[:-4] for m in glob.glob(os.path.join(d, "*.txt")))


def todo(ids, again, d=GAMES, known=None, most=None):
    known = titles() if known is None else known
    given = bool(ids)
    ids = ids or listed()
    bad = [i for i in ids if i not in known]
    if bad:
        raise SystemExit(f"not in tests/corpus/titles.tsv: {' '.join(bad)}")
    have = {re.sub(r"-(32|64)$", "", n) for n in names(d)} | set(skipped(d))
    out = [i for i in ids if given or again or i not in have]
    # (a run takes at most `most`: GitHub's matrix holds 256 jobs; the next run takes the rest)
    return out[:most] if most else out


def meta(prefix):
    return dict(l.split(" ", 1) for l in open(prefix + ".txt", encoding="utf-8").read().splitlines() if " " in l)


def sha(b):
    return "sha256:" + hashlib.sha256(b).hexdigest()


# --- the registry (the OCI distribution API: what docker and oras speak) ---

class Registry:
    def __init__(self, ref, token=None):
        host, _, self.repo = ref.partition("/")
        local = host.split(":")[0] in ("127.0.0.1", "localhost")
        self.base = ("http://" if local else "https://") + host
        self.secret = os.environ.get("GHCR_TOKEN") if token is None else token
        self.bearer = {}

    def _auth(self, challenge, scope):
        # (Bearer realm="...",service="...",scope="...": a token for the scope, with the secret
        # as Basic when there is one; anonymous otherwise, which a public package allows)
        f = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
        q = {"service": f.get("service", ""), "scope": f.get("scope", scope)}
        r = urllib.request.Request(f["realm"] + "?" + urllib.parse.urlencode(q))
        if self.secret:
            r.add_unredirected_header("Authorization", "Basic " + base64.b64encode(b"x:" + self.secret.encode()).decode())
        try:
            with urllib.request.urlopen(r, timeout=60) as o:
                j = json.load(o)
        except urllib.error.HTTPError as e:
            raise SystemExit(f"{f['realm']}: {scope}: HTTP {e.code}" + ("" if self.secret else " (no GHCR_TOKEN)"))
        return j.get("token") or j.get("access_token")

    def call(self, method, path, body=None, headers=None, push=False, ok=(200,)):
        scope = f"repository:{self.repo}:" + ("pull,push" if push else "pull")
        url = path if path.startswith("http") else self.base + path
        for _ in range(2):
            r = urllib.request.Request(url, data=body, method=method, headers=headers or {})
            if scope in self.bearer:
                # (unredirected: a blob's GET is redirected to a signed URL that wants none)
                r.add_unredirected_header("Authorization", "Bearer " + self.bearer[scope])
            try:
                with urllib.request.urlopen(r, timeout=600) as o:
                    return o.status, dict(o.headers), o.read()
            except urllib.error.HTTPError as e:
                if e.code == 401 and scope not in self.bearer and "Bearer" in e.headers.get("WWW-Authenticate", ""):
                    self.bearer[scope] = self._auth(e.headers["WWW-Authenticate"], scope)
                    continue
                if e.code in ok:
                    return e.code, dict(e.headers), e.read()
                raise SystemExit(f"{self.base}/v2/{self.repo}: {method} {path}: HTTP {e.code} {e.read()[:300]!r}")
        raise SystemExit(f"{self.base}/v2/{self.repo}: {method} {path}: refused")

    def has(self, digest):
        st, _, _ = self.call("HEAD", f"/v2/{self.repo}/blobs/{digest}", push=True, ok=(200, 404))
        return st == 200

    def put_blob(self, data):
        d = sha(data)
        if self.has(d):
            return d
        _, h, _ = self.call("POST", f"/v2/{self.repo}/blobs/uploads/", b"", {"Content-Length": "0"}, push=True, ok=(202,))
        loc = urllib.parse.urljoin(self.base + "/", h.get("Location") or h["location"])
        loc += ("&" if "?" in loc else "?") + "digest=" + urllib.parse.quote(d)
        self.call("PUT", loc, data, {"Content-Type": "application/octet-stream"}, push=True, ok=(201,))
        return d

    def put_manifest(self, tag, m):
        b = json.dumps(m, separators=(",", ":")).encode()
        self.call("PUT", f"/v2/{self.repo}/manifests/{tag}", b, {"Content-Type": m["mediaType"]}, push=True, ok=(201,))
        return sha(b)

    def blob(self, digest):
        _, _, b = self.call("GET", f"/v2/{self.repo}/blobs/{digest}")
        return b


EMPTY = b"{}"


def push(trace, prefix, api, exe, seconds, ref=REF, reg=None):
    data = open(trace, "rb").read()
    z = subprocess.run(["zstd", "-19", "-T0", "-q", "-c"], input=data, stdout=subprocess.PIPE, check=True).stdout
    return push_zst(z, data, prefix, api, exe, seconds, ref, reg)


def push_zst(z, data, prefix, api, exe, seconds, ref=REF, reg=None):
    reg = reg or Registry(ref)
    name = os.path.basename(prefix)
    layer = {"mediaType": "application/zstd", "digest": reg.put_blob(z), "size": len(z),
             "annotations": {"org.opencontainers.image.title": name + ".trace.zst"}}
    m = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
         "artifactType": "application/vnd.masu.apitrace.v1",
         "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": reg.put_blob(EMPTY), "size": len(EMPTY)},
         "layers": [layer],
         # (links the package to the repository: its visibility and access)
         "annotations": {"org.opencontainers.image.source": SOURCE}}
    reg.put_manifest(name, m)
    with open(prefix + ".txt", "w", encoding="utf-8") as f:
        f.write(f"api {api}\nexe {exe}\nseconds {seconds}\nbytes {len(data)}\nsha256 {hashlib.sha256(data).hexdigest()}\n"
                f"ref {ref}:{name}\nzst {layer['digest']}\nzbytes {len(z)}\n")
    return layer["digest"]


def unzstd(z, what):
    r = subprocess.run(["zstd", "-d", "-q", "-c"], input=z, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.returncode:
        raise SystemExit(f"{what}: zstd: {r.stderr.decode(errors='replace').strip()}")
    return r.stdout


def cache_dir():
    return os.environ.get("TRACES_CACHE") or os.path.join(os.path.expanduser("~"), ".cache", "masu-traces")


def unpack(prefix, out, reg=None):
    m = meta(prefix)
    if "zst" not in m:
        # (still in parts in the repository, until gametrace.yml's migrate has pushed it)
        open(out, "wb").write(from_parts(prefix, m))
        return
    d = m["zst"]
    c = os.path.join(cache_dir(), d.replace(":", "-") + ".zst")
    z = open(c, "rb").read() if os.path.exists(c) else None
    if z is None or sha(z) != d:
        z = (reg or Registry(m["ref"].rsplit(":", 1)[0])).blob(d)
        if sha(z) != d or len(z) != int(m["zbytes"]):
            raise SystemExit(f"{prefix}: the registry's blob is not {d}")
        os.makedirs(os.path.dirname(c), exist_ok=True)
        open(c + ".tmp", "wb").write(z)
        os.replace(c + ".tmp", c)
    data = unzstd(z, prefix)
    if hashlib.sha256(data).hexdigest() != m["sha256"] or len(data) != int(m["bytes"]):
        raise SystemExit(f"{prefix}: the blob does not make the recorded trace")
    open(out, "wb").write(data)


def old_parts(prefix):
    p = sorted(glob.glob(glob.escape(prefix) + ".trace.zst.[0-9][0-9][0-9]"))
    if [x[-3:] for x in p] != [f"{k:03d}" for k in range(len(p))]:
        raise SystemExit(f"{prefix}: parts {' '.join(x[-3:] for x in p)}")
    return p


def from_parts(prefix, m):
    p = old_parts(prefix)
    if len(p) != int(m["parts"]):
        raise SystemExit(f"{prefix}: {len(p)} parts, {m['parts']} recorded")
    data = unzstd(b"".join(open(x, "rb").read() for x in p), prefix)
    if hashlib.sha256(data).hexdigest() != m["sha256"] or len(data) != int(m["bytes"]):
        raise SystemExit(f"{prefix}: the parts do not make the recorded trace")
    return data


def migrate(d=GAMES, ref=REF, reg=None):
    """the traces still in parts (the old way): pushed, the parts gone; the names done"""
    done = []
    for n in names(d):
        pre = os.path.join(d, n)
        m = meta(pre)
        if "zst" in m:
            continue
        p = old_parts(pre)
        z = b"".join(open(x, "rb").read() for x in p)
        push_zst(z, from_parts(pre, m), pre, m["api"], m["exe"], m["seconds"], ref, reg)
        for x in p:
            os.remove(x)
        done.append(n)
    return done


def main():
    a = argparse.ArgumentParser()
    s = a.add_subparsers(dest="cmd", required=True)
    t = s.add_parser("todo")
    t.add_argument("--again", default="false")
    t.add_argument("--max", type=int, default=None)
    t.add_argument("ids", nargs="*")
    p = s.add_parser("push")
    p.add_argument("trace")
    p.add_argument("prefix")
    p.add_argument("--api", required=True)
    p.add_argument("--exe", required=True)
    p.add_argument("--seconds", required=True)
    p.add_argument("--ref", default=REF)
    g = s.add_parser("migrate")
    g.add_argument("dir", nargs="?", default=GAMES)
    u = s.add_parser("unpack")
    u.add_argument("prefix")
    u.add_argument("out")
    k = s.add_parser("skip")
    k.add_argument("id")
    k.add_argument("reason")
    k.add_argument("detail", nargs="?", default="")
    k.add_argument("--dir", default=GAMES)
    t2 = s.add_parser("took")
    t2.add_argument("id")
    t2.add_argument("--dir", default=GAMES)
    n = s.add_parser("names")
    n.add_argument("dir", nargs="?", default=GAMES)
    o = a.parse_args()
    if o.cmd == "todo":
        print(" ".join(todo(o.ids, o.again == "true", most=o.max)))
    elif o.cmd == "push":
        print(f"{o.prefix}: {o.ref}:{os.path.basename(o.prefix)} {push(o.trace, o.prefix, o.api, o.exe, o.seconds, o.ref)}")
    elif o.cmd == "migrate":
        print(" ".join(migrate(o.dir)) or "nothing in parts")
    elif o.cmd == "unpack":
        unpack(o.prefix, o.out)
    elif o.cmd == "skip":
        skip(o.id, o.reason, o.detail, o.dir)
    elif o.cmd == "took":
        took(o.id, o.dir)
    else:
        print("\n".join(names(o.dir)))


if __name__ == "__main__":
    main()
