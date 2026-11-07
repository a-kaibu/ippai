#!/usr/bin/env python3
"""The second Win16 benchmark game (suite.mjs's "win16-pascal" group): testdata/novel16/script.txt
in Shift_JIS played by novel16p.exe (tests/bench/build-novel16p.sh), with the KiriKiri game's
pictures (testdata/kirikiri) at half size in one shared 255-colour palette (index 255: where a
sprite doesn't show) as .pic files (width, height: 16-bit words, then the rows top-down), the
palette and a darkening table for the text window (pal.bin: 256 RGBQUADs, then 256 indices), and
its voices: OUT_DIR/novel16p/.

  python3 -I tests/bench/pack-novel16p.py OUT_DIR BUILD_DIR    (BUILD_DIR: build-novel16p.sh's)
"""
import os, shutil, struct, sys
from PIL import Image

out, build = sys.argv[1], sys.argv[2]
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "testdata")
kk = os.path.join(root, "kirikiri")
dst = os.path.join(out, "novel16p")
shutil.rmtree(dst, ignore_errors=True)
os.makedirs(dst)

pics = []  # (name, RGBA at the game's size, sprite)
for sub, sprite, size in (("bgimage", False, None), ("fgimage", True, None), ("image", True, (32, 32))):
    for f in sorted(os.listdir(os.path.join(kk, sub))):
        if f.endswith(".png"):
            im = Image.open(os.path.join(kk, sub, f)).convert("RGBA")
            im = im.resize(size or (im.width // 2, im.height // 2), Image.LANCZOS)
            pics.append((f[:-4], im, sprite))

# one palette for every picture: from all their shown pixels together
sheet = Image.new("RGB", (sum(p[1].width for p in pics), max(p[1].height for p in pics)))
x = 0
for _, im, _ in pics:
    sheet.paste(im.convert("RGB"), (x, 0))
    x += im.width
# (223 colours from the pictures, 32 of a dark blue ramp for the text window: 223..254)
RAMP = 223
pal_img = sheet.quantize(RAMP, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
ramp = [(4 + i * 2, 4 + i * 2, 24 + i * 3) for i in range(32)]
pal = pal_img.getpalette()[: RAMP * 3] + [v for c in ramp for v in c] + [0, 0, 0]
pal_img.putpalette(pal)
colors = [tuple(pal[3 * i : 3 * i + 3]) for i in range(256)]


for name, im, sprite in pics:
    q = im.convert("RGB").quantize(palette=pal_img, dither=Image.Dither.NONE)
    px = bytearray(q.tobytes())
    if sprite:
        for i, a in enumerate(im.getchannel("A").tobytes()):
            if a < 128:
                px[i] = 255
    with open(os.path.join(dst, name + ".pic"), "wb") as f:
        f.write(struct.pack("<HH", im.width, im.height) + bytes(px))

# the text window: each colour as the ramp's step of its brightness (the ramp itself: its own)
dark = bytes(i if RAMP <= i < 255 else RAMP + min(31, (r * 3 + g * 6 + b) // 10 // 8) for i, (r, g, b) in enumerate(colors))
with open(os.path.join(dst, "pal.bin"), "wb") as f:
    f.write(b"".join(bytes((b, g, r, 0)) for r, g, b in colors) + dark)

shutil.copytree(os.path.join(kk, "sound"), os.path.join(dst, "sound"))
script = open(os.path.join(root, "novel16", "script.txt"), encoding="utf-8").read()
with open(os.path.join(dst, "script.txt"), "wb") as f:
    f.write(script.replace("\n", "\r\n").encode("cp932"))
shutil.copy(os.path.join(build, "novel16p.exe"), dst)
shutil.copy(os.path.join(build, "COPYING.fpc"), dst)
print(f"novel16p/: {len(pics)} pictures, {len(script.splitlines())} lines of script")
