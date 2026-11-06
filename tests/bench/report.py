#!/usr/bin/env python3
"""The benchmark report: this commit against the one before it, and the history.

    report.py RESULTS_DIR --sha SHA [--old-sha SHA] [--groups "cpu-kernels gdi ... js"]
              [--history history.json] [--out report.md] [--summary FILE] [--history-out history.json]
              [--rejected-out rejected.json]

RESULTS_DIR holds the JSON files tests/bench/run.mjs and js.mjs wrote (any depth). For each metric
(ms, lower is better) the median of the rounds is compared between the builds that ran side by
side on the same runner ("new" and "old"); a change counts when it is over THRESHOLD and every
round of one build beat every round of the other. Metrics measured only on "new" (the JS
engines, or no previous build) are compared with the last commit in the history instead.

The history (the artifact the previous run left) keeps the medians and checksums of the last 300
commits; only entries of the same suite VERSION are compared with.

Exits 1 (after writing the report) when the results can't be trusted or something broke: a
case that worked before fails (a broken benchmark is a broken feature), a checksum differs from
the previous build's, a group of --groups left no results, a case has fewer numbers than its
rounds, or there are no numbers at all. Slower numbers alone do not fail the run. Such a run is
not added to the history (--history-out gets the previous history unchanged); its entry goes to
--rejected-out instead, with the reasons."""
import argparse
import glob
import json
import os
import statistics
import sys
import time

THRESHOLD = 0.03
KEEP = 300

p = argparse.ArgumentParser()
p.add_argument("results")
p.add_argument("--sha", required=True)
p.add_argument("--old-sha", default="")
p.add_argument("--groups", default="")
p.add_argument("--history")
p.add_argument("--history-out")
p.add_argument("--rejected-out")
p.add_argument("--out")
p.add_argument("--summary", action="append", default=[])
a = p.parse_args()

new, old, checks_new, checks_old, err_new, err_old = {}, {}, {}, {}, {}, {}
problems = []  # (why the results can't be trusted: each fails the run)
files, versions, env, incompatible, unstable = [], set(), {}, {}, {}
old_cases = set()  # (the cases the previous build ran next to this one)
for f in sorted(glob.glob(os.path.join(a.results, "**", "*.json"), recursive=True)):
    try:
        r = json.load(open(f, encoding="utf-8"))
    except (OSError, ValueError):
        continue
    if not isinstance(r, dict) or "runs" not in r:
        continue
    files.append(r)
    versions.add(r.get("version"))
    env.update(r.get("env", {}))
    incompatible.update(r.get("incompatible", {}))
    for b, u in r.get("unstable", {}).items():
        if b == "new":
            unstable.update(u)
    new.update(r["runs"].get("new", {}))
    old.update(r["runs"].get("old", {}))
    checks_new.update(r.get("checks", {}).get("new", {}))
    checks_old.update(r.get("checks", {}).get("old", {}))
    err_new.update(r.get("errors", {}).get("new", {}))
    err_old.update(r.get("errors", {}).get("old", {}))
    ran_old = "old" in r.get("builds", []) and "old" not in r.get("incompatible", {})
    if ran_old:
        old_cases.update(r.get("cases", []))
    # every case: an error, or every metric it made with all its rounds
    rounds = r.get("rounds", 0)
    for case in r.get("cases", []):
        if case in err_new:
            continue
        made = r.get("produced", {}).get("new", {}).get(case)
        if made is None:
            # (run.mjs: by the metric names' prefix, group/exe.)
            g, exe = case.split("/", 1)[0], case.split("/", 1)[1].split(":")[0].removesuffix(".exe")
            made = [m for m in r["runs"].get("new", {}) if m.startswith(f"{g}/{exe}.")]
        if not made:
            problems.append(f"`{case}`: 数値がない")
        for m in made:
            n = len(r["runs"].get("new", {}).get(m, []))
            if n < rounds:
                problems.append(f"`{m}`: {n}/{rounds} ラウンドしかない")
