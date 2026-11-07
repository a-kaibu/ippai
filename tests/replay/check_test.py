#!/usr/bin/env python3
"""check.py on made-up answers: the probe's pictures found in order among the replay's, one
missing or out of order, a differing pixel, answers not there yet or of older sources (waited
for, as a18dd97 learned when a stale answer failed the winref job)."""
import os
import random
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "winref"))
import check  # noqa: E402
import golden  # noqa: E402

fails = 0


def expect(what, got, want):
    global fails
    if got != want:
        print(f"check_test: {what}: got {got!r}, want {want!r}")
        fails += 1


def pic(seed, w=2, h=2):
    r = random.Random(seed)
    return (w, h, [bytes(r.randrange(256) for _ in range(3 * w)) for _ in range(h)])


# match(): in order, skipping the replay's extra pictures
A, B, C = pic(1), pic(2), pic(3)
expect("all found", check.match([("a", A), ("b", B)], [("m1", A), ("m1_alpha", C), ("m2", B)]), ([("a", "m1"), ("b", "m2")], None))
expect("out of order", check.match([("a", A), ("b", B)], [("m1", B), ("m2", A)])[1], "b")
expect("missing", check.match([("a", A), ("c", C)], [("m1", A)])[1], "c")
expect("one picture twice", check.match([("a", A), ("b", A)], [("m1", A)])[1], "b")
expect("nothing to find", check.match([], [("m1", A)]), ([], None))
# a picture one pixel off is not the probe's
w, h, rows = A
off = (w, h, [bytes([rows[0][0] ^ 1]) + rows[0][1:]] + rows[1:])
expect("one pixel off", check.match([("a", A)], [("m1", off)])[1], "a")
# (random lists: a subsequence of the replay's is always found, with its own names)
for seed in range(200):
    r = random.Random(seed)
    got = [(f"m{i}", pic(r.randrange(6))) for i in range(r.randrange(12))]
    keep = sorted(r.sample(range(len(got)), r.randrange(len(got) + 1)))
    want = [(f"p{i}", got[i][1]) for i in keep]
    pairs, missing = check.match(want, got)
    expect(f"subsequence {seed}", missing, None)
    expect(f"subsequence {seed} pictures", [got[int(g[1:])][1] for _, g in pairs], [p for _, p in want])

expect("replay name 32", check.replay_name("d3d9-32"), "replay_d3d9-32-32")
expect("replay name 64", check.replay_name("d3d11-64"), "replay_d3d11-64-64")


# check(): on golden-like directories
def answer(root, name, src, pics):
    d = os.path.join(root, name)
    os.makedirs(d)
    open(os.path.join(d, "src"), "w").write(src + "\n")
    out = []
    for n, (w, h, rows) in pics:
        golden.png_write(os.path.join(d, n + ".png"), w, h, rows)
        out.append(f"image {n} {w} {h} 2")
    open(os.path.join(d, "stdout.txt"), "w").write("\n".join(out + ["calls 1 frames 1"]) + "\n")


hashes = {"t-32": "a", "replay_t-32-32": "b"}
with tempfile.TemporaryDirectory() as root:
    expect("no answers", check.check(root, "t-32", hashes.get)[0], True)
    answer(root, "t-32", "a", [("shot", A), ("shot2", B)])
    expect("no replay answer", check.check(root, "t-32", hashes.get), (True, "t-32: no answer yet"))
    answer(root, "replay_t-32-32", "b", [("f0", B), ("m5", A), ("m5_alpha", C), ("m9", B)])
    expect("found", check.check(root, "t-32", hashes.get)[0], True)
    expect("stale replay", check.check(root, "t-32", {"t-32": "a", "replay_t-32-32": "new"}.get), (True, "t-32: waiting for Windows' answers for replay_t-32-32"))
    expect("stale probe", check.check(root, "t-32", {"t-32": "new", "replay_t-32-32": "b"}.get)[0], True)
with tempfile.TemporaryDirectory() as root:
    # (the back buffer at a Present does not stand for a read of the probe's)
    answer(root, "t-32", "a", [("shot", A), ("shot2", B)])
    answer(root, "replay_t-32-32", "b", [("m5", A), ("f0", B)])
    expect("a frame is not a read", check.check(root, "t-32", hashes.get)[0], False)
with tempfile.TemporaryDirectory() as root:
    # known.txt: a picture the recording cannot replay is left out, and one the probe does not
    # show is an error (a stale entry)
    answer(root, "t-32", "a", [("shot", A), ("lost", C), ("shot2", B)])
    answer(root, "replay_t-32-32", "b", [("m5", A), ("m9", B)])
    expect("unknown miss", check.check(root, "t-32", hashes.get)[0], False)
    expect("known miss", check.check(root, "t-32", hashes.get, {"lost"}), (True, "t-32: 2 pictures found in the replay's 2 (1 known not to be)"))
    expect("stale known", check.check(root, "t-32", hashes.get, {"lost", "gone"})[0], False)
    kt = os.path.join(root, "known.txt")
    open(kt, "w").write("# c\nt-32 lost  # why\n\nt-32 other\nu-64 x\n")
    expect("known.txt", check.known(kt), {"t-32": {"lost", "other"}, "u-64": {"x"}})
    open(kt, "w").write("t-32\n")
    try:
        check.known(kt)
        expect("bad known.txt", "read", "error")
    except ValueError:
        pass
# (the real known.txt reads, and names pictures its probes show)
for p, pics in check.known().items():
    d = os.path.join(golden.GOLDEN, p)
    if os.path.exists(d):
        expect(f"known {p}", pics - {n for n, _ in check.images(d)}, set())

if fails:
    sys.exit(1)
print("check_test: ok")
