#!/usr/bin/env python3
"""Use-case coverage: the entry points games call (IPPAI_API_COUNTS tallies in corpus logs)
set beside what the tests call (the same tallies from the unit tests and the winref probes).

    usage.py --games DIR... --tests FILE_OR_DIR... [--md out.md] [--top N] [--prefix d3d,gl]

A tally is the last `api-counts k=n ...` line of a log (crates/kura/src/usage.rs). Games:
each log under DIR is one run, named by its directory. Prints, per key kind (d3d, api, stub,
glmissing), how many of the keys games use the tests reach, weighted by the number of games
and by calls; then the keys games use that no test reaches, most used first. `stub` keys
(COM methods sake does not implement) and `glmissing` keys (GL functions kura does not have)
are listed on their own: a game reached code that is not there."""
import argparse, collections, os, re, sys


def tally(path):
    last = None
    try:
        with open(path, encoding='utf-8', errors='replace') as f:
            for line in f:
                i = line.find('api-counts')
                if i >= 0:
                    last = line[i:]
    except OSError:
        return None
    if last is None:
        return None
    t = {}
    for kv in last.split()[1:]:
        k, _, n = kv.rpartition('=')
        if k and n.isdigit():
            t[k] = int(n)
    return t


def logs(paths):
    for p in paths:
        if os.path.isfile(p):
            yield p
            continue
        for d, _, fs in os.walk(p):
            for f in fs:
                if f.endswith(('.log', '.txt')):
                    yield os.path.join(d, f)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--games', nargs='+', required=True)
    ap.add_argument('--tests', nargs='+', default=[])
    ap.add_argument('--md')
    ap.add_argument('--top', type=int, default=60)
    ap.add_argument('--prefix', default='', help='only keys of these kinds (comma-separated)')
    A = ap.parse_args()
    kinds = set(filter(None, A.prefix.split(',')))
    keep = lambda k: not kinds or k.split(':', 1)[0] in kinds
    games = collections.defaultdict(set)  # key -> runs
    calls = collections.Counter()
    runs = 0
    for p in logs(A.games):
        t = tally(p)
        if not t:
            continue
        runs += 1
        run = os.path.basename(os.path.dirname(p)) or p
        for k, n in t.items():
            if keep(k):
                games[k].add(run)
                calls[k] += n
    tested = set()
    for p in logs(A.tests):
        t = tally(p)
        if t:
            tested |= set(t)
    out = []
    out.append(f'# Use-case coverage\n\n{runs} game runs, {len(games)} entry points they call, {len(tested)} the tests call.\n')
    out.append('| kind | used by games | tested | by games weighted | by calls weighted |')
    out.append('|---|---|---|---|---|')
    by_kind = collections.defaultdict(list)
    for k in games:
        by_kind[k.split(':', 1)[0]].append(k)
    for kind, ks in sorted(by_kind.items()):
        hit = [k for k in ks if k in tested]
        gw = sum(len(games[k]) for k in ks) or 1
        cw = sum(calls[k] for k in ks) or 1
        out.append(f'| {kind} | {len(ks)} | {len(hit)} | {100 * sum(len(games[k]) for k in hit) / gw:.1f}% | {100 * sum(calls[k] for k in hit) / cw:.1f}% |')
    miss = sorted((k for k in games if k not in tested and not k.startswith(('stub:', 'glmissing:'))), key=lambda k: (-len(games[k]), -calls[k], k))
    out.append(f'\n## Called by games, reached by no test ({len(miss)})\n')
    out.append('| entry point | games | calls |')
    out.append('|---|---|---|')
    for k in miss[:A.top]:
        out.append(f'| `{k}` | {len(games[k])} | {calls[k]} |')
    absent = sorted((k for k in games if k.startswith(('stub:', 'glmissing:'))), key=lambda k: (-len(games[k]), -calls[k], k))
    out.append(f'\n## Called by games, not implemented ({len(absent)})\n')
    out.append('| entry point | games | calls |')
    out.append('|---|---|---|')
    for k in absent[:A.top]:
        out.append(f'| `{k}` | {len(games[k])} | {calls[k]} |')
    text = '\n'.join(out) + '\n'
    if A.md:
        open(A.md, 'w', encoding='utf-8').write(text)
    sys.stdout.write(text)


if __name__ == '__main__':
    main()