if len(versions) > 1:
    problems.append(f"結果のスイートの版がそろっていない: {sorted(map(str, versions))}")
version = next(iter(versions), None)
if not new:
    problems.append("数値が一つもない")
for g in a.groups.split():
    if not any(m.startswith(g + "/") for m in new) and not any(k.startswith(g + "/") for k in err_new):
        problems.append(f"グループ `{g}` の結果がない（ジョブが落ちたか、結果を書く前に止まった）")

history = {"commits": []}
if a.history and os.path.exists(a.history):
    try:
        history = json.load(open(a.history, encoding="utf-8"))
    except ValueError:
        pass
# (only commits measured the same way)
same = [c for c in history["commits"] if c["sha"] != a.sha and c.get("version") == version]
prev = same[-1] if same else None


def fmt(v):
    return "" if v is None else f"{v:.3g}" if v < 10 else f"{v:.1f}"


def verdict(m):
    """(old median, change, mark, source) for metric m"""
    n = new[m]
    if m in old and old[m]:
        o = old[m]
        mo, mn = statistics.median(o), statistics.median(n)
        ch = mn / mo - 1 if mo else 0
        # (one round each says nothing about the noise)
        clear = min(len(n), len(o)) >= 2 and (max(n) < min(o) or min(n) > max(o))
        mark = "faster" if ch < -THRESHOLD and clear else "slower" if ch > THRESHOLD and clear else ""
        return mo, ch, mark, "A/B"
    if prev and m in prev["metrics"]:
        mo, mn = prev["metrics"][m], statistics.median(n)
        ch = mn / mo - 1 if mo else 0
        # (different runners: only big changes mean something)
        mark = "faster" if ch < -0.15 else "slower" if ch > 0.15 else ""
        return mo, ch, mark, "history"
    return None, None, "", ""


MARK = {"faster": "🟢 速くなった", "slower": "🔴 遅くなった", "": ""}
rows, groups = [], {}
for m in sorted(new):
    mo, ch, mark, src = verdict(m)
    rows.append((m, statistics.median(new[m]), mo, ch, mark, src))
    groups.setdefault(m.split("/")[0], []).append(rows[-1])

all_cases = {c for r in files for c in r.get("cases", [])}


def had_baseline(k):
    """whether something ran case k before: the previous build next to this one, or the history"""
    return k in old_cases or (prev is not None and k in prev.get("cases", []))


def failed_before(k):
    return k in err_old if k in old_cases else k in prev.get("errors", [])


# (errors that are not one case's, like a build that won't run unpaced or engines that disagree,
# always count)
broken = sorted(k for k in err_new if k not in all_cases or (had_baseline(k) and not failed_before(k)))
still = sorted(k for k in err_new if k not in broken)
fixed = sorted(k for k in err_old if k not in err_new)
# checksums: against the previous build next to this one, else the history's last commit (but not
# the ones that differ from run to run of this build)
base_checks = dict(prev.get("checks", {})) if prev else {}
base_checks.update(checks_old)
diff_checks = sorted(k for k in checks_new if k in base_checks and checks_new[k] != base_checks[k] and k not in unstable)
if diff_checks:
    problems.append(f"チェックサムが前の版と違う: {len(diff_checks)} 件")
if broken:
    problems.append(f"前の版で動いていたケースが動かない: {len(broken)} 件")

ab = a.old_sha and old
out = [f"## ベンチマーク {a.sha[:7]}" + (f"（比較対象 {a.old_sha[:7]}、同じランナーで交互に計測）" if ab else "（前の版と並べて計測していない: 履歴と比較）")]
if problems:
    out.append("\n### ❌ この結果は信用できない、または壊れた\n")
    out += [f"- {p}" for p in problems]
    out.append("\n（この版の数値は履歴に入れていない: 次のコミットの比較対象は前の履歴のまま）")
if incompatible:
    out.append("\n### 比較から外した前の版\n")
    out += [f"- {b} ({a.old_sha[:7]}): {why}" for b, why in incompatible.items()]
