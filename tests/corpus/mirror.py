#!/usr/bin/env python3
"""The trial games of the VN Mirror Archive (https://a-kaibu.github.io/mirror/) for the corpus:
its catalog (manifest.json, ~240 MB, ~30 MB gzipped) cut down to the trials it holds
(kind "trial" with a download_url and sha256), and the corpus' download URLs looked up in it.
tests/corpus-prep.sh downloads a title from the mirror when the mirror has it (and checks the
sha256), else from the title's own `url` in titles.tsv.

  mirror.py fetch OUT.json [URL]   OUT.json: the trials of the catalog at URL (default MANIFEST).
                                   A conditional request on OUT.json's etag: unchanged = no download.
  mirror.py lookup TRIALS.json URL prints "download_url<TAB>sha256<TAB>size" of the trial whose
                                   sources name URL; exit 1 when the mirror doesn't have it
  mirror.py report TRIALS.json TITLES.tsv   how many corpus titles the mirror has (stdout)

A trial matches a title's URL when one of its sources lists that URL (sources[].mirrors,
source_url, fields.published_link), compared by norm(): scheme, case and a Wayback Machine
"web.archive.org/web/<time>id_/" prefix don't count. Only download URLs under ORIGIN (the
mirror's Hugging Face bucket, as the mirror's own sw.js checks) are used; MIRROR_ORIGIN
overrides it (tests).
"""
import csv, gzip, json, os, re, sys, urllib.request

MANIFEST = 'https://a-kaibu.github.io/mirror/manifest.json'
ORIGIN = 'https://huggingface.co/buckets/'
WAYBACK = re.compile(r'^web\.archive\.org/web/[0-9]+(?:id_)?/', re.I)
SHA256 = re.compile(r'^[0-9a-f]{64}$')


def origin():
    return os.environ.get('MIRROR_ORIGIN', ORIGIN)


def norm(url):
    """The key a URL is matched by: no scheme, no Wayback prefix, no default port, lower case."""
    u = url.strip()
    u = re.sub(r'^[a-z][a-z0-9+.-]*://', '', u, flags=re.I)
    while True:
        m = WAYBACK.match(u)
        if not m: break
        u = re.sub(r'^[a-z][a-z0-9+.-]*://', '', u[m.end():], flags=re.I)
    host, sep, path = u.partition('/')
    host = re.sub(r':(80|443)$', '', host)
    return (host + sep + path).lower()


def source_urls(f):
    out = []
    for s in f.get('sources') or []:
        out += s.get('mirrors') or []
        if s.get('source_url'): out.append(s['source_url'])
        pl = (s.get('fields') or {}).get('published_link')
        if isinstance(pl, str): out.append(pl)
    return out


def trials(catalog):
    """The catalog's trials that the mirror holds: [{id, filename, size, sha256, download_url, urls}]."""
    out = []
    for f in catalog.get('files') or []:
        url, sha = f.get('download_url'), (f.get('sha256') or '').lower()
        if f.get('kind') != 'trial' or not isinstance(url, str) or not url.startswith(origin()): continue
        if not SHA256.match(sha): continue
        urls = sorted({norm(u) for u in source_urls(f) if isinstance(u, str) and u.strip()})
        if not urls: continue
        out.append({'id': f.get('id'), 'filename': f.get('filename'), 'size': f.get('size'),
                    'sha256': sha, 'download_url': url, 'urls': urls})
    out.sort(key=lambda t: t['id'] or '')
    return out


def fetch(out, url=MANIFEST):
    """Writes OUT (the trials of the catalog at URL); True when it changed."""
    old = None
    try: old = json.load(open(out, encoding='utf-8'))
    except (OSError, ValueError): pass
    req = urllib.request.Request(url, headers={'Accept-Encoding': 'gzip', 'User-Agent': 'ippai-corpus'})
    if old and old.get('source') == url and old.get('etag'): req.add_header('If-None-Match', old['etag'])
    try:
        r = urllib.request.urlopen(req, timeout=300)
    except urllib.error.HTTPError as e:
        if e.code == 304:
            print(f'mirror: catalog unchanged ({old["etag"]}), {len(old["trials"])} trials', file=sys.stderr)
            return False
        raise
    body = r.read()
    if r.headers.get('Content-Encoding') == 'gzip' or body[:2] == b'\x1f\x8b': body = gzip.decompress(body)
    catalog = json.loads(body)
    if catalog.get('schema_version') != 1:
        raise SystemExit(f'mirror: unknown schema_version {catalog.get("schema_version")!r}')
    data = {'source': url, 'etag': r.headers.get('ETag'), 'generated_at': catalog.get('generated_at'),
            'trials': trials(catalog)}
    tmp = out + '.part'
    with open(tmp, 'w', encoding='utf-8') as w: json.dump(data, w, ensure_ascii=False, indent=0)
    os.replace(tmp, out)
    print(f'mirror: catalog {data["generated_at"]}, {len(data["trials"])} trials', file=sys.stderr)
    return True


def index(data):
    idx = {}
    for t in data['trials']:
        for u in t['urls']: idx.setdefault(u, t)
    return idx


def lookup(data, url, idx=None):
    return (idx or index(data)).get(norm(url))


def main(argv):
    if len(argv) >= 2 and argv[0] == 'fetch':
        fetch(argv[1], *argv[2:3])
    elif len(argv) == 3 and argv[0] == 'lookup':
        t = lookup(json.load(open(argv[1], encoding='utf-8')), argv[2])
        if not t: return 1
        print(f"{t['download_url']}\t{t['sha256']}\t{t['size'] if t['size'] is not None else ''}")
    elif len(argv) == 3 and argv[0] == 'report':
        data = json.load(open(argv[1], encoding='utf-8'))
        idx = index(data)
        rows = list(csv.DictReader(open(argv[2], encoding='utf-8'), delimiter='\t'))
        have = [r['id'] for r in rows if lookup(data, r['url'], idx)]
        print(f'mirror {data.get("generated_at")}: {len(have)}/{len(rows)} corpus titles on the mirror, '
              f'the rest from their own sites')
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
