#!/usr/bin/env python3
"""The benchmark report: this commit against the one before it, and the history.

    report.py RESULTS_DIR --sha SHA [--old-sha SHA] [--history history.json] [--out report.md]
              [--summary FILE] [--history-out history.json]

RESULTS_DIR holds the JSON files tests/bench/run.mjs and js.mjs wrote (any depth). For each metric
(ms, lower is better) the median of the rounds is compared between the builds that ran side by
side on the same runner ("new" and "old"); a change counts when it is over THRESHOLD and every
round of one build beat every round of the other. Metrics measured only on "new" (the JS
engines, or no previous build) are compared with the last commit in the history instead.

The history (the artifact the previous run left) keeps the medians of the last 300 commits.
Exits 1 when a case ran on the previous build and fails on this one (a broken benchmark is a
broken feature); slower numbers alone do not fail the run."""
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
p.add_argument("--history")
p.add_argument("--history-out")
p.add_argument("--out")
p.add_argument("--summary", action="append", default=[])
a = p.parse_args()

new, old, checks_new, checks_old, err_new, err_old = {}, {}, {}, {}, {}, {}
for f in sorted(glob.glob(os.path.join(a.results, "**", "*.json"), recursive=True)):
    try:
        r = json.load(open(f, encoding="utf-8"))
    except (OSError, ValueError):
        continue
    if not isinstance(r, dict) or "runs" not in r:
        continue
    new.update(r["runs"].get("new", {}))
    old.update(r["runs"].get("old", {}))
    checks_new.update(r.get("checks", {}).get("new", {}))
    checks_old.update(r.get("checks", {}).get("old", {}))
    err_new.update(r.get("errors", {}).get("new", {}))
    err_old.update(r.get("errors", {}).get("old", {}))

history = {"commits": []}
if a.history and os.path.exists(a.history):
    try:
        history = json.load(open(a.history, encoding="utf-8"))
    except ValueError:
        pass
prev = next((c for c in reversed(history["commits"]) if c["sha"] != a.sha), None)


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

broken = sorted(k for k in err_new if k not in err_old and old)  # (cases the old build ran)
still = sorted(k for k in err_new if k in err_old or not old)
fixed = sorted(k for k in err_old if k not in err_new)
diff_checks = sorted(k for k in checks_new if k in checks_old and checks_new[k] != checks_old[k])

out = [f"## ベンチマーク {a.sha[:7]}" + (f"（比較対象 {a.old_sha[:7]}、同じランナーで交互に計測）" if a.old_sha else "（比較対象なし: 履歴と比較）")]
faster = [r for r in rows if r[4] == "faster"]
slower = [r for r in rows if r[4] == "slower"]
out.append(f"\n{len(rows)} 指標: 🟢 {len(faster)} 速くなった、🔴 {len(slower)} 遅くなった（しきい値 {THRESHOLD:.0%}、全ラウンドが分かれたときだけ）。単位 ms、小さいほど速い。\n")
if broken:
    out.append("### ⚠️ この版で動かなくなったケース\n")
    out += [f"- `{k}`: {err_new[k]['error'][:300]}" for k in broken]
if diff_checks:
    out.append("\n### ⚠️ 計算結果（チェックサム）が前の版と違う\n")
    out += [f"- `{k}`: {checks_old[k]} → {checks_new[k]}" for k in diff_checks]
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

# the trend of the last commits for every metric
recent = [c for c in history["commits"] if c["sha"] != a.sha][-9:]
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

if a.history_out:
    history["commits"] = [c for c in history["commits"] if c["sha"] != a.sha]
    history["commits"].append({"sha": a.sha, "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                               "metrics": {m: statistics.median(v) for m, v in new.items()},
                               "errors": sorted(err_new)})
    history["commits"] = history["commits"][-KEEP:]
    json.dump(history, open(a.history_out, "w", encoding="utf-8"), indent=0)

if broken:
    print(f"{len(broken)} case(s) broke on this build: {' '.join(broken)}", file=sys.stderr)
    sys.exit(1)
