#!/usr/bin/env python3
"""Shrinks cargo target directories before actions/cache saves them: what a fresh checkout
rebuilds anyway goes (incremental state, the repository's own crates: their sources get new
mtimes on checkout, so cargo always rebuilds them), the registry crates' builds stay.

    tools/ci/cache-prune.py DIR...   (each a target directory, e.g. target, fuzz/target)
"""
import os
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def own_crates():
    names = set()
    for f in subprocess.run(["git", "-C", ROOT, "ls-files", "*Cargo.toml"], capture_output=True, text=True).stdout.split():
        pkg = tomllib.load(open(os.path.join(ROOT, f), "rb")).get("package", {})
        n = pkg.get("name")
        if n:
            names |= {n, n.replace("-", "_")}
            for b in pkg.get("bin", []) + tomllib.load(open(os.path.join(ROOT, f), "rb")).get("bin", []):
                if "name" in b:
                    names |= {b["name"], b["name"].replace("-", "_")}
    return names


def size(p):
    t = 0
    for d, _, fs in os.walk(p):
        for f in fs:
            try:
                t += os.lstat(os.path.join(d, f)).st_size
            except OSError:
                pass
    return t


def prune(target, own):
    # (<name>-<hash>..., or a directory <name> in the newer build-dir layout:
    # <profile>/build/<name>/<hash>/, which nightly cargo uses)
    pat = re.compile(r"^(lib)?(%s)(-[0-9a-f]{16}|$)" % "|".join(sorted(map(re.escape, own))))
    before = size(target)
    # (any layout: target/<profile>, target/<triple>/<profile>, and the newer one where a
    # profile has no .fingerprint of its own, only build/<name>/<hash>/)
    for d, subs, _ in os.walk(target):
        for sub in list(subs):
            p = os.path.join(d, sub)
            if sub == "incremental":
                shutil.rmtree(p, ignore_errors=True)
            elif sub in (".fingerprint", "deps", "build"):
                for e in os.listdir(p):
                    if pat.match(e):
                        q = os.path.join(p, e)
                        shutil.rmtree(q) if os.path.isdir(q) and not os.path.islink(q) else os.remove(q)
            else:
                continue
            subs.remove(sub)
    print(f"{target}: {before >> 20} MiB -> {size(target) >> 20} MiB")


if __name__ == "__main__":
    own = own_crates()
    for t in sys.argv[1:]:
        if os.path.isdir(t):
            prune(t, own)
