#!/usr/bin/env python3
"""How far each trial game's recording goes, title by title:

    Captured     apitrace recorded its calls (testdata/apitrace/<id>-<bits>.txt; else
                 skipped.tsv says why)
    Convertible  the converter made a replay stream of them (build.sh's convert/<name>.txt:
                 exit 0; the calls it could not replay are listed)
    Faithful     the replay on Windows (tests/winref/golden/replay_<name>-<bits>, its answers)
                 ran through: exit 0, every call's HRESULT the recorded one, no call it could not
                 make, every picture read back, and nothing left out by the converter
    Compatible   that Faithful replay on sake (the winref run's output) draws what Windows did
                 (the probe's lines and pictures, tests/winref/known.txt's differences aside)

For a replay that is Faithful but not Compatible: the first frame sake drew differently, and
from a drill probe (drill.txt, build.sh) the first draw call after which the render target
differs, with its call number and method.

    status.py [--golden tests/winref/golden] [--sake out/jit] [--convert exe/convert]
              [--report status.md] [--tsv status.tsv] [--drill]

--drill rewrites drill.txt: each Faithful, not Compatible game gets the frames around its first
difference, narrowed run by run (a 16th of the range each time) down to one frame drawn call by
call. Exits 0 (it reports; compare.py is what fails a run).
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "tests", "winref"))
sys.path.insert(0, HERE)
import compare  # noqa: E402
import games  # noqa: E402
import golden  # noqa: E402

# a replay's lines that say it did not play the recording as it was (replay.c)
UNFAITHFUL = re.compile(r"^(hr \d+ |unknown |no object |memcpy into no lock|stream: |callv: |image \S+: )")
STAGES = ["Captured", "Convertible", "Faithful", "Compatible"]


def read(path):
    try:
        return open(path, encoding="latin-1").read()
    except FileNotFoundError:
        return None


def known_rules(path=os.path.join(ROOT, "tests", "winref", "known.txt")):
    out = []
    for l in open(path, encoding="utf-8"):
        l = l.split("  #")[0].strip()
        if l and not l.startswith("#"):
            g, rx = l.split(None, 1)
            out.append((g, re.compile(rx)))
    return out


def probe_diffs(ref_dir, got_dir, probe, known):
    """compare.py's differences of one probe that known.txt does not explain, the first
    differing frame and draw: (diffs, first f<n> name or None, first d<call> name or None), or
    None when sake has no output for it"""
    import fnmatch
    wl, wi = compare.parse(os.path.join(ref_dir, probe, "stdout.txt"))
    gl, gi = compare.parse(os.path.join(got_dir, probe, "stdout.txt"))
    if gl is None:
        return None
    is_known = lambda d: any(fnmatch.fnmatch(probe, g) and rx.search(d) for g, rx in known)
    diffs = []
    gl = compare.ulp_fix(wl, gl)
    import difflib
    for d in difflib.unified_diff(wl, gl, lineterm="", n=0):
        if not d.startswith(("---", "+++", "@@")):
            diffs.append(("want " if d[0] == "-" else "got  ") + d[1:])
    first_f = first_d = None
    for name, im in wi.items():
        d = compare.image_diff(name, im, gi.get(name), None, probe)
        if d and not is_known(d):
            diffs.append(d)
            if name[0] == "f" and first_f is None:
                first_f = name
            if name[0] == "d" and first_d is None:
                first_d = name
    return [d for d in diffs if not is_known(d)], first_f, first_d


def frames(stdout):
    """the f<n> pictures of a replay's answer, in order: [n]"""
    return [int(m) for m in re.findall(r"^image f(\d+) ", stdout or "", re.M)]


