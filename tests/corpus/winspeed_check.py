#!/usr/bin/env python3
"""Whether a Windows run and a masu run of tests/corpus/winspeed.tsv measured the same screens
(the review of 2026-11-14: a warning page taken for the title, a title that never left for the
story on one side), and the start-up and CPU comparison only for rows where they did.

  python3 tests/corpus/winspeed_check.py WIN_OUT MASU_OUT [ids...] > table.md

WIN_OUT: winspeed.py's WORK/out (<id>-<n>/result.json, title.png); MASU_OUT: corpus-run.py's
output (<id>/result.json, title.png). A side's screen is its result's titleSig (shots.py sig),
else its title.png's. Per row:
- title: both sides' title screens are the same screen (shots.sig_same), and the row's
  title_sig, when it has one, is that screen on both sides;
- story: both left the title (leftTitle), and when the row has a story_sig both reached it;
- the times and cores are compared only when the title is the same; a row whose story differs
  has its cores marked: they are of different screens.
Exit 1 when a row's screens differ (so a run can be checked before its numbers are used)."""
import csv, json, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shots

HERE = os.path.dirname(os.path.abspath(__file__))


def load(p):
    try:
        return json.load(open(p, encoding='utf-8'))
    except (OSError, ValueError):
        return None


def screen(d, r):
    """a side's title signature: its result's, else its title.png's"""
    if r and r.get('titleSig'):
        return r['titleSig']
    p = os.path.join(d, 'title.png')
    if os.path.exists(p):
        try:
            return shots.sig(*shots.read(p))
        except (ValueError, IndexError):
            return None
    return None


def verdict(row, win, masu):
    """row: winspeed.tsv's; win: [(result, title sig)] of the Windows runs; masu: (result, sig).
    -> dict(title, story, why, win_title, masu_title, ratio, win_cores, masu_cores)"""
    out = {'title': False, 'story': False, 'why': [], 'win_title': None, 'masu_title': None, 'ratio': None,
           'win_cores': None, 'masu_cores': None}
    wins = [(r, s) for r, s in win if r and r.get('title') is not None]
    mr, ms = masu
    if not wins: out['why'].append('Windows: no title')
    if not mr or mr.get('title') is None: out['why'].append('masu: no title')
    if out['why']: return out
    want = row.get('title_sig') or ''
    # (the Windows run whose screen is masu's, the fastest of those)
    same = [(r, s) for r, s in wins if shots.sig_same(s, ms)]
    if not same:
        out['why'].append('title screens differ')
    elif want and not shots.sig_same(want, ms):
        out['why'].append("not the row's title_sig")
    else:
        out['title'] = True
    pick = min(same or wins, key=lambda x: x[0]['title'])[0]
    out['win_title'], out['masu_title'] = pick['title'], mr['title']
    if out['title'] and pick['title'] > 0:
        out['ratio'] = round(mr['title'] / pick['title'], 1)
    left = bool(pick.get('leftTitle')) and bool(mr.get('leftTitle'))
    story_want = row.get('story_sig') or ''
    if story_want:
        left = left and any(shots.sig_same(story_want, g) for g in pick.get('storySigs') or []) \
            and any(shots.sig_same(story_want, g) for g in mr.get('storySigs') or [])
    elif bool(pick.get('leftTitle')) != bool(mr.get('leftTitle')):
        out['why'].append('only one side left the title')
    out['story'] = out['title'] and left
    if not left and story_want: out['why'].append("not the row's story_sig on both sides")
    out['win_cores'], out['masu_cores'] = pick.get('storyCores'), mr.get('storyCores')
    return out


def main(a):
    win_out, masu_out, only = a[0], a[1], set(a[2:])
    rows = list(csv.DictReader(open(os.path.join(HERE, 'winspeed.tsv'), encoding='utf-8'), delimiter='\t'))
    L = ['| id | name | same title | story on both | Windows title s | masu title s | x | Windows cores | masu cores | why |',
         '|---|---|---|---|---|---|---|---|---|---|']
    bad = 0
    for row in rows:
        if only and row['id'] not in only: continue
        win = []
        for n in range(1, 10):
            d = os.path.join(win_out, f"{row['id']}-{n}")
            if not os.path.isdir(d): break
            r = load(os.path.join(d, 'result.json'))
            win.append((r, screen(d, r)))
        d = os.path.join(masu_out, row['id'])
        r = load(os.path.join(d, 'result.json'))
        v = verdict(row, win, (r, screen(d, r)))
        bad += not v['title']
        f = lambda x: '-' if x is None else f'{x:.2f}'
        t = lambda x: '-' if x is None else f'{x:.1f}'
        cores = (f(v['win_cores']), f(v['masu_cores'])) if v['story'] else (f(v['win_cores']) + ' (別画面)', f(v['masu_cores']) + ' (別画面)') if v['title'] else ('-', '-')
        L.append(f"| {row['id']} | {row['name']} | {'yes' if v['title'] else 'NO'} | {'yes' if v['story'] else 'no'} | "
                 f"{t(v['win_title'])} | {t(v['masu_title'])} | {t(v['ratio'])} | {cores[0]} | {cores[1]} | {'; '.join(v['why'])} |")
    print('\n'.join(L))
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]) if len(sys.argv) >= 3 else __doc__)
