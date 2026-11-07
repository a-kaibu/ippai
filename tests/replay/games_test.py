#!/usr/bin/env python3
"""games.py: a trace packed into parts (any part size, more than ten parts) unpacks to the same
bytes; a part missing, out of its numbers or changed is an error; which titles are left to
record."""
import os
import random
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


with tempfile.TemporaryDirectory() as d:
    r = random.Random(1)
    for seed, size, part in [(0, 0, 100), (1, 1, 100), (2, 5000, 100), (3, 300000, 7000), (4, 300000, 1 << 30)]:
        r = random.Random(seed)
        # (half random, half runs: zstd makes something of both)
        data = bytes(r.randrange(256) for _ in range(size // 2)) + bytes([r.randrange(4)]) * (size - size // 2)
        t = os.path.join(d, f"t{seed}.trace")
        open(t, "wb").write(data)
        pre = os.path.join(d, f"g{seed}-32")
        n = games.pack(t, pre, "d3d9", "dir/ゲーム.exe", "60", part)
        z = sum(os.path.getsize(p) for p in games.parts(pre))
        expect(f"parts {seed}", n, max(1, -(-z // part)))
        expect(f"each part {seed}", all(os.path.getsize(p) <= part for p in games.parts(pre)), True)
        out = os.path.join(d, "out")
        games.unpack(pre, out)
        expect(f"round trip {seed}", open(out, "rb").read(), data)
        expect(f"meta {seed}", (games.meta(pre)["exe"], games.meta(pre)["parts"]), ("dir/ゲーム.exe", str(n)))
        # (packed again, smaller: no old part is left behind)
        games.pack(t, pre, "d3d9", "x", "60", 1 << 30)
        expect(f"repacked {seed}", len(games.parts(pre)), 1)
    pre = os.path.join(d, "g3-32")
    games.pack(os.path.join(d, "t3.trace"), pre, "d3d9", "x", "60", 7000)
    ps = games.parts(pre)
    expect("more than ten parts", len(ps) > 10, True)
    data = open(ps[4], "rb").read()
    open(ps[4], "wb").write(bytes([data[0] ^ 1]) + data[1:])
    fails_with("a part changed", lambda: games.unpack(pre, os.path.join(d, "out")))
    open(ps[4], "wb").write(data)
    os.rename(ps[-1], ps[-1][:-3] + "999")
    fails_with("a part out of its numbers", lambda: games.unpack(pre, os.path.join(d, "out")))
    os.remove(ps[-1][:-3] + "999")
    fails_with("the last part missing", lambda: games.unpack(pre, os.path.join(d, "out")))
    # names / todo
    open(os.path.join(d, "x-64.txt"), "w").write("api d3d9\n")
    expect("names", games.names(d), ["g0-32", "g1-32", "g2-32", "g3-32", "g4-32", "x-64"])
    known = {"g0", "x", "new"}
    orig = games.listed
    games.listed = lambda: ["g0", "x", "new"]
    expect("todo: the new", games.todo([], False, d, known), ["new"])
    expect("todo: again", games.todo([], True, d, known), ["g0", "x", "new"])
    expect("todo: given", games.todo(["x"], False, d, known), ["x"])
    fails_with("todo: unknown", lambda: games.todo(["nope"], False, d, known))
    games.listed = orig
# (the real list: every id a title)
expect("games.txt", [i for i in games.listed() if i not in games.titles()], [])
# (the committed traces: each unpacks to its sha256)
for n in games.names():
    with tempfile.TemporaryDirectory() as d:
        games.unpack(os.path.join(games.GAMES, n), os.path.join(d, "t"))

if fails:
    sys.exit(1)
print("games_test: ok")