def status(golden_dir, sake_dir, convert_dir, gamedir=games.GAMES):
    """[(id, stage reached or "", detail dict)] for every title of games.txt"""
    known = known_rules()
    # (golden.expand: the answers with their pictures as compare.py reads them)
    import tempfile
    ref = tempfile.mkdtemp()
    golden.GOLDEN = os.path.abspath(golden_dir)
    golden.expand(ref)
    skipped = games.skipped(gamedir)
    traces = {n.rsplit("-", 1)[0]: n for n in games.names(gamedir)}
    drills = drill_lines()
    out = []
    for i in sorted(games.listed()):
        d = {}
        n = traces.get(i)
        if not n:
            r = skipped.get(i)
            d["why"] = f"{r[0]}: {r[1]}" if r else "not recorded yet"
            out.append((i, "", d))
            continue
        bits = n.rsplit("-", 1)[1]
        probe = f"replay_{n}-{bits}"
        stage = "Captured"
        conv = read(os.path.join(convert_dir, n + ".txt")) if convert_dir else None
        if conv is not None:
            d["dropped"] = re.findall(r"^  not replayed: (.*)$", conv, re.M)
            if re.search(r"^exit 0$", conv, re.M):
                stage = "Convertible"
            else:
                d["why"] = (conv.strip().splitlines() or ["?"])[-2 if len(conv.strip().splitlines()) > 1 else 0]
        elif os.path.isdir(os.path.join(golden_dir, probe)):
            stage = "Convertible"  # (built before: Windows answered its replay)
        ans = read(os.path.join(ref, probe, "stdout.txt"))
        if stage == "Convertible":
            if ans is None:
                d["why"] = "no Windows answer yet"
            else:
                current = (read(os.path.join(golden_dir, probe, "src")) or "").strip() == golden.src_hash(probe)
                bad = [l for l in ans.splitlines() if UNFAITHFUL.match(l)]
                ex = (read(os.path.join(ref, probe, "exit")) or "?").strip()
                d["frames"] = len(frames(ans))
                if not current:
                    d["stale"] = True
                if ex != "0" or bad or d.get("dropped"):
                    d["why"] = "; ".join(([f"exit {ex}"] if ex != "0" else []) + bad[:5] + [f"left out: {x}" for x in d.get("dropped", [])[:5]])
                else:
                    stage = "Faithful"
        if stage == "Faithful" and sake_dir:
            r = probe_diffs(ref, sake_dir, probe, known)
            if r is None:
                d["why"] = "not run on sake"
            elif not r[0]:
                stage = "Compatible"
            else:
                d["why"] = f"{len(r[0])} differences on sake"
                d["first_frame"] = r[1]
                fs = frames(ans)
                if r[1]:
                    k = int(r[1][1:])
                    before = [x for x in fs if x < k]
                    d["range"] = (before[-1] if before else 0, k)
                # (the drill probe of this game, if it ran)
                for g, frm, to, every in drills:
                    if g != n:
                        continue
                    dp = f"replay_{n}-f{frm}-{bits}"
                    dr = probe_diffs(ref, sake_dir, dp, known) if os.path.isdir(os.path.join(ref, dp)) else None
                    dans = read(os.path.join(ref, dp, "stdout.txt"))
                    if dr is None or dans is None:
                        d["drill"] = f"{dp}: not answered yet"
                        continue
                    d["drill_probe"] = (frm, to, every)
                    if dr[2]:
                        call = dr[2][1:]
                        m = re.search(rf"^draw {call} (\S+)$", dans, re.M)
                        d["first_draw"] = (int(call), m.group(1) if m else "?")
                    elif dr[1]:
                        k = int(dr[1][1:])
                        before = [x for x in frames(dans) if x < k]
                        d["range"] = (before[-1] if before else frm, k)
                    elif every == 0:
                        d["first_draw"] = (None, "the frame differs, no draw in it does")
        out.append((i, stage, d))
    return out


def drill_lines(path=os.path.join(HERE, "drill.txt")):
    """[(game, from, to, every)]: every 0 means the draws of one frame"""
    out = []
    for l in (read(path) or "").splitlines():
        p = l.split("#")[0].split()
        if len(p) >= 3:
            out.append((p[0], int(p[1]), int(p[2]), int(p[3]) if len(p) > 3 else 0))
    return out


def next_drill(rows):
    """drill.txt's lines for the next run: [(game, from, to, every)]"""
    out = []
    for i, stage, d in rows:
        if stage != "Faithful" or "range" not in d or "first_draw" in d:
            # (keep a finished drill: its answer is the report)
            if "drill_probe" in d and "first_draw" in d:
                out.append((d.get("name") or "",) + d["drill_probe"])
            continue
        a, b = d["range"]
        n = d["name"]
        if b - a <= 1:
            out.append((n, b, b, 0))
        else:
            out.append((n, a, b, max(1, (b - a) // 16)))
    return [x for x in out if x[0]]


def render(rows):
    count = {s: 0 for s in STAGES}
    for _, s, _ in rows:
        if s:
            for t in STAGES[:STAGES.index(s) + 1]:
                count[t] += 1
    lines = [f"apitrace replays of {len(rows)} trial games: " + ", ".join(f"{count[s]} {s}" for s in STAGES), "",
             "| title | reached | frames | why not further / first difference |", "|---|---|---|---|"]
    for i, s, d in rows:
        why = d.get("why", "")
        if d.get("stale"):
            why += "; (Windows' answer is older than the replay's sources)"
        if d.get("first_frame"):
            why += f"; first frame f{d['first_frame'][1:]}"
        if d.get("first_draw"):
            c, m = d["first_draw"]
            why += f"; first draw: call {c} {m}" if c is not None else f"; {m}"
        elif d.get("drill"):
            why += f"; {d['drill']}"
        lines.append(f"| {i} | {s or '-'} | {d.get('frames', '')} | {why.strip('; ').replace('|', '/')} |")
    return "\n".join(lines) + "\n"


def tsv(rows):
    out = ["# id\tstage\twhy (tests/replay/status.py)"]
    for i, s, d in rows:
        out.append(f"{i}\t{s or '-'}\t{d.get('why', '')}")
    return "\n".join(out) + "\n"


def main():
    a = sys.argv[1:]
    opt = {"--golden": os.path.join(ROOT, "tests", "winref", "golden"), "--sake": None, "--convert": None, "--report": None, "--tsv": None}
    drill = "--drill" in a
    a = [x for x in a if x != "--drill"]
    for k in list(opt):
        if k in a:
            j = a.index(k)
            opt[k] = a[j + 1]
            del a[j:j + 2]
    if a:
        sys.exit(__doc__)
    rows = status(opt["--golden"], opt["--sake"], opt["--convert"])
    names = {n.rsplit("-", 1)[0]: n for n in games.names()}
    for i, _, d in rows:
        d["name"] = names.get(i, "")
    text = render(rows)
    print(text)
    if opt["--report"]:
        open(opt["--report"], "w", encoding="utf-8").write(text)
    if opt["--tsv"]:
        open(opt["--tsv"], "w", encoding="utf-8").write(tsv(rows))
    if drill:
        head = "".join(l + "\n" for l in (read(os.path.join(HERE, "drill.txt")) or "").splitlines() if l.startswith("#"))
        body = "".join(f"{g} {f} {t} {e}\n" for g, f, t, e in next_drill(rows))
        open(os.path.join(HERE, "drill.txt"), "w").write(head + body)


if __name__ == "__main__":
    main()
