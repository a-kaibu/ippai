#!/usr/bin/env python3
"""Runs Wine's conformance tests (built by tests/winetest/build.sh) on sake's node build and
compares the results with tests/winetest/expected-<arch>.tsv.

Every test program is asked for its tests (--list) and each test runs as its own process, in a
fresh C:\\ (SAKE_C), with a time limit. A test's result is one of
  ok        it finished with no failures
  fail      it finished with N failures (N is kept)
  crash     it ended without its summary line (unhandled exception, abort, sake exit)
  timeout   it ran out of time
The tests expect Windows' behaviour: sake is not Wine (no wine_server_call), so todo_wine
marks do not apply.

Functions sake knows but lacks are found by GetProcAddress (SAKE_PROC_STUBS: their stubs return
0), as on Windows, so a test that does not check for NULL fails instead of crashing. Other
processes are not started (SAKE_NO_EXEC): the tests that need a child fail.

A run fails (exit 1) on a regression against the expected file: a test that got worse
(ok -> fail -> crash/timeout) or one with clearly more failures than recorded (over 10%, 5 at
least). Better results are
listed; --update writes the new results as the expected ones.

  tests/winetest/run.py [--arch i386] [--update] [-j N] [--shard i/n] [dll[:test] ...]
  SAKE=target/sake-out/sake.js WINETEST=target/winetest

Also written: $WINETEST/report-<arch>.md (per-dll totals, the most-called unimplemented
functions, the first failure messages of each test) and results-<arch>.tsv.
"""
import argparse
import collections
import concurrent.futures
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
RANK = {"ok": 0, "fail": 1, "crash": 2, "timeout": 2}
SUMMARY = re.compile(r"^(?:[0-9a-f]{4}:)?(\S+): (\d+) tests executed \((\d+) marked as todo, (?:(\d+) as flaky, )?(\d+) failures?\), (\d+) skipped\.")
FAILED = re.compile(r"^(?:[0-9a-f]{4}:)?\S+\.c:\d+:(?:\d+\.\d+ )?(?: Test failed:|Test failed:| unhandled exception)")
CRASH = re.compile(r"panicked|RuntimeError|Aborted|unhandled|sake: .*(exception|fault|unknown|not supported)")
UNIMPL = re.compile(r"^sake: unimplemented (\S+?) ")


def list_tests(sake, exe, env):
    p = subprocess.run(["node", sake, exe, "--list"], capture_output=True, text=True, errors="replace", env=env, timeout=120)
    out = p.stdout + p.stderr
    names = [l.strip() for l in out.split("Valid test names:", 1)[-1].splitlines() if l.startswith("    ")]
    return names


def run_one(sake, exe, test, timeout, fonts):
    c = tempfile.mkdtemp(prefix="winetest-c-")
    for d in ("windows/temp", "windows/system32", "users/user/desktop"):
        os.makedirs(os.path.join(c, d), exist_ok=True)
    if fonts:
        os.symlink(fonts, os.path.join(c, "windows/Fonts"))
    env = dict(os.environ, SAKE_C=c, SAKE_NO_EXEC="1", SAKE_PROC_STUBS="1")
    t0 = time.time()
    try:
        p = subprocess.run(["node", sake, exe, test], capture_output=True, text=True, errors="replace", env=env, timeout=timeout, cwd=c)
        out, timed_out = p.stdout + p.stderr, False
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"").decode(errors="replace") + (e.stderr or b"").decode(errors="replace")
        timed_out = True
    finally:
        shutil.rmtree(c, ignore_errors=True)
    return parse(test, out, timed_out, time.time() - t0)


def parse(test, out, timed_out, secs):
    lines = out.splitlines()
    executed = failures = 0
    summary = False
    for l in lines:
        m = SUMMARY.match(l)
        if m:
            summary = True
            executed += int(m.group(2))
            failures += int(m.group(5))
    failed = [l for l in lines if FAILED.match(l)]
    if not summary:
        failures = len(failed)  # (the ones printed before it ended)
    status = "timeout" if timed_out else ("crash" if not summary else ("fail" if failures else "ok"))
    msgs = failed[:5]
    if status == "crash":
        msgs += [l for l in lines if CRASH.search(l)][:3]
    unimpl = collections.Counter(m.group(1) for m in map(UNIMPL.match, lines) if m)
    return dict(test=test, status=status, executed=executed, failures=failures, secs=secs, msgs=msgs, unimpl=unimpl)


