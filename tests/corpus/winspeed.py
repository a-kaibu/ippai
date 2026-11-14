#!/usr/bin/env python3
"""Real-Windows start-up and CPU numbers for the trials in targets.tsv (thread 起動の遅さと1コア張り付きを
Windows実機と比べる) in tests/corpus/winspeed.tsv. For each row: download from the VN Mirror Archive (sha256 checked), unpack,
pick the exe (ippai's tests/corpus/pickexe.py), run it with tests/corpus/winplay.py (the same
title / steps / story / CPU rules as tests/corpus.mjs on sake), RUNS times.

  python tests/corpus/winspeed.py WORK [ids...]
  env: RUNS (2), STORY (30), IDLE (10), TITLE_MAX (150)

WORK/dl/<file>, WORK/x/<id>/ (unpacked), WORK/out/<id>-<n>/result.json; WORK/summary.tsv.
"""
import csv, hashlib, json, os, subprocess, sys, urllib.request, zipfile

here = os.path.dirname(os.path.abspath(__file__))
work = os.path.abspath(sys.argv[1])
only = set(sys.argv[2:])
RUNS = int(os.environ.get('RUNS', 2))
os.environ.setdefault('STORY', '30'); os.environ.setdefault('IDLE', '10'); os.environ.setdefault('TITLE_MAX', '150')
C = here


def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''): h.update(b)
    return h.hexdigest()


def fetch(row):
    p = os.path.join(work, 'dl', row['filename'])
    if os.path.exists(p) and sha(p) == row['sha256']: return p
    os.makedirs(os.path.dirname(p), exist_ok=True)
    req = urllib.request.Request(row['url'], headers={'User-Agent': 'ippai-corpus'})
    with urllib.request.urlopen(req) as r, open(p + '.part', 'wb') as f:
        while True:
            b = r.read(1 << 20)
            if not b: break
            f.write(b)
    os.replace(p + '.part', p)
    if sha(p) != row['sha256']: raise SystemExit(f'{p}: sha256 mismatch')
    return p


def name_of(i):
    if i.flag_bits & 0x800: return i.filename
    try: return i.filename.encode('cp437').decode('cp932')
    except UnicodeError: return i.filename


def unzip(z, d):
    with zipfile.ZipFile(z) as zf:
        for i in zf.infolist():
            n = name_of(i).replace('\\', '/')
            t = os.path.join(d, *[s for s in n.split('/') if s not in ('', '..')])
            if n.endswith('/'): os.makedirs(t, exist_ok=True); continue
            os.makedirs(os.path.dirname(t), exist_ok=True)
            with zf.open(i) as s, open(t, 'wb') as o:
                while True:
                    b = s.read(1 << 20)
                    if not b: break
                    o.write(b)
    # (a zip inside the zip: unpacked beside it, once)
    for root, _, fs in os.walk(d):
        for f in fs:
            if f.lower().endswith('.zip'):
                p = os.path.join(root, f)
                try: unzip(p, p[:-4]); os.remove(p)
                except zipfile.BadZipFile: pass


def unpack(row, z):
    d = os.path.join(work, 'x', row['id'])
    if os.path.exists(os.path.join(d, '.done')): return d
    os.makedirs(d, exist_ok=True)
    if z.lower().endswith('.zip'): unzip(z, d)
    else: raise SystemExit(f'{z}: only zips here')
    open(os.path.join(d, '.done'), 'w').close()
    return d


def main():
    rows = list(csv.DictReader(open(os.path.join(here, 'winspeed.tsv'), encoding='utf-8'), delimiter='\t'))
    summ = []
    for row in rows:
        if only and row['id'] not in only: continue
        try:
            d = unpack(row, fetch(row))
            exe = subprocess.run([sys.executable, os.path.join(C, 'pickexe.py'), d, row['filename']],
                                 capture_output=True, text=True, encoding='utf-8').stdout.strip()
            if row.get('exe'): exe = row['exe']
            if not exe: raise RuntimeError('no exe')
        except Exception as e:
            print(row['id'], 'prep failed:', e, flush=True)
            summ.append([row['id'], row['name'], '', 'prep: ' + str(e)])
            continue
        for n in range(1, RUNS + 1):
            out = os.path.join(work, 'out', f"{row['id']}-{n}")
            env = dict(os.environ)
            if row.get('pre'): env['PRE'] = row['pre']
            subprocess.run([sys.executable, os.path.join(C, 'winplay.py'), 'run', os.path.join(d, *exe.split('/')), out,
                            row.get('steps') or 'key:Return wait:3'], env=env)
            try: r = json.load(open(os.path.join(out, 'result.json'), encoding='utf-8'))
            except Exception as e: r = {'errors': [str(e)]}
            line = [row['id'], row['name'], exe, n] + [r.get(k) for k in
                    ('firstWindow', 'title', 'titleStill', 'titleCores', 'storyCores', 'idleCores', 'leftTitle', 'size', 'crashed')]
            print(*line, sep='\t', flush=True)
            summ.append(line)
    with open(os.path.join(work, 'summary.tsv'), 'a', encoding='utf-8') as f:
        for l in summ: f.write('\t'.join('' if v is None else str(v) for v in l) + '\n')


main()
