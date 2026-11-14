#!/usr/bin/env python3
"""GPU metamorphic test: disabled blending and ONE/ZERO ADD must render identically.

Uses run-browser.mjs (MASU, WEB and PLAYWRIGHT environment variables as documented there).
Requires i686-w64-mingw32-gcc/windres on PATH. No Windows golden output is invented:
both actual PE programs run on the same browser/runtime and their outputs are compared.
"""
import difflib
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent


NAMES = ["d3d9rtfmt-disabled-32", "d3d9rtfmt-identity-32"]


def build(exe):
    """The two programs into `exe` (made), with their browser.txt."""
    exe.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="identity-blend-") as tmp:
        root = Path(tmp)
        original = HERE / "probes/d3d9rtfmt.c"
        source = original.read_text().replace('#include "../probe.h"', f'#include "{HERE / "probe.h"}"')
        disabled = "IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);"
        identity = """IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_ONE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_ZERO);"""
        assert source.count(disabled) == 2, "Update the transformation when the probe changes"
        variant = root / "identity.c"
        variant.write_text(source.replace(disabled, identity))
        resource = root / "ja.res"
        subprocess.run(["i686-w64-mingw32-windres", "ja.rc", "-O", "coff", "-o", str(resource)], cwd=HERE, check=True)
        for name, src in zip(NAMES, [original, variant]):
            subprocess.run(["i686-w64-mingw32-gcc", "-O2", "-msse2", str(src), str(resource),
                            str(HERE / "fpu53.c"), "-ld3d9", "-o", str(exe / f"{name}.exe")], check=True)
    (exe / "browser.txt").write_text("\n".join(NAMES) + "\n")


def check(exe, out):
    """Runs build()'s programs on sake in the browser; their outputs must be the same."""
    env = dict(os.environ)
    env.setdefault("TIMEOUT", "60")
    env["DEBUG_CONSOLE"] = "1"
    subprocess.run(["node", str(HERE / "run-browser.mjs"), str(exe), str(out)], env=env, check=True)
    outputs = []
    for name in NAMES:
        result = out / name
        diagnostics = (result / "stderr.txt").read_text()
        assert (result / "exit").read_text().strip() == "0", diagnostics
        if env.get("MEMORY") in ("fixed", "grow"):
            expected = "sake-grow.wasm" if env["MEMORY"] == "grow" else "sake.wasm"
            assert (result / "runtime").read_text().strip() == expected, diagnostics
        lines = (result / "stdout.txt").read_text().splitlines()
        for fmt in ["R5G6B5", "A1R5G5B5", "A4R4G4B4", "A2R10G10B10", "V8U8", "Q8W8V8U8"]:
            assert any(line.startswith(fmt + " rendertarget ") and line.endswith("create 0") for line in lines), lines
        outputs.append(lines)
    delta = list(difflib.unified_diff(*outputs, fromfile=NAMES[0], tofile=NAMES[1], lineterm=""))
    assert not delta, "\n".join(delta)
    print(f"identity blend: {len(outputs[0])} output lines match")


def main():
    # identity-blend.py: both steps in a temporary directory; "build EXE" / "check EXE OUT" split
    # them (CI: the probes job has the compilers, a browser job the GPU)
    a = sys.argv[1:]
    if a[:1] == ["build"] and len(a) == 2:
        return build(Path(a[1]))
    if a[:1] == ["check"] and len(a) == 3:
        return check(Path(a[1]), Path(a[2]))
    if a:
        sys.exit("usage: identity-blend.py [build EXE | check EXE OUT]")
    with tempfile.TemporaryDirectory(prefix="identity-blend-") as tmp:
        build(Path(tmp) / "exe")
        check(Path(tmp) / "exe", Path(tmp) / "out")


if __name__ == "__main__":
    main()
