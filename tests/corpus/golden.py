#!/usr/bin/env python3
"""Windows' answers for the corpus: tests/corpus/golden/<id>/ holds what the title showed on real
Windows (tests/corpus/winplay.py on .github/workflows/corpusref.yml): result.json (how far it got,
when, its size) and title.png / steps.png at half size (shots.half). sake's shots of the same
moments (tests/corpus.mjs) are compared with them (shots.compare) by tests/corpus/check.py.

  golden.py todo TITLES.tsv [--set smoke|full] [--again] [ID...]   ids that want Windows' answers
  golden.py import OUT_DIR                                        Windows' runs into golden/
  golden.py look ID SAKE_OUT_DIR                                   the comparison of one title

A title Windows could not bring to its title screen (a dialog the runner cannot answer, a launcher,
a download that failed) gets no answer: it is played again by the next `todo`, never compared.
"""
import csv, json, os, shutil, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import shots

GOLDEN = os.environ.get('CORPUS_GOLDEN') or os.path.join(HERE, 'golden')
SHOTS = ('title', 'steps')
# the levels of the `look` column: none (not compared, or it differs) < title < steps
LOOK = {'': 0, 'none': 0, 'title': 1, 'steps': 2}
KEEP = ('title', 'titleStill', 'firstWindow', 'size', 'shots', 'leftTitle', 'storyFrames', 'crashed', 'errors', 'manifests', 'steps', 'prep_error')


def todo(titles, kind='smoke', again=False, only=(), golden=GOLDEN):
    rows = list(csv.DictReader(open(titles, encoding='utf-8'), delimiter='\t'))
    ids = [r['id'] for r in rows if (r['id'] in only if only else (kind == 'full' or r.get('smoke') == '1'))]
    unknown = set(only) - {r['id'] for r in rows}
    if unknown: raise SystemExit(f'not in {titles}: {" ".join(sorted(unknown))}')
    return [i for i in ids if again or not os.path.exists(os.path.join(golden, i, 'result.json'))]


def import_runs(out, golden=GOLDEN):
    """OUT/<id>/result.json (+ PNGs) of winplay.py -> golden/<id>; returns the ids taken"""
    took = []
    for tid in sorted(os.listdir(out)):
        src = os.path.join(out, tid)
        if not os.path.isfile(os.path.join(src, 'result.json')): continue
        r = json.load(open(os.path.join(src, 'result.json'), encoding='utf-8'))
        if r.get('title') is None or not os.path.exists(os.path.join(src, 'title.png')):
            # (no title screen on Windows either: most often the runner's fault, a dialog it
            # cannot answer; not an answer, so the title is played again next time)
            print(f'{tid}: no title screen on Windows, not taken', file=sys.stderr)
            continue
        dst = os.path.join(golden, tid)
        shutil.rmtree(dst, ignore_errors=True)
        os.makedirs(dst)
        keep = {k: r[k] for k in KEEP if k in r}
        keep['shots'] = []
        for s in SHOTS:
            p = os.path.join(src, s + '.png')
            if r.get('title') is not None and os.path.exists(p):
                shots.write(os.path.join(dst, s + '.png'), *shots.half(*shots.read(p)))
                keep['shots'].append(s)
        with open(os.path.join(dst, 'result.json'), 'w', encoding='utf-8') as f:
            json.dump(keep, f, ensure_ascii=False, indent=1, sort_keys=True)
            f.write('\n')
        took.append(tid)
    return took


def look(tid, out, golden=GOLDEN):
    """sake's shots of one title (OUT/<id>) against Windows': {level, title, steps, why} or None
    when Windows has no answer. Cached in OUT/<id>/look.json (the corpus artifact keeps no PNGs
    of sake's run and is graded on another runner)"""
    cache = os.path.join(out, tid, 'look.json')
    g = os.path.join(golden, tid)
    if not os.path.exists(os.path.join(g, 'result.json')): return None
    gr = json.load(open(os.path.join(g, 'result.json'), encoding='utf-8'))
    if not gr.get('shots'): return None
    have = [s for s in SHOTS if os.path.exists(os.path.join(out, tid, s + '.png'))]
    if not have and os.path.exists(cache):
        return json.load(open(cache, encoding='utf-8'))
    res = {'level': 'none', 'why': ''}
    for s in SHOTS:
        if s not in gr['shots']: break
        p = os.path.join(out, tid, s + '.png')
        if not os.path.exists(p):
            res[s] = {'ok': False, 'why': 'no shot on sake'}
        else:
            try: res[s] = shots.compare(shots.read(os.path.join(g, s + '.png')), shots.read(p))
            except ValueError as e: res[s] = {'ok': False, 'why': str(e)}
        if not res[s]['ok']:
            res['why'] = f"{s}: {res[s]['why']}"
            break
        res['level'] = s
    with open(cache, 'w', encoding='utf-8') as f: json.dump(res, f, ensure_ascii=False)
    return res


if __name__ == '__main__':
    a = sys.argv[1:]
    if a[:1] == ['todo'] and len(a) >= 2:
        kind, again, only, rest = 'smoke', False, [], a[2:]
        while rest:
            x = rest.pop(0)
            if x == '--set': kind = rest.pop(0)
            elif x.startswith('--set='): kind = x[6:]
            elif x == '--again': again = True
            elif x.startswith('--again='): again = x[8:] == 'true'
            else: only.append(x)
        print(' '.join(todo(a[1], kind, again, only)))
    elif a[:1] == ['import'] and len(a) == 2:
        print('took:', ' '.join(import_runs(a[1])) or 'nothing')
    elif a[:1] == ['look'] and len(a) == 3:
        print(json.dumps(look(a[1], a[2]), ensure_ascii=False))
    else:
        sys.exit(__doc__)
