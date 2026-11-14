#!/usr/bin/env python3
"""Which CI build of main the shirusu workflow tests on sake (.github/workflows/shirusu.yml).

The run is about one commit: the push that started it (or, nightly, the newest commit of main
with a good build when it started). sake must be that commit's build, not whatever build of
main was newest when the sake job ran (the review of 2026-11-14). main's CI of a commit is
often cancelled by the next push (ci.yml cancels in progress): then the first good build of a
later commit, which has this one in it, stands for it, and the report says so.

  pick_build.py SHA [--wait MIN]     prints "<run id> <sha of its build> <exact|later>"
  pick_build.py --newest             the newest good build of main: "<run id> <sha> exact"

Needs gh (GH_TOKEN, GITHUB_REPOSITORY) and a clone with main's history (fetch-depth 0)."""
import json, os, subprocess, sys, time


def choose(runs, sha, contains):
    """runs: CI runs of main, newest first, as gh prints them (databaseId, headSha, status,
    conclusion). contains(a, b): whether commit a has commit b in it. -> (run, 'exact' | 'later')
    when one stands for `sha`; ('wait', None) while sha's own build or a later one may still
    come; (None, reason) when none will."""
    own = [r for r in runs if r['headSha'] == sha]
    if any(r['conclusion'] == 'success' for r in own):
        return next(r for r in own if r['conclusion'] == 'success'), 'exact'
    # (later commits' good builds, oldest first: the nearest one that has sha in it)
    later = [r for r in reversed(runs) if r['headSha'] != sha and r['conclusion'] == 'success' and contains(r['headSha'], sha)]
    own_done = own and all(r['status'] == 'completed' for r in own)
    if own_done and later:
        return later[0], 'later'
    if own and not own_done:
        return 'wait', None
    # (own build failed, not cancelled: that is the answer, a later one would hide it)
    if own_done and any(r['conclusion'] == 'failure' for r in own):
        return None, f'CI of {sha[:10]} failed'
    if later:
        return later[0], 'later'
    return 'wait', None


def gh(*args):
    return subprocess.run(['gh', *args], check=True, capture_output=True, text=True).stdout


def runs_of_main(limit=60):
    out = gh('run', 'list', '--repo', os.environ['GITHUB_REPOSITORY'], '--workflow', 'ci.yml', '--branch', 'main',
             '--event', 'push', '--limit', str(limit), '--json', 'databaseId,headSha,status,conclusion')
    return json.loads(out)


def contains(a, b):
    return subprocess.run(['git', 'merge-base', '--is-ancestor', b, a], capture_output=True).returncode == 0


def main(argv):
    if argv[:1] == ['--newest']:
        r = next(r for r in runs_of_main() if r['conclusion'] == 'success')
        print(r['databaseId'], r['headSha'], 'exact')
        return 0
    sha = argv[0]
    wait = float(argv[argv.index('--wait') + 1]) if '--wait' in argv else 0
    end = time.time() + wait * 60
    while True:
        subprocess.run(['git', 'fetch', '-q', 'origin', 'main'], capture_output=True)
        run, how = choose(runs_of_main(), sha, contains)
        if run not in ('wait', None):
            print(run['databaseId'], run['headSha'], how)
            return 0
        if run is None:
            print(how, file=sys.stderr)
            return 1
        if time.time() > end:
            print(f'no build of {sha[:10]} or a later commit after {wait:.0f} min', file=sys.stderr)
            return 1
        time.sleep(120)


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
