#!/usr/bin/env python3
"""The shirusu report (.github/workflows/shirusu.yml, docs/shirusu.md): per title, the Windows
capture (windows/windows-<id>/windows.trace, its meta.txt and winplay's result.json) against sake's
lines (sake/<id>/console.log, converted by tools/shirusu), both through shirusu-conformance and
shirusu-compare; then the differences that recur across titles, by API and kind.

  report.py --tools DIR --convert SAKE_SHIRUSU --windows DIR --sake DIR --out DIR ID...

Writes OUT/report.md, OUT/summary.json and OUT/<id>/ (sake.trace, compare.json, conformance-*.json).
Standard library only."""
import argparse, collections, json, os, subprocess, sys


def run(cmd, out=None):
    """(exit code, stdout) of cmd; stdout also into the file out"""
    p = subprocess.run(cmd, capture_output=True, text=True)
    if out:
        open(out, 'w', encoding='utf-8').write(p.stdout)
    return p.returncode, p.stdout + p.stderr


def bits_of(meta_txt):
    """the game's width from prep's meta.txt (api, bits, exe)"""
    try:
        lines = open(meta_txt, encoding='utf-8').read().split('\n')
        return 64 if lines[1].strip() == '64' else 32
    except (OSError, IndexError):
        return 32


def load(path):
    try:
        return json.load(open(path, encoding='utf-8'))
    except (OSError, ValueError):
        return None


def reached(result):
    """how far a play got (corpus.mjs / winplay.py result.json): story, title, none, or '-'"""
    if not result:
        return '-'
    if result.get('title') is None:
        return 'none'
    return 'story' if result.get('leftTitle') else 'title'


def conformant(report):
    """shirusu-conformance --json: True, False, or None (none ran); failing check ids"""
    if report is None:
        return None, []
    fails = [x.get('id') for x in report.get('checks', []) if x.get('status') == 'fail']
    return bool(report.get('conformant')) and not fails, fails


def candidates(cmp):
    return (cmp or {}).get('candidates', [])


def summarize(rows):
    """recurring differences: (kind, api) -> titles it appears in; the severity counts"""
    by = collections.defaultdict(set)
    sev = collections.Counter()
    for r in rows:
        for c in r.get('candidates', []):
            by[(c.get('kind'), c.get('api') or '-')].add(r['id'])
            sev[c.get('severity')] += 1
    top = sorted(by.items(), key=lambda kv: (-len(kv[1]), kv[0]))
    return [{'kind': k, 'api': a, 'titles': sorted(v)} for (k, a), v in top], dict(sev)


def api_table(cmp):
    """compare's per-API call counts (left Windows, right sake)"""
    out = {}
    for k, v in ((cmp or {}).get('api_counts') or {}).items():
        if isinstance(v, dict):
            out[k] = (v.get('left', 0), v.get('right', 0))
    return out


def markdown(rows, recurring, sev):
    L = ['# shirusu: Windows capture vs sake', '',
         'Candidates are shirusu-compare\'s: differences in what was recorded, not verdicts.', '',
         'Windows alone / under shirusu / sake: how far each play got (story, title, none).', '',
         '| title | Windows alone | under shirusu | sake | Windows calls | sake calls | conformance W/S | high | medium | low | info |',
         '|---|---|---|---|---|---|---|---|---|---|---|']
    for r in rows:
        c = collections.Counter(x.get('severity') for x in r.get('candidates', []))
        conf = '/'.join({True: 'ok', False: 'FAIL', None: '-'}[x] for x in (r['win_conformant'], r['sake_conformant']))
        L.append(f"| {r['id']} | {r.get('win_plain', '-')} | {r['win_reached']} | {r['sake_reached']} | {r.get('win_calls', '-')} | {r.get('sake_calls', '-')} | {conf} | "
                 f"{c['high']} | {c['medium']} | {c['low']} | {c['info']} |")
    L += ['', f'Candidates by severity: {json.dumps(sev)}', '', '## Differences that recur', '',
          '| kind | API | titles |', '|---|---|---|']
    for x in recurring[:60]:
        L.append(f"| {x['kind']} | {x['api']} | {len(x['titles'])}: {' '.join(x['titles'][:8])}{' ...' if len(x['titles']) > 8 else ''} |")
    L += ['', '## Notes per title', '']
    for r in rows:
        if r.get('notes'):
            L.append(f"- {r['id']}: {'; '.join(r['notes'])}")
    return '\n'.join(L) + '\n'


