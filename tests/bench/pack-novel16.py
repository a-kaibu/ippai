#!/usr/bin/env python3
"""The Win16 benchmark's game (suite.mjs's "win16" group, played by scenario.mjs):
testdata/novel16's script in Shift_JIS, with the KiriKiri game's pictures (testdata/kirikiri) as
256-colour BMPs at half size (the game draws them twice as large: backgrounds 640x360, sprites
with a <name>m.bmp mask, black where they show) and its voices, next to novel16.exe
(tests/bench/build-novel16.sh): OUT_DIR/novel16/.

  python3 -E -P tests/bench/pack-novel16.py OUT_DIR BUILD_DIR    (BUILD_DIR: build-novel16.sh's)
"""
import os, shutil, sys
from PIL import Image

out, build = sys.argv[1], sys.argv[2]
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "testdata")
kk = os.path.join(root, "kirikiri")
dst = os.path.join(out, "novel16")
shutil.rmtree(dst, ignore_errors=True)
os.makedirs(dst)


def save8(im, path):
    """an 8-bit BMP (PIL writes palette images as BITMAPINFOHEADER + 256 RGBQUADs)"""
    im.save(path, "BMP")


n = 0
for sub, sprite in (("bgimage", False), ("fgimage", True), ("image", True)):
    for f in sorted(os.listdir(os.path.join(kk, sub))):
        if not f.endswith(".png"):
            continue
        name = f[:-4]
        im = Image.open(os.path.join(kk, sub, f)).convert("RGBA")
        im = im.resize((im.width // 2, im.height // 2), Image.LANCZOS)
        rgb = im.convert("RGB").quantize(255, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
        if sprite:
            # (index 255: black, where the sprite doesn't show; the mask is white there)
            pal = rgb.getpalette()[: 255 * 3] + [0, 0, 0]
            rgb.putpalette(pal)
            shown = im.getchannel("A").point(lambda a: 255 if a >= 128 else 0)
            px, sh = rgb.load(), shown.load()
            for y in range(im.height):
                for x in range(im.width):
                    if not sh[x, y]:
                        px[x, y] = 255
            mask = Image.eval(shown, lambda a: 0 if a else 255).convert("P", palette=Image.Palette.ADAPTIVE, colors=2)
            save8(mask, os.path.join(dst, name + "m.bmp"))
        save8(rgb, os.path.join(dst, name + ".bmp"))
        n += 1
shutil.copytree(os.path.join(kk, "sound"), os.path.join(dst, "sound"))
script = open(os.path.join(root, "novel16", "script.txt"), encoding="utf-8").read()
with open(os.path.join(dst, "script.txt"), "wb") as f:
    f.write(script.replace("\n", "\r\n").encode("cp932"))
shutil.copy(os.path.join(build, "novel16.exe"), dst)
shutil.copy(os.path.join(build, "LICENSE.watcom"), dst)
print(f"novel16/: {n} pictures, {len(script.splitlines())} lines of script")
