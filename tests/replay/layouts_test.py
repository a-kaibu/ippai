#!/usr/bin/env python3
"""layouts.txt (clang's layouts of the headers) against gcc's own: every struct's size and every
named member's offset, asserted at compile time with mingw-w64's gcc for i686 and x86_64 (the
compiler the replayer is built with; what the converter packs has to be what it reads)."""
import os
import re
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import layouts  # noqa: E402

text = open(os.path.join(HERE, "..", "..", "crates", "apitrace", "src", "layouts.txt")).read()
recs, cur = [], None
for line in text.splitlines():
    if line.startswith("R "):
        _, name, s32, s64, kind, tu = line.split()
        cur = (name, int(s32), int(s64), kind, tu, [])
        recs.append(cur)
    elif line.startswith(" ") and cur:
        o32, o64, m, t = line.split()
        cur[5].append((m, int(o32), int(o64), t))
    else:
        cur = None
fails, n = 0, 0
heads = {tu: "".join(open(h, encoding="latin-1").read() for h in layouts.headers(tu)) for tu in layouts.TUS}
for tu, hs in layouts.TUS.items():
    c = ["#define COBJMACROS", "#define CINTERFACE", "#include <stddef.h>"] + [f"#include <{h}>" for h in hs]
    for name, s32, s64, kind, rtu, ms in recs:
        if rtu != tu or "::" in name:
            continue
        cname = name[2:] if name[:2] in ("8.", "7.") else name
        # (a "typedef struct { ... } NAME" is named by its typedef: no tag to say "struct NAME" by;
        # DECLARE_HANDLE makes "struct NAME__")
        ty = f"{kind} {cname}" if cname.endswith("__") or re.search(rf"\b{kind}\s+{re.escape(cname)}\b", heads[tu]) else cname
        c.append(f"_Static_assert(sizeof({ty}) == (sizeof(void *) == 8 ? {s64} : {s32}), \"{name} size\");")
        for m, o32, o64, t in ms:
            if m == "-" or t.startswith("b"):
                continue
            c.append(f"_Static_assert(offsetof({ty}, {m}) == (sizeof(void *) == 8 ? {o64} : {o32}), \"{name}.{m}\");")
            n += 1
    with tempfile.NamedTemporaryFile("w", suffix=".c", delete=False) as f:
        f.write("\n".join(c) + "\n")
    for cc in ("i686-w64-mingw32-gcc", "x86_64-w64-mingw32-gcc"):
        r = subprocess.run([cc, "-fsyntax-only", f.name], capture_output=True, text=True)
        if r.returncode:
            print(f"layouts_test: {tu} with {cc}:\n" + "\n".join(r.stderr.splitlines()[:20]))
            fails += 1
    os.unlink(f.name)
if fails:
    sys.exit(1)
print(f"layouts_test: {n} member offsets and {len(recs)} records agree with gcc (i686, x86_64)")