def one(tid, a):
    out = os.path.join(a.out, tid)
    os.makedirs(out, exist_ok=True)
    wdir = os.path.join(a.windows, f'windows-{tid}')
    wtrace = os.path.join(wdir, 'windows.trace')
    log = os.path.join(a.sake, tid, 'console.log')
    r = {'id': tid, 'notes': [], 'candidates': [],
         'win_plain': reached(load(os.path.join(wdir, 'plain', 'result.json'))),
         'win_reached': reached(load(os.path.join(wdir, 'shots', 'result.json'))),
         'sake_reached': reached(load(os.path.join(a.sake, tid, 'result.json'))),
         'win_conformant': None, 'sake_conformant': None}
    bits = bits_of(os.path.join(wdir, 'meta.txt'))
    r['bits'] = bits
    strace = os.path.join(out, 'sake.trace')
    if os.path.exists(log):
        code, text = run([a.convert, log, strace, '--arch', 'x86_64' if bits == 64 else 'x86', '--target', tid])
        open(os.path.join(out, 'convert.txt'), 'w', encoding='utf-8').write(text)
        if code != 0:
            r['notes'].append('sake: ' + (text.strip().splitlines() or ['no calls'])[0][:200])
            strace = None
        else:
            r['sake_calls'] = int(text.split()[0])
    else:
        r['notes'].append('sake: no console.log')
        strace = None
    if os.path.exists(wtrace):
        # (what shirusu itself says it missed: children it could not inject, losses)
        _, text = run([os.path.join(a.tools, 'shirusu'), 'inspect', wtrace])
        for l in text.splitlines():
            l = l.strip()
            if l.startswith('note:') and any(k in l for k in ('could not be injected', 'loss events', 'not be recorded', 'failed')):
                r['notes'].append('Windows trace ' + l[:240])
    if not os.path.exists(wtrace):
        r['notes'].append('Windows: no trace')
        wtrace = None
    for side, t in (('win', wtrace), ('sake', strace)):
        if t:
            run([os.path.join(a.tools, 'shirusu-conformance'), t, '--json'], os.path.join(out, f'conformance-{side}.json'))
            ok, fails = conformant(load(os.path.join(out, f'conformance-{side}.json')))
            r[f'{side}_conformant'] = ok
            if fails:
                r['notes'].append(f'{side} conformance fails: {" ".join(map(str, fails))}')
    if wtrace and strace:
        p = os.path.join(out, 'compare.json')
        run([os.path.join(a.tools, 'shirusu-compare'), wtrace, strace, '--json', '--output', p])
        cmp = load(p)
        if cmp is None:
            r['notes'].append('compare wrote nothing')
        else:
            r['candidates'] = candidates(cmp)
            apis = api_table(cmp)
            r['apis'] = apis
            r['win_calls'] = sum(v[0] for v in apis.values())
            # (the sake side's count is the converter's: calls with no leave included)
            missing = sorted(k for k, (w, s) in apis.items() if w and not s)
            extra = sorted(k for k, (w, s) in apis.items() if s and not w)
            if missing: r['notes'].append('only Windows called ' + ' '.join(missing))
            if extra: r['notes'].append('only sake called ' + ' '.join(extra))
    return r


def main():
    ap = argparse.ArgumentParser()
    for k in ('tools', 'convert', 'windows', 'sake', 'out'):
        ap.add_argument('--' + k, required=True)
    ap.add_argument('ids', nargs='+')
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    rows = [one(t, a) for t in a.ids]
    recurring, sev = summarize(rows)
    json.dump({'titles': rows, 'recurring': recurring, 'severity': sev}, open(os.path.join(a.out, 'summary.json'), 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    open(os.path.join(a.out, 'report.md'), 'w', encoding='utf-8').write(markdown(rows, recurring, sev))
    print(markdown(rows, recurring, sev))


if __name__ == '__main__':
    main()