if unstable:
    out.append("\n### ⚠️ 同じ版の実行ごとにチェックサムが変わる（比較していない）\n")
    out += [f"- `{k}`: {' / '.join(v)}" for k, v in sorted(unstable.items())]
faster = [r for r in rows if r[4] == "faster"]
slower = [r for r in rows if r[4] == "slower"]
out.append(f"\n{len(rows)} 指標: 🟢 {len(faster)} 速くなった、🔴 {len(slower)} 遅くなった（しきい値 {THRESHOLD:.0%}、全ラウンドが分かれたときだけ）。単位 ms、小さいほど速い。\n")
if broken:
    out.append("### ⚠️ この版で動かなくなったケース\n")
    out += [f"- `{k}`: {err_new[k]['error'][:300]}" for k in broken]
if diff_checks:
    out.append("\n### ⚠️ 計算結果・描画結果（チェックサム）が前の版と違う\n")
    out += [f"- `{k}`: {base_checks[k]} → {checks_new[k]}" for k in diff_checks]
if fixed:
    out.append("\n### 動くようになったケース\n")
    out += [f"- `{k}`" for k in fixed]
for g, rs in groups.items():
    out.append(f"\n### {g}\n")
    out.append("| 指標 | この版 | 前の版 | 変化 | |")
    out.append("|---|---:|---:|---:|---|")
    for m, mn, mo, ch, mark, src in rs:
        change = "" if ch is None else f"{ch:+.1%}" + (" (履歴)" if src == "history" else "")
        out.append(f"| `{m.split('/', 1)[1]}` | {fmt(mn)} | {fmt(mo)} | {change} | {MARK[mark]} |")
if still:
    out.append("\n### 未対応（前の版でも動かない）\n")
    out += [f"- `{k}`: {err_new[k]['error'][:200]}" for k in still]

out.append("\n<details><summary>計測条件</summary>\n")
out.append(f"- スイートの版: {version}、比較対象: {a.old_sha[:7] if ab else 'なし'}")
out += [f"- {k}: {v}" for k, v in sorted(env.items()) if v]
out.append("\n</details>")

# the trend of the last commits for every metric
recent = same[-9:]
if recent:
    out.append("\n<details><summary>最近のコミットの推移（中央値 ms）</summary>\n")
    shas = [c["sha"][:7] for c in recent] + [a.sha[:7]]
    out.append("| 指標 | " + " | ".join(shas) + " |")
    out.append("|---|" + "---:|" * len(shas))
    for m in sorted(new):
        vals = [fmt(c["metrics"].get(m)) for c in recent] + [fmt(statistics.median(new[m]))]
        out.append(f"| `{m}` | " + " | ".join(vals) + " |")
    out.append("\n</details>")

text = "\n".join(out) + "\n"
for f in ([a.out] if a.out else []) + a.summary:
    with open(f, "a" if f != a.out else "w", encoding="utf-8") as fh:
        fh.write(text)
if not a.out and not a.summary:
    sys.stdout.write(text)

entry = {"sha": a.sha, "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
         "version": version, "old_sha": a.old_sha if ab else "", "env": env,
         "metrics": {m: statistics.median(v) for m, v in new.items()},
         "checks": {k: v for k, v in checks_new.items() if k not in unstable},
         "cases": sorted(all_cases), "errors": sorted(err_new)}
if a.history_out:
    # Only a run that can be trusted joins the history: it is what the next commits compare with.
    # A failed one leaves the history as it was (written again, so the artifact carries on).
    if not problems:
        history["commits"] = [c for c in history["commits"] if c["sha"] != a.sha]
        history["commits"].append(entry)
        history["commits"] = history["commits"][-KEEP:]
    json.dump(history, open(a.history_out, "w", encoding="utf-8"), indent=0)
if a.rejected_out and problems:
    # (the failed run's numbers, to look into: never compared with)
    json.dump({**entry, "problems": problems}, open(a.rejected_out, "w", encoding="utf-8"), indent=0)

if problems:
    print("\n".join(problems), file=sys.stderr)
    sys.exit(1)
