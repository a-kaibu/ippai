#!/usr/bin/env python3
"""GPU metamorphic test: disabled blending and ONE/ZERO ADD must render identically.

Uses run-browser.mjs (IPPAI, WEB and PLAYWRIGHT environment variables as documented there).
Requires i686-w64-mingw32-gcc/windres on PATH. No Windows golden output is invented:
both actual PE programs run on the same browser/runtime and their outputs are compared.
"""
import difflib
import os
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent


def main():
    with tempfile.TemporaryDirectory(prefix="identity-blend-") as tmp:
        root = Path(tmp)
        exe, out = root / "exe", root / "out"
        exe.mkdir()
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
        names = ["d3d9rtfmt-disabled-32", "d3d9rtfmt-identity-32"]
        for name, src in zip(names, [original, variant]):
            subprocess.run(["i686-w64-mingw32-gcc", "-O2", "-msse2", str(src), str(resource),
                            str(HERE / "fpu53.c"), "-ld3d9", "-o", str(exe / f"{name}.exe")], check=True)
        (exe / "browser.txt").write_text("\n".join(names) + "\n")
        env = dict(os.environ)
        env.setdefault("TIMEOUT", "60")
        env["DEBUG_CONSOLE"] = "1"
        subprocess.run(["node", str(HERE / "run-browser.mjs"), str(exe), str(out)], env=env, check=True)
        outputs = []
        for name in names:
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
        delta = list(difflib.unified_diff(*outputs, fromfile=names[0], tofile=names[1], lineterm=""))
        assert not delta, "\n".join(delta)
        print(f"identity blend: {len(outputs[0])} output lines match")


if __name__ == "__main__":
    main()
