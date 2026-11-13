#!/usr/bin/env python3
"""tests/corpus-prep.sh unpacking a self-extracting .exe: each extractor it tries starts from an
empty folder, so what a failed one left half done never sits beside the next one's copy (Front
Wing's canary_game_preview.exe: unar failed half way through the LHa self-extractor, leaving a
SYSTEM.PAK cut at 1.75 MB and an empty IMAGE.PAK under the converted names, 7-Zip then
unpacked it whole under Shift-JIS names, and the game was run from the broken copy).

The extractors are stand-ins on PATH (innoextract, unar, 7z, unrar) that write the files they
are told to and exit as told. A fixed case for that title, then random runs of outcomes: the
tree prep leaves is exactly the first successful extractor's, or the exe itself when none
succeeds.

  python3 tests/corpus/prep_test.py
"""
import json, os, random, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
PREP = os.path.join(os.path.dirname(HERE), 'corpus-prep.sh')
TOOLS = ['innoextract', 'unar', '7z', 'unrar']

FAKE = r'''#!/usr/bin/env python3
import json, os, sys
# (the installers prep finds inside the unpacked tree, innoextract -d: not this test's)
if '-d' in sys.argv: sys.exit(2)
plan = json.loads(os.environ.get('FAKE_PLAN', '{}')).get(os.path.basename(sys.argv[0]))
if not plan: sys.exit(2)
# (7z l -slt: the listing the plan gives, if any)
if sys.argv[1:2] == ['l']:
    # (only for the self-extractor itself: what it unpacks is no archive)
    if 'list' not in plan or open(sys.argv[-1], 'rb').read() != b'sfx': sys.exit(2)
    print(plan['list'])
    sys.exit(0)
# (unar -o DIR; the others write into the current folder)
out = '.'
for flag in ('-o',):
    if flag in sys.argv: out = sys.argv[sys.argv.index(flag) + 1]
for name, data in plan['files'].items():
    p = os.path.join(out, name)
    os.makedirs(os.path.dirname(p) or '.', exist_ok=True)
    open(p, 'w').write(data)
sys.exit(plan['exit'])
'''


def run(plan, tmp, ext='exe'):
    """corpus-prep.sh on a dummy dl.<ext> with the stand-ins following `plan`: {tool: {files, exit}}.
    Returns ({relative path: content} of the tree, printed exe)."""
    bin_ = os.path.join(tmp, 'bin')
    os.makedirs(bin_, exist_ok=True)
    for t in TOOLS:
        p = os.path.join(bin_, t)
        open(p, 'w').write(FAKE)
        os.chmod(p, 0o755)
    trials = os.path.join(tmp, 'trials')
    shutil.rmtree(trials, ignore_errors=True)
    os.makedirs(os.path.join(trials, 't'))
    # (not a PE the Python unpackers know: isextract.py and zsfx.py fail on it)
    open(os.path.join(trials, 't', f'dl.{ext}'), 'wb').write(b'not an installer')
    env = dict(os.environ, PATH=bin_ + os.pathsep + os.environ['PATH'], TRIALS=trials,
               FAKE_PLAN=json.dumps(plan))
    r = subprocess.run(['bash', PREP, 't', f'http://example.invalid/canary_game_preview.{ext}', '', ''],
                       env=env, capture_output=True, text=True, timeout=120)
    x = os.path.join(trials, 't', 'x')
    tree = {}
    for d, _, fs in os.walk(x):
        for f in fs:
            p = os.path.join(d, f)
            tree[os.path.relpath(p, x)] = open(p, 'rb').read().decode('utf-8', 'replace')
    return tree, r.stdout.strip().splitlines()[-1] if r.returncode == 0 and r.stdout.strip() else None


def expect(plan):
    """the tree of the first stand-in that succeeds with files, in prep's order; else the exe"""
    for t in ['innoextract', 'unar', '7z', 'unrar']:
        p = plan.get(t)
        if p and p['exit'] == 0 and p['files']:
            return p['files']
    return {'canary_game_preview.exe': 'not an installer'}


def check(plan, tmp, what):
    tree, exe = run(plan, tmp)
    want = expect(plan)
    if tree != want:
        sys.exit(f'FAIL {what}: plan {json.dumps(plan, ensure_ascii=False)}\n got {tree}\n want {want}')
    if exe is None:
        sys.exit(f'FAIL {what}: no exe printed')


def main():
    tmp = tempfile.mkdtemp(prefix='prep_test')
    try:
        # the title: unar fails half way (cut files under the converted names), 7-Zip succeeds
        canary = {
            'unar': {'files': {'カナリア 体験版/SYSTEM.PAK': 'cut', 'カナリア 体験版/IMAGE.PAK': ''}, 'exit': 1},
            '7z': {'files': {'canary/ADV.EXE': 'MZ', 'canary/SYSTEM.PAK': 'whole', 'canary/IMAGE.PAK': 'whole'}, 'exit': 0},
        }
        check(canary, tmp, 'canary')
        tree, exe = run(canary, tmp)
        assert exe == 'canary/ADV.EXE', exe
        # a RAR5 self-extractor of the game inside the zip (CYCLET's tokage_trial_kanau_web.exe):
        # unar does not know it, 7-Zip lists it, unrar unpacks it in place of the exe
        rar = {
            'unar': {'files': {'tokage/tokage_web.exe': 'sfx'}, 'exit': 0},
            '7z': {'list': 'Path = x\nType = Rar5\n\n----------\nPath = 叶/data.vaw\n\nPath = 叶/tokage_trial.exe\n', 'files': {}, 'exit': 0},
            'unrar': {'files': {'叶/tokage_trial.exe': 'MZ', '叶/data.vaw': 'v'}, 'exit': 0},
        }
        tree, exe = run(rar, tmp, 'zip')
        want = {'tokage/tokage_web.exe.sfx/叶/tokage_trial.exe': 'MZ', 'tokage/tokage_web.exe.sfx/叶/data.vaw': 'v'}
        if tree != want or exe != 'tokage/tokage_web.exe.sfx/叶/tokage_trial.exe':
            sys.exit(f'FAIL rar5 sfx: got {tree} {exe}')
        # (an exe 7-Zip lists as something else stays as it is)
        rar['7z']['list'] = rar['7z']['list'].replace('Rar5', 'PE')
        tree, exe = run(rar, tmp, 'zip')
        if tree != {'tokage/tokage_web.exe': 'sfx'}:
            sys.exit(f'FAIL not rar: got {tree}')
        # random runs: each stand-in fails (leaving files or not), succeeds, or is missing
        rng = random.Random(int(os.environ.get('SEED', '20261113')))
        for i in range(int(os.environ.get('RUNS', '25'))):
            plan = {}
            for t in TOOLS:
                k = rng.choice(['missing', 'fail', 'fail-files', 'ok'])
                if k == 'missing': continue
                files = {} if k == 'fail' else {f'{t}{rng.randrange(3)}/g{j}.{"exe" if j == 0 else "dat"}': f'{t}{j}' for j in range(rng.randrange(1, 4))}
                plan[t] = {'files': files, 'exit': 0 if k == 'ok' else 1}
            check(plan, tmp, f'run {i}')
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    print('prep_test: ok')


if __name__ == '__main__':
    main()
