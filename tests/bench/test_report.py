#!/usr/bin/env python3
"""report.py: a trusted run joins the history, a failed one does not (it goes to --rejected-out).

    python3 tests/bench/test_report.py"""
import json
import os
import subprocess
import sys
import tempfile

REPORT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "report.py")


def run(tmp, sha, runs, history, rounds=2, errors=None):
    res = os.path.join(tmp, sha)
    os.makedirs(res, exist_ok=True)
    json.dump({"version": 9, "rounds": rounds, "builds": ["new"], "cases": ["g/a.exe"],
               "runs": {"new": runs}, "errors": {"new": errors or {}}},
              open(os.path.join(res, "g.json"), "w"))
    out, rej = os.path.join(tmp, sha + "-h.json"), os.path.join(tmp, sha + "-r.json")
    args = [sys.executable, REPORT, res, "--sha", sha, "--groups", "g", "--history-out", out,
            "--rejected-out", rej, "--out", os.path.join(tmp, sha + ".md")]
    if history:
        args += ["--history", history]
    p = subprocess.run(args, capture_output=True, text=True)
    return p.returncode, json.load(open(out)), json.load(open(rej)) if os.path.exists(rej) else None


with tempfile.TemporaryDirectory() as tmp:
    code, h1, rej = run(tmp, "aaa", {"g/a.t": [1.0, 1.1]}, None)
    assert code == 0 and rej is None, (code, rej)
    assert [c["sha"] for c in h1["commits"]] == ["aaa"]
    hist = os.path.join(tmp, "aaa-h.json")
    # one round missing: fails, the history stays as it was
    code, h2, rej = run(tmp, "bbb", {"g/a.t": [5.0]}, hist)
    assert code == 1, code
    assert h2 == h1, h2
    assert rej["sha"] == "bbb" and rej["metrics"] == {"g/a.t": 5.0} and rej["problems"], rej
    # a case that worked before breaks: fails too
    code, h3, rej = run(tmp, "ccc", {}, hist, errors={"g/a.exe": {"error": "crashed"}})
    assert code == 1 and h3 == h1 and rej["errors"] == ["g/a.exe"], (code, rej)
    # the next good run is compared with aaa, not with bbb's numbers
    code, h4, rej = run(tmp, "ddd", {"g/a.t": [1.0, 1.0]}, hist)
    assert code == 0 and [c["sha"] for c in h4["commits"]] == ["aaa", "ddd"], h4
print("report.py: ok")
