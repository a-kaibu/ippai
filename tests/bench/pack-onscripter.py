#!/usr/bin/env python3
"""The ONScripter benchmark's game (suite.mjs's "onscripter" group, played by scenario.mjs):
testdata/onscripter/0.txt in Shift_JIS (as NScripter reads it) with the KiriKiri game's pictures
and sounds (testdata/kirikiri) loose in its folders, next to onscripter.exe
(tests/bench/build-onscripter.sh) and default.ttf (IPA Gothic): OUT_DIR/onscripter/; the same
with the x64 build (onscripter64.exe as onscripter.exe, on sake's own msvcrt): OUT_DIR/onscripter64/.

  python3 -I tests/bench/pack-onscripter.py OUT_DIR ONS_DIR [FONT]
    ONS_DIR: build-onscripter.sh's;
    FONT: default /usr/share/fonts/opentype/ipafont-gothic/ipag.ttf
"""
import os, shutil, sys

out, ons = sys.argv[1], sys.argv[2]
font = sys.argv[3] if len(sys.argv) > 3 else "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf"
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "testdata")
script = open(os.path.join(root, "onscripter", "0.txt"), encoding="utf-8").read()
for name, exe in (("onscripter", "onscripter.exe"), ("onscripter64", "onscripter64.exe")):
    dst = os.path.join(out, name)
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst)
    for sub in ("bgimage", "fgimage", "image", "rule", "sound", "bgm"):
        shutil.copytree(os.path.join(root, "kirikiri", sub), os.path.join(dst, sub))
    with open(os.path.join(dst, "0.txt"), "wb") as f:
        f.write(script.replace("\n", "\r\n").encode("cp932"))
    # (the mode the script reads with getreg: --registry play.reg or load.reg)
    for mode in ("play", "load"):
        with open(os.path.join(dst, f"{mode}.reg"), "w") as f:
            f.write(f'[bench]\n"mode"="{mode}"\n')
    shutil.copy(os.path.join(ons, exe), os.path.join(dst, "onscripter.exe"))
    shutil.copy(os.path.join(ons, "COPYING.onscripter"), dst)
    shutil.copy(font, os.path.join(dst, "default.ttf"))
print(f"onscripter/, onscripter64/: {len(script.splitlines())} lines of script")
