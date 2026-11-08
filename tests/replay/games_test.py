#!/usr/bin/env python3
"""games.py: a trace pushed to a registry (fakeregistry.py, as ghcr.io answers) pulls back to the
same bytes, anonymously when public, from the cache the second time; a changed blob, cache or
.txt is an error or pulled again; a push needs the secret; traces in parts (the old way)
migrate; which titles are left to record."""
import hashlib
import json
import os
import random
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import games  # noqa: E402

fails = 0


def expect(what, got, want):
    global fails
    if got != want:
        print(f"games_test: {what}: got {got!r}, want {want!r}")
        fails += 1


def fails_with(what, f):
    try:
        f()
        expect(what, "no error", "an error")
    except SystemExit:
        pass


# (the registry: a fake one as ghcr.io answers, on this machine)
os.environ["no_proxy"] = os.environ["NO_PROXY"] = "127.0.0.1,localhost"
import fakeregistry  # noqa: E402

fake = fakeregistry.Fake()
with tempfile.TemporaryDirectory() as d:
    os.environ["TRACES_CACHE"] = os.path.join(d, "cache")
    reg = games.Registry(fake.ref, token=fake.secret)
    for seed, size in [(0, 0), (1, 1), (2, 5000), (3, 300000)]:
        r = random.Random(seed)
        # (half random, half runs: zstd makes something of both)
        data = bytes(r.randrange(256) for _ in range(size // 2)) + bytes([r.randrange(4)]) * (size - size // 2)
        t = os.path.join(d, f"t{seed}.trace")
        open(t, "wb").write(data)
        pre = os.path.join(d, f"g{seed}-32")
        z = games.push(t, pre, "d3d9", "dir/ゲーム.exe", "60", fake.ref, reg)
        m = games.meta(pre)
        expect(f"meta {seed}", (m["exe"], m["ref"], m["zst"], m["zst"] in fake.blobs), ("dir/ゲーム.exe", fake.ref + f":g{seed}-32", z, True))
        mf = json.loads(fake.manifests[f"g{seed}-32"])
        expect(f"manifest {seed}", ([x["digest"] for x in mf["layers"]], mf["annotations"]["org.opencontainers.image.source"]), ([z], games.SOURCE))
        out = os.path.join(d, "out")
        # (pulled anonymously: the package is public)
        g0 = fake.gets
        games.unpack(pre, out, games.Registry(fake.ref, token=""))
        expect(f"round trip {seed}", open(out, "rb").read(), data)
        expect(f"pulled {seed}", fake.gets - g0, 1)
        # (the second time from the cache)
        games.unpack(pre, out, games.Registry(fake.ref, token=""))
        expect(f"cached {seed}", (fake.gets - g0, open(out, "rb").read() == data), (1, True))
    expect("no Authorization header follows the redirect", fake.leaked, 0)
    # (the same trace again: its blobs are there, none uploaded)
    p0 = fake.puts
    games.push(os.path.join(d, "t3.trace"), os.path.join(d, "g3-32"), "d3d9", "x", "60", fake.ref, reg)
    expect("pushed again: no upload", fake.puts, p0)
    pre = os.path.join(d, "g3-32")
    z = games.meta(pre)["zst"]
    c = os.path.join(d, "cache", z.replace(":", "-") + ".zst")
    # (a cached blob changed: pulled again)
    open(c, "wb").write(b"x")
    games.unpack(pre, os.path.join(d, "out"), games.Registry(fake.ref, token=""))
    expect("a changed cache pulled again", open(c, "rb").read() == fake.blobs[z], True)
    # (the registry's blob changed: an error, nothing cached)
    good = fake.blobs[z]
    fake.blobs[z] = good[:-1] + bytes([good[-1] ^ 1])
    os.remove(c)
    fails_with("the registry's blob changed", lambda: games.unpack(pre, os.path.join(d, "out"), games.Registry(fake.ref, token="")))
    expect("a bad blob is not cached", os.path.exists(c), False)
    fake.blobs[z] = good
    # (the .txt's sha256 not the trace's: an error)
    txt = open(pre + ".txt", encoding="utf-8").read()
    open(pre + ".txt", "w", encoding="utf-8").write(re.sub(r"sha256 [0-9a-f]+", "sha256 " + "0" * 64, txt))
    fails_with("the trace not the recorded one", lambda: games.unpack(pre, os.path.join(d, "out")))
    open(pre + ".txt", "w", encoding="utf-8").write(txt)
    # (a push without the secret is refused; so is a private package's pull without it)
    fails_with("push without the secret", lambda: games.push(os.path.join(d, "t2.trace"), os.path.join(d, "n-32"), "d3d9", "x", "60", fake.ref, games.Registry(fake.ref, token="")))
    fake.public = False
    os.remove(c)
    fails_with("a private package, anonymous", lambda: games.unpack(pre, os.path.join(d, "out"), games.Registry(fake.ref, token="")))
    games.unpack(pre, os.path.join(d, "out"), games.Registry(fake.ref, token=fake.secret))
    expect("a private package with the secret", open(os.path.join(d, "out"), "rb").read(), open(os.path.join(d, "t3.trace"), "rb").read())
    fake.public = True
    # (the old way, parts in the repository: migrated, parts gone, the same trace)
    o = os.path.join(d, "old")
    os.makedirs(o)
    data = open(os.path.join(d, "t3.trace"), "rb").read()
    zz = subprocess.run(["zstd", "-q", "-c"], input=data, stdout=subprocess.PIPE, check=True).stdout
    for k in range(3):
        open(os.path.join(o, f"m-32.trace.zst.{k:03d}"), "wb").write(zz[k * len(zz) // 3:(k + 1) * len(zz) // 3])
    open(os.path.join(o, "m-32.txt"), "w", encoding="utf-8").write(f"api d3d8\nexe a/b.exe\nseconds 30\nbytes {len(data)}\nsha256 {hashlib.sha256(data).hexdigest()}\nparts 3\n")
    games.unpack(os.path.join(o, "m-32"), os.path.join(d, "out"))
    expect("old way: unpacks until migrated", open(os.path.join(d, "out"), "rb").read(), data)
    open(os.path.join(o, "m-32.trace.zst.001"), "ab").write(b"!")
    fails_with("old way: a changed part is not migrated", lambda: games.migrate(o, fake.ref, reg))
    fails_with("old way: a changed part does not unpack", lambda: games.unpack(os.path.join(o, "m-32"), os.path.join(d, "out")))
    os.rename(os.path.join(o, "m-32.trace.zst.002"), os.path.join(o, "m-32.trace.zst.003"))
    fails_with("old way: a part out of its numbers", lambda: games.unpack(os.path.join(o, "m-32"), os.path.join(d, "out")))
    os.rename(os.path.join(o, "m-32.trace.zst.003"), os.path.join(o, "m-32.trace.zst.002"))
    open(os.path.join(o, "m-32.trace.zst.001"), "wb").write(zz[len(zz) // 3:2 * len(zz) // 3])
    expect("migrated", games.migrate(o, fake.ref, reg), ["m-32"])
    expect("migrated: parts gone", sorted(os.listdir(o)), ["m-32.txt"])
    expect("migrated: what was recorded kept", {k: v for k, v in games.meta(os.path.join(o, "m-32")).items() if k in ("api", "exe", "seconds")}, {"api": "d3d8", "exe": "a/b.exe", "seconds": "30"})
    games.unpack(os.path.join(o, "m-32"), os.path.join(d, "out"), games.Registry(fake.ref, token=""))
    expect("migrated: the same trace", open(os.path.join(d, "out"), "rb").read(), data)
    expect("migrated again: nothing", games.migrate(o, fake.ref, reg), [])
    # names / todo
    open(os.path.join(d, "x-64.txt"), "w").write("api d3d9\n")
    expect("names", games.names(d), ["g0-32", "g1-32", "g2-32", "g3-32", "x-64"])
    known = {"g0", "x", "new"}
    orig = games.listed
    games.listed = lambda: ["g0", "x", "new"]
    expect("todo: the new", games.todo([], False, d, known), ["new"])
    expect("todo: again", games.todo([], True, d, known), ["g0", "x", "new"])
    expect("todo: given", games.todo(["x"], False, d, known), ["x"])
    fails_with("todo: unknown", lambda: games.todo(["nope"], False, d, known))
    expect("todo: at most", games.todo([], True, d, known, most=2), ["g0", "x"])
    expect("todo: at most, more than there are", games.todo([], False, d, known, most=5), ["new"])
    # skipped.tsv: a skipped title is not tried again unless asked; one that took leaves it
    known = {"g0", "x", "new", "dd", "bad"}
    games.listed = lambda: ["g0", "x", "new", "dd", "bad"]
    games.skip("dd", "none", "a.exe:\tddraw  only", d)
    games.skip("bad", "notrace", "", d)
    games.skip("dw", "ddraw", "tried d3d8", d)
    expect("skipped", games.skipped(d), {"dd": ("none", "a.exe: ddraw only"), "bad": ("notrace", ""), "dw": ("ddraw", "tried d3d8")})
    expect("todo: skipped left out", games.todo([], False, d, known), ["new"])
    expect("todo: skipped again", games.todo([], True, d, known), ["g0", "x", "new", "dd", "bad"])
    expect("todo: skipped given", games.todo(["bad"], False, d, known), ["bad"])
    games.skip("bad", "noconvert", "", d)
    expect("skip: a new reason replaces", games.skipped(d)["bad"], ("noconvert", ""))
    games.took("bad", d)
    games.took("never", d)
    expect("took", sorted(games.skipped(d)), ["dd", "dw"])
    expect("todo: took", games.todo([], False, d, known), ["new", "bad"])
    fails_with("skip: unknown reason", lambda: games.skip("x", "lazy", "", d))
    open(os.path.join(d, "skipped.tsv"), "a").write("y\tlazy\n")
    fails_with("skipped.tsv: unknown reason", lambda: games.skipped(d))
    expect("names: skipped.tsv is no trace", "skipped" in " ".join(games.names(d)), False)
    games.listed = orig
# (the real list: no id twice; at most 256 a run: gametrace.yml's --max)
ids = games.listed()
expect("games.txt: no id twice", sorted({i for i in ids if ids.count(i) > 1}), [])
import re
yml_max = re.search(r"games.py todo --max=(\d+)", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".github/workflows/gametrace.yml"), encoding="utf-8").read())
expect("gametrace.yml: todo --max under GitHub's 256 matrix jobs", bool(yml_max) and 0 < int(yml_max.group(1)) <= 256, True)
# (the real list: every id a title; the real skipped.tsv reads)
games.skipped()
expect("games.txt", [i for i in games.listed() if i not in games.titles()], [])
# (the committed traces: each says where it is, on ghcr.io, or is still in parts to migrate;
# pulling them is build.sh's, in winref)
for n in games.names():
    m = games.meta(os.path.join(games.GAMES, n))
    if "zst" in m:
        expect(f"{n}: where it is", (m["ref"] == f"{games.REF}:{n}", bool(re.fullmatch(r"sha256:[0-9a-f]{64}", m["zst"])), m["zbytes"].isdigit()), (True, True, True))
    else:
        expect(f"{n}: its parts", len(games.old_parts(os.path.join(games.GAMES, n))), int(m["parts"]))
expect("no part without its .txt", sorted({re.sub(r"\.trace\.zst\.\d+$", "", f) for f in os.listdir(games.GAMES) if ".trace.zst." in f} - set(games.names())), [])
# (gametrace.yml calls record-game.ps1 with the parameters it declares, and record-game.ps1
# calls winplay.py's `play` with its usage: no Windows runner needed to find a renamed one)
import re
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
ps1 = open(os.path.join(root, "tests/replay/record-game.ps1"), encoding="utf-8").read()
declared = set(re.findall(r"\[(?:string|int)\] \$(\w+)", ps1.split("$ErrorActionPreference")[0]))
mandatory = set(re.findall(r"\[Parameter\(Mandatory\)\] \[\w+\] \$(\w+)", ps1))
yml = open(os.path.join(root, ".github/workflows/gametrace.yml"), encoding="utf-8").read()
call = [l for l in yml.splitlines() if "record-game.ps1 -" in l]
expect("gametrace.yml: one record-game.ps1 call", len(call), 1)
given = set(re.findall(r" -(\w+) ", call[0] + " ")) if call else set()
expect("record-game.ps1: the parameters given are declared", sorted(given - declared), [])
expect("record-game.ps1: the mandatory parameters are given", sorted(mandatory - given), [])
winplay = open(os.path.join(root, "tests/corpus/winplay.py"), encoding="utf-8").read()
expect("winplay.py: play GAME OUT", "a[:1] == ['play'] and len(a) == 3" in winplay and "python $winplay play $Game $Shots" in ps1 and "$winplay = Join-Path $PSScriptRoot '..\\corpus\\winplay.py'" in ps1 and "python $gameapi next $log" in ps1, True)
gapi = open(os.path.join(root, "tests/replay/gameapi.py"), encoding="utf-8").read()
expect("gameapi.py: next", 'sys.argv[1:2] == ["next"]' in gapi, True)
# (record-game.ps1's .why reasons are skipped.tsv's)
why = re.findall(r'\$why = if .*\{ "(\w+)" \} else \{ "(\w+)" \}', ps1)
expect("record-game.ps1: its reasons are skipped.tsv's", [r for r in (why[0] if why else ("?",)) if r not in games.REASONS], [])
# (the workflows call games.py's commands as they are; a workflow that builds the replays, which
# pulls the games' traces, may read packages and gives build.sh the token; one that pushes may write)
cmds = set(re.findall(r'add_parser\("(\w+)"\)', open(os.path.join(HERE, "games.py"), encoding="utf-8").read()))
for w in ("gametrace.yml", "winref.yml", "replay.yml"):
    y = open(os.path.join(root, ".github/workflows", w), encoding="utf-8").read()
    expect(f"{w}: games.py's commands", sorted(set(re.findall(r"games\.py (\w+)", y)) - cmds), [])
    for job in re.split(r"\n  (?=[\w-]+:\n)", y.split("\njobs:\n", 1)[1]):
        if "tests/winref/build.sh" in job:
            expect(f"{w}: build.sh gets GHCR_TOKEN", "GHCR_TOKEN: ${{ github.token }}" in job, True)
            expect(f"{w}: packages: read", "packages: read" in y, True)
        if re.search(r"games\.py (push|migrate)", job):
            expect(f"{w}: {job.split(':')[0]}: packages: write and GHCR_TOKEN", ("packages: write" in job, "GHCR_TOKEN: ${{ github.token }}" in job), (True, True))
# (a game's stream goes into its replay exe at most GAME_MAX bytes: apitrace -max, under the 2 GB
# a PE image and a 32-bit process hold; b2020_akabeesoft3's whole stream crashed the linker)
bsh = open(os.path.join(root, "tests/replay/build.sh"), encoding="utf-8").read()
gmax = re.search(r'"\$APITRACE" "-\$bits" "-max=\$\{GAME_MAX:-(\d+)\}" "\$OUT/\$n.trace"', bsh)
expect("build.sh: games' streams through apitrace -max, under 2 GB", bool(gmax) and 0 < int(gmax.group(1)) < 1 << 31, True)
expect("apitrace reads -max", "-max=" in open(os.path.join(root, "crates/apitrace/src/main.rs"), encoding="utf-8").read(), True)
expect("record-game.ps1 asks winplay to close the game first", "$env:CLOSE_FIRST = '" in ps1 and "os.environ.get('CLOSE_FIRST'" in winplay, True)
expect("prep writes winplay's meta.json", "winplay.py meta tests/corpus/titles.tsv \"$ID\" \"$e\" game/meta.json" in yml, True)

# (winref's answers for a game's replay are by its trace (golden.py's src_hash), not where it is
# kept: the same after a migrate, another for another trace)
sys.path.insert(0, os.path.join(HERE, "..", "winref"))
import golden  # noqa: E402

with tempfile.TemporaryDirectory() as d:
    root, golden.ROOT = golden.ROOT, d
    os.makedirs(os.path.join(d, "testdata", "apitrace"))
    t = os.path.join(d, "testdata", "apitrace", "x-32.txt")
    old = "api d3d9\nexe a.exe\nseconds 30\nbytes 5\nsha256 " + "1" * 64 + "\nparts 1\n"
    open(t, "w").write(old)
    h0 = golden.src_hash("replay_x-32-32")
    open(t, "w").write(old.replace("parts 1\n", "ref ghcr.io/a/b:x-32\nzst sha256:" + "2" * 64 + "\nzbytes 9\n"))
    expect("golden: the same trace moved", golden.src_hash("replay_x-32-32"), h0)
    open(t, "w").write(old.replace("1" * 64, "3" * 64))
    expect("golden: another trace", golden.src_hash("replay_x-32-32") != h0, True)
    golden.ROOT = root

if fails:
    sys.exit(1)
print("games_test: ok")
