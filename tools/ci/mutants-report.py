#!/usr/bin/env python3
"""Markdown report of a cargo-mutants run; exit 1 when a gate file has a missed mutant that is
not in the known list (tools/ci/mutants-known.txt: mutants that change nothing the tests could
see, such as a fast path's shortcut; "file: description", without the line and column).
    mutants-report.py MUTANTS_OUT GATE_LIST [KNOWN_LIST]"""
import os
import re
import sys


def lines(path):
    try:
        with open(path) as f:
            return [l.rstrip("\n") for l in f if l.strip()]
    except FileNotFoundError:
        return []


def unplaced(m):
    """'file:line:col: what' -> 'file: what'"""
    return re.sub(r"^([^:]+):\d+:\d+: ", r"\1: ", m)


out, gate_list = sys.argv[1], sys.argv[2]
gate = {l.strip() for l in lines(gate_list) if not l.startswith("#")}
known = {l.strip() for l in lines(sys.argv[3]) if not l.startswith("#")} if len(sys.argv) > 3 else set()
caught = lines(os.path.join(out, "caught.txt"))
missed = lines(os.path.join(out, "missed.txt"))
timeout = lines(os.path.join(out, "timeout.txt"))
unviable = lines(os.path.join(out, "unviable.txt"))
missed_known = [m for m in missed if unplaced(m) in known]
missed = [m for m in missed if unplaced(m) not in known]
gated = [m for m in missed if m.split(":", 1)[0] in gate]

print("## Mutation testing\n")
print(f"caught {len(caught)}, missed {len(missed)} (and {len(missed_known)} known), timeout {len(timeout)}, unviable {len(unviable)}\n")
for title, items in (("Missed in gate files (a test gap: fails the run)", gated),
                     ("Missed", [m for m in missed if m not in gated]),
                     ("Timeout", timeout)):
    if items:
        print(f"### {title}\n")
        for m in items:
            print(f"- `{m}`")
        print()
sys.exit(1 if gated else 0)
