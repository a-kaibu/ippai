#!/usr/bin/env python3
"""Runs masu corpus titles on vnvm one after another (tests/corpus.mjs), preparing the next
title (download, extract, pack: tests/corpus-prep.sh) while the current one runs.

  tests/corpus-run.py TITLES.tsv IDS_FILE OUT_DIR [--steps STEPS.tsv] [--keep]

TITLES.tsv: id, url, exe_rel columns (header row); optional steps, story_at, pre, title_at, title_still, args columns.
STEPS.tsv: id<TAB>steps<TAB>story_at<TAB>pre (x,y clicked before the title)<TAB>title_at (seconds:
the title from then on however few colours it has, a dark menu on black), wins over the columns.
IDS_FILE "-": every title of TITLES.tsv.
OUT_DIR/<id>/ gets result.json + PNGs; titles whose result.json exists are skipped.
env: MASU_DIST (a release directory: vnvm, web/; default the checkout's
target/release + web/dist), JIT_DIR, SKIP_X64=0 (run x64 exes too: sake runs them), PORT,
TRIALS, STORES, BASE_STORE (corpus-prep.sh), PREP_JOBS, CORPUS_QS (appended to the page's query).
"""
import csv, json, os, subprocess, sys, threading, queue, time, shutil, urllib.parse, socket

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
titles = {r['id']: r for r in csv.DictReader(open(sys.argv[1]), delimiter='\t')}
ids = list(titles) if sys.argv[2] == '-' else [l.split()[0] for l in open(sys.argv[2]) if l.strip() and not l.startswith('#')]
out = sys.argv[3]
steps = {i: (r.get('steps') or 'key:Return wait:3', r.get('story_at') or '', r.get('pre') or '', r.get('title_at') or '') for i, r in titles.items() if r.get('steps') or r.get('story_at') or r.get('pre') or r.get('title_at')}
if '--steps' in sys.argv:
    for r in csv.reader(open(sys.argv[sys.argv.index('--steps') + 1]), delimiter='\t'):
        if r and not r[0].startswith('#'): steps[r[0]] = tuple(r[k] if len(r) > k else '' for k in range(1, 5))
keep = '--keep' in sys.argv
TRIALS = os.environ.get('TRIALS', '/home/user/trials')
STORES = os.environ.get('STORES', '/home/user/runs/stores')
os.makedirs(STORES, exist_ok=True)
PORT = int(os.environ.get('PORT', '8092'))
DIST = os.environ.get('MASU_DIST')
BIN = DIST or f'{ROOT}/target/release'
WEB = f'{DIST}/web' if DIST else f'{ROOT}/web/dist'
JIT_DIR = os.environ.get('JIT_DIR', '/home/user/runs/jit')
SKIP_X64 = os.environ.get('SKIP_X64', '1') == '1'
env = dict(os.environ, PATH=f"{BIN}:{os.environ['PATH']}")
# (sake for node: corpus-prep.sh runs setup programs in it; a release's sake-node/, else the
# checkout's crates/sake/build.sh output)
for sake in (f'{BIN}/sake-node/sake.js', f'{ROOT}/target/sake-out/sake.js'):
    if 'SAKE_NODE' not in env and os.path.exists(sake): env['SAKE_NODE'] = sake


def installed(exe):
    """A game a setup program installed (corpus-prep.sh prints C:\\...)."""
    return len(exe) > 2 and exe[1] == ':' and exe[0].isalpha()


def exe_file(tid, exe):
    """The exe's file in the prepared trial: the tree's, or the installer's C:\\ ($T/c)."""
    if installed(exe): return f"{TRIALS}/{tid}/c/" + exe[3:].replace('\\', '/')
    return f"{TRIALS}/{tid}/x/{exe}"

def prep(tid):
    t = titles[tid]
    if os.path.isdir(f'{STORES}/{tid}') and os.path.exists(f'{STORES}/{tid}/exe.txt'):
        return open(f'{STORES}/{tid}/exe.txt').read().strip(), None
    r = subprocess.run(['bash', f'{HERE}/corpus-prep.sh', tid, t['url'], t.get('exe_rel', ''), f'{STORES}/{tid}'],
                       capture_output=True, text=True, env=env, timeout=int(os.environ.get('PREP_TIMEOUT', '4000')))
    if r.returncode: return None, (r.stderr.strip() or f'exit {r.returncode}')[-300:]
    return r.stdout.strip().splitlines()[-1], None

todo = [i for i in ids if not os.path.exists(f'{out}/{i}/result.json')]
# several downloads at once (the hosts are slow per connection); a few titles ready ahead
JOBS = int(os.environ.get('PREP_JOBS', '4'))
q = queue.Queue(maxsize=JOBS)
work = queue.Queue()
for tid in todo: work.put(tid)
PREP_S = {}  # download + extract + pack seconds per title
def producer():
    while True:
        try: tid = work.get_nowait()
        except queue.Empty: break
        t = time.time()
        try: exe, err = prep(tid)
        except Exception as e: exe, err = None, str(e)
        PREP_S[tid] = round(time.time() - t)
        q.put((tid, exe, err))
    q.put(None)
for _ in range(JOBS): threading.Thread(target=producer, daemon=True).start()
ends = 0

def web_game(store, exe):
    """a package.json next to the exe (NW.js, unpacked by masu pack) or in resources/app (Electron): the page runs it, x64 or not"""
    d = exe.rsplit('/', 1)[0] + '/' if '/' in exe else ''
    if installed(exe): return False
    want = {f'{d}package.json'.lower(), f'{d}resources/app/package.json'.lower()}
    try:
        return any(l.split('\t')[3].lower() in want for l in open(f'{store}/trees/g.tsv', encoding='utf-8') if l.startswith('f\t'))
    except OSError:
        return False


