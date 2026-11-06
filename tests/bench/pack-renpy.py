#!/usr/bin/env python3
"""The Ren'Py benchmark's game (suite.mjs's "renpy-gl" and "renpy-angle" groups, played by
scenario.mjs): testdata/renpy (scripts, the movie) with the KiriKiri game's pictures and sounds
(testdata/kirikiri), compiled by the SDK's Linux runtime and packed into game/data.rpa as games
ship, next to the Windows x64 runtime: OUT_DIR/renpy/RenBench.exe.

  python3 -I tests/bench/pack-renpy.py OUT_DIR SDK_DIR CRT64_DIR
    SDK_DIR: tests/bench/fetch-renpy.sh's DIR/renpy-sdk; CRT64_DIR: fetch-crt64.sh's DIR/crt64
"""
import os, pickle, random, shutil, subprocess, sys, zlib

out, sdk, crt64 = sys.argv[1], sys.argv[2], sys.argv[3]
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "testdata")
kk = os.path.join(root, "kirikiri")
dst = os.path.join(out, "renpy")
shutil.rmtree(dst, ignore_errors=True)
game = os.path.join(dst, "game")

# the runtime as a distribution lays it out: <name>.exe + <name>.py, renpy/, lib/
for d in ("renpy", "lib/python3.12", "lib/py3-windows-x86_64"):
    shutil.copytree(os.path.join(sdk, d), os.path.join(dst, d), ignore=shutil.ignore_patterns("*.py[co]~"))
shutil.copy(os.path.join(sdk, "renpy.exe"), os.path.join(dst, "RenBench.exe"))
shutil.copy(os.path.join(sdk, "renpy.py"), os.path.join(dst, "RenBench.py"))
shutil.copy(os.path.join(sdk, "LICENSE.txt"), os.path.join(dst, "LICENSE.txt"))
# (the 64-bit C runtime, app-local)
shutil.copy(os.path.join(crt64, "ucrtbase.dll"), dst)

# the game: scripts, movie, and the KiriKiri game's pictures and sounds
shutil.copytree(os.path.join(root, "renpy", "game"), game)
os.makedirs(os.path.join(game, "images"))
for sub in ("bgimage", "fgimage", "image"):
    for f in os.listdir(os.path.join(kk, sub)):
        shutil.copy(os.path.join(kk, sub, f), os.path.join(game, "images", f))
shutil.copytree(os.path.join(kk, "rule"), os.path.join(game, "rule"))
os.makedirs(os.path.join(game, "audio"))
for sub in ("sound", "bgm"):
    for f in os.listdir(os.path.join(kk, sub)):
        shutil.copy(os.path.join(kk, sub, f), os.path.join(game, "audio", f))
shutil.copy(os.path.join(sdk, "sdk-fonts", "SourceHanSansLite.ttf"), game)

# compiled as a release is (.rpyc; the first start would otherwise compile them in the browser)
linux = os.path.join(sdk, "lib", "py3-linux-x86_64")
env = dict(os.environ, SDL_AUDIODRIVER="dummy", SDL_VIDEODRIVER="dummy")
subprocess.run([os.path.join(linux, "python"), "-EO", os.path.join(sdk, "renpy.py"), dst, "compile"],
               check=True, env=env, stdout=subprocess.DEVNULL)
shutil.rmtree(os.path.join(game, "saves"), ignore_errors=True)

# game/data.rpa (RPA-3.0: "RPA-3.0 <index offset> <key>\n", the files, the index: zlib of a
# pickle {name: [(offset ^ key, length ^ key, b"")]}), in no useful order; the scripts' sources
# stay out (as in releases)
files = []
for d, _, fs in os.walk(game):
    for f in fs:
        p = os.path.join(d, f)
        name = os.path.relpath(p, game).replace(os.sep, "/")
        if name.endswith(".rpy") or name.startswith("cache/"):
            if name.endswith(".rpy"):
                os.remove(p)
            continue
        if name.endswith(".rpyc") or name.startswith(("images/", "audio/", "rule/", "movie/")) or name.endswith(".ttf"):
            files.append((name, p))
random.Random(777).shuffle(files)
key = 0x42424242
index, parts, at = {}, [], 34
for name, p in files:
    data = open(p, "rb").read()
    index[name] = [(at ^ key, len(data) ^ key, b"")]
    parts.append(data)
    at += len(data)
    os.remove(p)
packed = zlib.compress(pickle.dumps(index, 2))
with open(os.path.join(game, "data.rpa"), "wb") as f:
    f.write(b"RPA-3.0 %016x %08x\n" % (at, key))
    for b in parts:
        f.write(b)
    f.write(packed)
for d, ds, fs in sorted(os.walk(game, topdown=False)):
    if d != game and not os.listdir(d):
        os.rmdir(d)
for f in ("log.txt", "traceback.txt"):
    if os.path.exists(os.path.join(dst, f)):
        os.remove(os.path.join(dst, f))
print(f"renpy/game/data.rpa: {len(files)} files, {at / 1048576:.1f} MiB")