def read_tsv(path):
    res = {}
    if os.path.exists(path):
        for l in open(path):
            if l.startswith("#") or not l.strip():
                continue
            k, status, failures = l.rstrip("\n").split("\t")[:3]
            res[k] = (status, int(failures))
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", default="i386")
    ap.add_argument("--update", action="store_true")
    ap.add_argument("-j", type=int, default=os.cpu_count())
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--fonts", default=os.environ.get("WINETEST_FONTS"))
    ap.add_argument("--shard", default="0/1", help="i/n: every n-th test from the i-th (CI matrix)")
    ap.add_argument("--browser", action="store_true", help="run in headless Chromium (run-browser.mjs): the GPU dlls of browser.txt")
    ap.add_argument("only", nargs="*")
    a = ap.parse_args()
    sake = os.path.abspath(os.environ.get("SAKE", os.path.join(ROOT, "target/sake-out/sake.js")))
    w = os.path.abspath(os.environ.get("WINETEST", os.path.join(ROOT, "target/winetest")))
    bindir = os.path.join(w, "bin", a.arch)
    dlls = [l.split()[0] for l in open(os.path.join(HERE, "dlls.txt")) if l.strip() and not l.startswith("#")]
    gpu = [l.split()[0] for l in open(os.path.join(HERE, "browser.txt")) if l.strip() and not l.startswith("#")]
    dlls = [d for d in dlls if (d in gpu) == a.browser]
    suffix = "-browser" if a.browser else ""
    sel = collections.defaultdict(set)
    for o in a.only:
        d, _, t = o.partition(":")
        sel[d].add(t)
    if sel:
        dlls = [d for d in dlls if d in sel] + [d for d in sel if d not in dlls]

    jobs = []
    env = dict(os.environ, SAKE_C=tempfile.mkdtemp(prefix="winetest-c-"), SAKE_NO_EXEC="1")
    for d in dlls:
        exe = os.path.join(bindir, d.split(".")[0] + "_test.exe")
        if not os.path.exists(exe):
            print(f"missing {exe}", file=sys.stderr)
            continue
        tests = list_tests(sake, exe, env)
        if not tests:
            print(f"{d}: --list gave no tests", file=sys.stderr)
            jobs.append((d, exe, "--list"))
        for t in tests:
            if not sel.get(d) or "" in sel[d] or t in sel[d]:
                jobs.append((d, exe, t))
    shutil.rmtree(env["SAKE_C"], ignore_errors=True)
    si, sn = map(int, a.shard.split("/"))
    jobs = jobs[si::sn]

    results = {}
    if a.browser:
        out = tempfile.mkdtemp(prefix="winetest-browser-")
        jf = os.path.join(out, "jobs")
        with open(jf, "w") as f:
            f.writelines(f"{d} {os.path.basename(exe)} {t}\n" for d, exe, t in jobs)
        subprocess.run(["node", os.path.join(HERE, "run-browser.mjs"), bindir, jf, out, str(a.timeout), str(a.j)], check=True)
        for d, exe, t in jobs:
            p = os.path.join(out, f"{d}:{t}.txt")
            text = open(p, errors="replace").read() if os.path.exists(p) else "timeout\n"
            r = parse(t, text, text.rstrip().endswith("timeout"), 0)
            results[f"{d}:{t}"] = r
            print(f"{r['status']:7} {d}:{t} {r['executed']} tests, {r['failures']} failures", flush=True)
        shutil.rmtree(out, ignore_errors=True)
    with concurrent.futures.ThreadPoolExecutor(a.j) as pool:
        futs = {pool.submit(run_one, sake, exe, t, a.timeout, a.fonts): (d, t) for d, exe, t in ([] if a.browser else jobs)}
        for f in concurrent.futures.as_completed(futs):
            d, t = futs[f]
            r = f.result()
            results[f"{d}:{t}"] = r
            print(f"{r['status']:7} {d}:{t} {r['executed']} tests, {r['failures']} failures ({r['secs']:.0f}s)", flush=True)

    exp_path = os.path.join(HERE, f"expected-{a.arch}{suffix}.tsv")
    expected = read_tsv(exp_path)
    worse, better = [], []
    for k in sorted(results):
        r = results[k]
        if k not in expected:
            continue
        es, ef = expected[k]
        # (failure counts of some tests move a little from run to run: timing, threads)
        if RANK[r["status"]] > RANK[es] or (r["status"] == es == "fail" and r["failures"] > ef + max(5, ef // 10)):
            worse.append(f"{k}: {es} ({ef}) -> {r['status']} ({r['failures']})")
        elif RANK[r["status"]] < RANK[es] or r["failures"] < ef:
            better.append(f"{k}: {es} ({ef}) -> {r['status']} ({r['failures']})")
    new = sorted(k for k in results if k not in expected)

    def tsv(path):
        with open(path, "w") as f:
            f.write(f"# tests/winetest/run.py --arch {a.arch}: <dll>:<test> status failures executed (Wine 11.20 tests on sake)\n")
            keep = {k: v for k, v in expected.items() if k not in results} if path == exp_path else {}
            rows = {k: (r["status"], r["failures"], r["executed"]) for k, r in results.items()}
            rows.update({k: (s, n, "") for k, (s, n) in keep.items()})
            for k in sorted(rows):
                s, n, e = rows[k]
                f.write(f"{k}\t{s}\t{n}\t{e}\n")

    tsv(os.path.join(w, f"results-{a.arch}{suffix}.tsv"))
    report(os.path.join(w, f"report-{a.arch}{suffix}.md"), a.arch, results, worse, better)
    if a.update:
        tsv(exp_path)
    counts = collections.Counter(r["status"] for r in results.values())
    print(f"\n{len(results)} tests: " + ", ".join(f"{counts[s]} {s}" for s in RANK))
    for l in better:
        print(f"better  {l}")
    for k in new:
        print(f"new     {k}: {results[k]['status']} ({results[k]['failures']})")
    for l in worse:
        print(f"WORSE   {l}")
    if worse and not a.update:
        print(f"\n{len(worse)} regressions against {os.path.relpath(exp_path, ROOT)}")
        return 1
    return 0


def report(path, arch, results, worse, better):
    by_dll = collections.defaultdict(list)
    for k, r in results.items():
        by_dll[k.split(":")[0]].append(r)
    unimpl = collections.Counter()
    unimpl_tests = collections.Counter()
    for r in results.values():
        unimpl.update(r["unimpl"])
        unimpl_tests.update(r["unimpl"].keys())
    with open(path, "w") as f:
        f.write(f"# Wine 11.20 conformance tests on sake ({arch})\n\n")
        f.write("| dll | tests | ok | fail | crash | timeout | checks run | failed checks |\n|---|---|---|---|---|---|---|---|\n")
        tot = collections.Counter()
        for d in sorted(by_dll):
            rs = by_dll[d]
            c = collections.Counter(r["status"] for r in rs)
            ex, fa = sum(r["executed"] for r in rs), sum(r["failures"] for r in rs)
            tot.update(c)
            tot["executed"] += ex
            tot["failures"] += fa
            tot["tests"] += len(rs)
            f.write(f"| {d} | {len(rs)} | {c['ok']} | {c['fail']} | {c['crash']} | {c['timeout']} | {ex} | {fa} |\n")
        f.write(f"| **all** | {tot['tests']} | {tot['ok']} | {tot['fail']} | {tot['crash']} | {tot['timeout']} | {tot['executed']} | {tot['failures']} |\n\n")
        if worse or better:
            f.write("## Changes against the expected results\n\n" + "".join(f"- worse: {l}\n" for l in worse) + "".join(f"- better: {l}\n" for l in better) + "\n")
        f.write("## Unimplemented functions, by the number of tests that called them\n\n")
        for fn, n in unimpl_tests.most_common(150):
            f.write(f"- {fn}: {n} tests ({unimpl[fn]} calls)\n")
        f.write("\n## First messages of each test that did not pass\n\n")
        for k in sorted(results):
            r = results[k]
            if r["status"] != "ok":
                f.write(f"### {k}: {r['status']} ({r['failures']} of {r['executed']})\n\n```\n" + "\n".join(m[:300] for m in r["msgs"]) + "\n```\n\n")


if __name__ == "__main__":
    sys.exit(main())