def pe_machine(path, store):
    """the exe's PE machine (0x14c x86, 0x8664 x64), from the extracted file"""
    try:
        with open(path, 'rb') as f:
            d = f.read(4096)
        o = int.from_bytes(d[0x3c:0x40], 'little')
        return int.from_bytes(d[o + 4:o + 6], 'little')
    except Exception:
        return 0

def wait_port(p):
    # (a minute: a server makes sake's C:\\ again at its start when the store's is older than its
    # fonts or RPG Maker RTPs, ~20 s on a busy runner; the page found no server after 10 s)
    for _ in range(300):
        try: socket.create_connection(('127.0.0.1', p), 0.2).close(); return True
        except OSError: time.sleep(0.2)
    return False

while True:
    item = q.get()
    if item is None:
        ends += 1
        if ends == JOBS: break
        continue
    tid, exe, err = item
    od = f'{out}/{tid}'
    os.makedirs(od, exist_ok=True)
    if not exe:
        # (corpus-prep.sh's "not a game: movie (...)": a sample movie or a patch, kept apart)
        ng = err.split('not a game: ', 1)[1].split()[0] if 'not a game: ' in (err or '') else None
        open(f'{od}/result.json', 'w').write(json.dumps({'prep_error': err, **({'not_game': ng} if ng else {})}, ensure_ascii=False))
        print(tid, 'PREP', err, flush=True)
    elif SKIP_X64 and not web_game(f'{STORES}/{tid}', exe) and pe_machine(exe_file(tid, exe), f'{STORES}/{tid}') == 0x8664:
        open(f'{od}/result.json', 'w').write('{"x64": true}')
        open(f'{od}/exe.txt', 'w').write(exe + '\n')
        print(tid, 'x64', flush=True)
    else:
        jit = f'{JIT_DIR}/{tid}'
        os.makedirs(jit, exist_ok=True)
        srv = subprocess.Popen([f'{BIN}/masu', 'serve', '--foreground', '--web', WEB, '--store', f'{STORES}/{tid}',
                                '--jit', jit, '--listen', f'127.0.0.1:{PORT}', '--no-prerun',
                                # (web games' origin; the default, PORT + 1, is the next lane's PORT)
                                '--game-listen', f'127.0.0.1:{PORT + 1000}'],
                               stdout=open(f'{od}/server.log', 'w'), stderr=subprocess.STDOUT)
        wait_port(PORT)
        # (an installed game: C:\\ is the page's /masu/prefix/drive_c, and the page's run= takes
        # a Z:\\ path; sake shows the program its C:\\ path)
        winexe = 'Z:\\masu\\prefix\\drive_c' + exe[2:] if installed(exe) else 'Z:\\masu\\game\\' + exe.replace('/', '\\')
        args = titles[tid].get('args', '')
        # (saves on: a fresh browser each title, and a launcher's settings file reaches the game it
        # starts, BISHOP's bsxx.ini)
        qs = 'autostart&headless&load&mount=/masu/game=g&run=' + urllib.parse.quote(winexe)
        if args: qs += '&args=' + urllib.parse.quote(args)
        qs += os.environ.get('CORPUS_QS', '')  # (more page parameters, e.g. &env=NAME%3Dvalue)
        st, at, pre, tat = steps.get(tid, ('key:Return wait:3', '', '', ''))
        e2 = dict(env)
        if at: e2['STORY_AT'] = at
        if pre: e2['PRE'] = pre  # launcher/settings dialog button before the title
        if tat: e2['TITLE_AT'] = tat
        if titles[tid].get('title_still'): e2['TITLE_STILL'] = titles[tid]['title_still']
        t0 = time.time()
        try:
            subprocess.run(['node', f'{HERE}/corpus.mjs', f'http://127.0.0.1:{PORT}/play.html?{qs}', od, st], env=e2,
                           timeout=600, stdout=open(f'{od}/run.log', 'w'), stderr=subprocess.STDOUT)
        except subprocess.TimeoutExpired:
            open(f'{od}/run.log', 'a').write('TIMEOUT\n')
        srv.terminate(); srv.wait()
        shutil.rmtree(jit, ignore_errors=True)
        if not os.path.exists(f'{od}/result.json'):
            open(f'{od}/result.json', 'w').write('{"run_error": "no result"}')
        open(f'{od}/exe.txt', 'w').write(exe + '\n')
        if os.path.isdir(f'{STORES}/{tid}'): open(f'{STORES}/{tid}/exe.txt', 'w').write(exe + '\n')
        print(tid, 'done', round(time.time() - t0), flush=True)
    # how long the title took (prep: download, extract, pack)
    try:
        r = json.load(open(f'{od}/result.json'))
        r['prep_s'] = PREP_S.get(tid)
        # where the game came from (tests/corpus-prep.sh: mirror, origin; none when it was ready)
        if os.path.exists(f'{TRIALS}/{tid}/source'): r['source'] = open(f'{TRIALS}/{tid}/source').read().strip()
        json.dump(r, open(f'{od}/result.json', 'w'), ensure_ascii=False, indent=1)
    except Exception: pass
    # with --keep a failed title's packed store stays (debugging needs no download); the extracted files go
    shutil.rmtree(f'{TRIALS}/{tid}', ignore_errors=True)
    try: ok = json.load(open(f'{od}/result.json')).get('title') is not None
    except Exception: ok = False
    # KEEP_STORES=1: keep every store (benchmark passes run the same titles again)
    if os.environ.get('KEEP_STORES') != '1' and (not keep or ok or shutil.disk_usage(STORES).free < int(os.environ.get('KEEP_MIN_GB', '8')) << 30):
        shutil.rmtree(f'{STORES}/{tid}', ignore_errors=True)
