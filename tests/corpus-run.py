#!/usr/bin/env python3
"""Runs vnbox corpus titles on vnvm one after another (tests/corpus.mjs), preparing the next
title (download, extract, pack: tests/corpus-prep.sh) while the current one runs.

  tests/corpus-run.py TITLES.tsv IDS_FILE OUT_DIR [--steps STEPS.tsv] [--keep]

TITLES.tsv: id, url, exe_rel columns (header row). STEPS.tsv: id<TAB>steps<TAB>story_at<TAB>pre (x,y clicked before the title).
OUT_DIR/<id>/ gets result.json + PNGs; titles whose result.json exists are skipped.
"""
import csv, json, os, subprocess, sys, threading, queue, time, shutil, urllib.parse, socket

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
titles = {r['id']: r for r in csv.DictReader(open(sys.argv[1]), delimiter='\t')}
ids = [l.split()[0] for l in open(sys.argv[2]) if l.strip() and not l.startswith('#')]
out = sys.argv[3]
steps = {}
if '--steps' in sys.argv:
    for r in csv.reader(open(sys.argv[sys.argv.index('--steps') + 1]), delimiter='\t'):
        if r and not r[0].startswith('#'): steps[r[0]] = (r[1] if len(r) > 1 else '', r[2] if len(r) > 2 else '', r[3] if len(r) > 3 else '')
keep = '--keep' in sys.argv
TRIALS = os.environ.get('TRIALS', '/home/user/trials')
STORES = os.environ.get('STORES', '/home/user/runs/stores')
os.makedirs(STORES, exist_ok=True)
PORT = int(os.environ.get('PORT', '8092'))
env = dict(os.environ, PATH=f"{ROOT}/target/release:{os.environ['PATH']}")

def prep(tid):
    t = titles[tid]
    if os.path.isdir(f'{STORES}/{tid}') and os.path.exists(f'{STORES}/{tid}/exe.txt'):
        return open(f'{STORES}/{tid}/exe.txt').read().strip(), None
    r = subprocess.run(['bash', f'{HERE}/corpus-prep.sh', tid, t['url'], t.get('exe_rel', ''), f'{STORES}/{tid}'],
                       capture_output=True, text=True, env=env, timeout=4000)
    if r.returncode: return None, (r.stderr.strip() or f'exit {r.returncode}')[-300:]
    return r.stdout.strip().splitlines()[-1], None

todo = [i for i in ids if not os.path.exists(f'{out}/{i}/result.json')]
# several downloads at once (the hosts are slow per connection); a few titles ready ahead
JOBS = int(os.environ.get('PREP_JOBS', '4'))
q = queue.Queue(maxsize=JOBS)
work = queue.Queue()
for tid in todo: work.put(tid)
def producer():
    while True:
        try: tid = work.get_nowait()
        except queue.Empty: break
        try: exe, err = prep(tid)
        except Exception as e: exe, err = None, str(e)
        q.put((tid, exe, err))
    q.put(None)
for _ in range(JOBS): threading.Thread(target=producer, daemon=True).start()
ends = 0

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
    for _ in range(50):
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
        open(f'{od}/result.json', 'w').write('{"prep_error": %s}' % __import__('json').dumps(err))
        print(tid, 'PREP', err, flush=True)
    elif pe_machine(f"{TRIALS}/{tid}/x/{exe}", f'{STORES}/{tid}') == 0x8664:
        open(f'{od}/result.json', 'w').write('{"x64": true}')
        open(f'{od}/exe.txt', 'w').write(exe + '\n')
        print(tid, 'x64', flush=True)
    else:
        jit = f'/home/user/runs/jit/{tid}'
        os.makedirs(jit, exist_ok=True)
        srv = subprocess.Popen([f'{ROOT}/target/release/vnvm-server', '--web', f'{ROOT}/web/dist', '--store', f'{STORES}/{tid}',
                                '--jit', jit, '--listen', f'127.0.0.1:{PORT}', '--no-prerun'],
                               stdout=open(f'{od}/server.log', 'w'), stderr=subprocess.STDOUT)
        wait_port(PORT)
        winexe = 'Z:\\vnvm\\game\\' + exe.replace('/', '\\')
        args = titles[tid].get('args', '')
        engine = os.environ.get('ENGINE', '')  # ENGINE=sake: sake instead of Wine (no launcher helper: sake starts no 2nd process)
        if not engine and os.environ.get('AUTOCLICK', '1') == '1' and '_vnvm_autoclick.exe\t' in open(f'{STORES}/{tid}/trees/g.tsv', errors='replace').read():
            # through the launcher helper (host/tests/autoclick.c, packed by corpus-prep.sh)
            args = ','.join([winexe] + [a for a in args.split(',') if a])
            winexe = 'Z:\\vnvm\\game\\_vnvm_autoclick.exe'
        qs = 'autostart&headless&load&nosave&mount=/vnvm/game=g&run=' + urllib.parse.quote(winexe)
        if args: qs += '&args=' + urllib.parse.quote(args)
        if engine: qs += '&engine=' + engine
        if winexe.endswith('_vnvm_autoclick.exe'):
            qs += '&env=VNVM_ROOT_APP%3D0&env=VNVM_HELPER_EXE%3D_vnvm_autoclick.exe'  # small slots for the helper: the game gets the low app area
        st, at, pre = steps.get(tid, ('key:Return wait:3', '', ''))
        e2 = dict(env)
        if at: e2['STORY_AT'] = at
        if pre: e2['PRE'] = pre  # launcher/settings dialog button before the title
        t0 = time.time()
        try:
            subprocess.run(['node', f'{HERE}/corpus.mjs', f'http://127.0.0.1:{PORT}/wine.html?{qs}', od, st], env=e2,
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
    # with --keep a failed title's packed store stays (debugging needs no download); the extracted files go
    shutil.rmtree(f'{TRIALS}/{tid}', ignore_errors=True)
    try: ok = json.load(open(f'{od}/result.json')).get('title') is not None
    except Exception: ok = False
    # KEEP_STORES=1: keep every store (benchmark passes run the same titles again)
    if os.environ.get('KEEP_STORES') != '1' and (not keep or ok or shutil.disk_usage(STORES).free < int(os.environ.get('KEEP_MIN_GB', '8')) << 30):
        shutil.rmtree(f'{STORES}/{tid}', ignore_errors=True)
