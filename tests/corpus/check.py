#!/usr/bin/env python3
"""Grades corpus runs (tests/corpus-run.py output) against tests/corpus/titles.tsv.

  tests/corpus/check.py TITLES.tsv OUT_DIR... [--summary FILE] [--regressions FILE] [--update]

Each title gets a level: prep (download/extract failed) < none (no title screen) < title
(title screen) < story (the title's steps left the title and the story screens change).
A title whose level is below its `expect` column is a regression; the exit code is 1 when
there is one. The same for the `look` column: how far sake's screens match Windows' answers
(tests/corpus/golden, tests/corpus/golden.py look): none < title (the title screen matches)
< steps (the screen the title's steps lead to matches too). A download failure of an expected title is reported but not a failure (the
trial hosts come and go). The table also lists, per title, the DLLs and functions it asked for that sake does not
have (from console.log). --summary writes a Markdown table (GitHub step summary; repeatable),
--regressions the regressed ids (one per line, for a rerun), --update writes the levels
reached into the expect column (raising only, so a flaky run does not lower a baseline;
--update set takes the levels as they are).
"""
import argparse, csv, json, os, re, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import golden

RANK = {'prep': 0, 'none': 1, 'title': 2, 'story': 3}


def level(r):
    if 'prep_error' in r: return 'prep'
    if r.get('title') is None: return 'none'
    if r.get('leftTitle') and r.get('storyFrames', 0) >= 2 and not r.get('crashed'): return 'story'
    return 'title'


ap = argparse.ArgumentParser()
ap.add_argument('titles')
ap.add_argument('outs', nargs='+')
ap.add_argument('--summary', action='append', default=[])
ap.add_argument('--regressions')
ap.add_argument('--update', nargs='?', const='raise', choices=['raise', 'set'])
A = ap.parse_args()
titles_path, outs = A.titles, A.outs
rows = list(csv.DictReader(open(titles_path, encoding='utf-8'), delimiter='\t'))
cols = list(rows[0].keys())
res = {}
for out in outs:
    for root, dirs, files in os.walk(out):
        if 'result.json' in files:
            tid = os.path.basename(root)
            try: r = json.load(open(f'{root}/result.json'))
            except Exception: r = {'run_error': 'bad result.json'}
            # the DLLs the title asked for that sake has not got (imports, and LoadLibrary with
            # SAKE_TRACE_LOAD), and any it still took from a prefix
            dlls = []
            if os.path.exists(f'{root}/console.log'):
                log = open(f'{root}/console.log', errors='replace').read()
                names = re.findall(r'sake: import from (\S+?):', log) + re.findall(r'sake: LoadLibrary\(([^)]+)\)', log)
                # (a LoadLibrary of a path: its file name)
                dlls = sorted(set(re.split(r'[\\/]', m)[-1].lower() for m in names) |
                              set('prefix:' + m for m in re.findall(r'sake: prefix DLL (\S+)', log)))
                # and the functions it called that sake has neither implemented nor declared
                dlls += sorted(set(re.findall(r'sake: call to (\S+), which we neither implement', log)))[:10]
            res[tid] = (r, dlls, os.path.dirname(root))

lines, regressed, counts, better, looks = [], [], {}, [], {}
for row in rows:
    tid = row['id']
    if tid not in res: continue
    r, dlls, out = res[tid]
    lk = golden.look(tid, out)
    got_look = lk['level'] if lk else ''
    lv = level(r)
    counts[lv] = counts.get(lv, 0) + 1
    exp = row.get('expect') or ''
    mark = ''
    if exp and RANK[lv] < RANK[exp]:
        if lv == 'prep': mark = '⚠ download'
        else: mark = '❌ regression'; regressed.append(tid)
    elif exp and RANK[lv] > RANK[exp]: mark = '⬆ better'; better.append(tid)
    elif not exp: mark = 'new'
    want_look = row.get('look') or ''
    if lk:
        looks[got_look] = looks.get(got_look, 0) + 1
        if golden.LOOK[got_look] < golden.LOOK[want_look]:
            if lv == 'prep': mark = mark or '⚠ download'
            elif tid not in regressed: mark = '❌ regression'; regressed.append(tid)
        elif golden.LOOK[got_look] > golden.LOOK[want_look] and '❌' not in mark: mark = (mark + ' ⬆ looks better').strip()
    shown = '-' if not lk else got_look + (f" ({lk['why'][:60]})" if lk.get('why') else '')
    t = r.get('title')
    err = (r.get('prep_error') or r.get('run_error') or (r.get('errors') or [''])[0] or '').replace('|', '/').replace('\n', ' ')[:120]
    lines.append(f"| {tid} | {row.get('engine', '')} | {exp or '-'} | {lv} | {want_look or '-'} | {shown} | {mark} | {'' if t is None else round(t, 1)} | {' '.join(dlls)} | {err} |")
    if A.update:
        if A.update == 'set' or not exp or RANK[lv] > RANK[exp]:
            row['expect'] = lv if lv != 'prep' else exp
        if lk and lv != 'prep' and (A.update == 'set' or golden.LOOK[got_look] > golden.LOOK[want_look]):
            row['look'] = got_look if got_look != 'none' else ''

total = sum(counts.values())
head = [f"### Corpus on sake: {total} titles",
        '',
        ' / '.join(f'{k} {counts.get(k, 0)}' for k in ('story', 'title', 'none', 'prep')) +
        f' — regressions {len(regressed)}, better than expected {len(better)}',
        '',
        'Like Windows (tests/corpus/golden): ' + ' / '.join(f'{k} {looks.get(k, 0)}' for k in ('steps', 'title', 'none')) +
        f' of {sum(looks.values())} titles Windows answered',
        '',
        '| id | engine | expect | got | look expected | looks like Windows | | title s | missing DLLs / functions | first error |',
        '|---|---|---|---|---|---|---|---|---|---|']
# regressions first, then the rest in titles.tsv order
key = lambda l: (0 if '❌' in l else 1 if '⬆' in l else 2)
text = '\n'.join(head + sorted(lines, key=key)) + '\n'
print(text)
for s in A.summary: open(s, 'a').write(text)
if A.regressions: open(A.regressions, 'w').write(''.join(i + '\n' for i in regressed))
if A.update:
    with open(titles_path, 'w', encoding='utf-8', newline='') as f:
        w = csv.DictWriter(f, fieldnames=cols, delimiter='\t', lineterminator='\n')
        w.writeheader(); w.writerows(rows)
sys.exit(1 if regressed else 0)
