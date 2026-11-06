#!/usr/bin/env python3
"""Writes imaging_data.h: the pictures the imaging probe decodes, in the file formats and
variants games ship (PNG, BMP, JPEG, GIF), made with Pillow from one 21x13 picture with alpha.
Run again only to change the set (the bytes are what Windows' answers were recorded from)."""
import io, os, struct, zlib
from PIL import Image

W, H = 21, 13
src = Image.new("RGBA", (W, H))
for y in range(H):
    for x in range(W):
        r = x * 255 // (W - 1)
        g = y * 255 // (H - 1)
        b = 255 if (x // 3 + y // 3) % 2 else 40
        a = 255 if x < 7 else (128 if x < 14 else (y * 255 // (H - 1)))
        src.putpixel((x, y), (r, g, b, a))
rgb = src.convert("RGB")

def save(img, fmt, **kw):
    b = io.BytesIO()
    img.save(b, fmt, **kw)
    return b.getvalue()

def png_raw(w, h, ctype, depth, rows, plte=None, trns=None, interlace=0):
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, ctype, 0, 0, interlace))
    if plte: out += chunk(b"PLTE", plte)
    if trns: out += chunk(b"tRNS", trns)
    return out + chunk(b"IDAT", zlib.compress(b"".join(b"\0" + r for r in rows))) + chunk(b"IEND", b"")

pics = []
pics.append(("png_rgba8", save(src, "PNG")))
pics.append(("png_rgb8", save(rgb, "PNG")))
pics.append(("png_gray8", save(src.convert("L"), "PNG")))
pics.append(("png_graya8", save(src.convert("LA"), "PNG")))
pal = src.convert("RGB").quantize(colors=16)
pics.append(("png_pal8", save(pal, "PNG")))
pics.append(("png_pal4", save(pal, "PNG", bits=4)))
pics.append(("png_pal_trns", save(src.quantize(colors=32, method=Image.Quantize.FASTOCTREE), "PNG")))
pics.append(("png_gray1", save(src.convert("1"), "PNG")))
# 16-bit channels and Adam7 written by hand (Pillow writes neither for RGBA)
rows16 = [b"".join(struct.pack(">HHH", min(65535, px[0] * 257 + x), min(65535, px[1] * 257 + 3 * y), px[2] * 257) for x in range(W) for px in [rgb.getpixel((x, y))]) for y in range(H)]
pics.append(("png_rgb16", png_raw(W, H, 2, 16, rows16)))
rows_g16 = [b"".join(struct.pack(">H", (x * 3000 + y * 1000) & 0xffff) for x in range(W)) for y in range(H)]
pics.append(("png_gray16", png_raw(W, H, 0, 16, rows_g16)))
pics.append(("png_rgb_trns", png_raw(W, H, 2, 8, [rgb.tobytes()[y * W * 3:(y + 1) * W * 3] for y in range(H)], trns=struct.pack(">HHH", 255, 0, 255))))
def adam7(img):
    passes = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    data = b""
    for x0, y0, dx, dy in passes:
        xs = list(range(x0, W, dx)); ys = list(range(y0, H, dy))
        if not xs or not ys: continue
        for y in ys:
            data += b"\0" + b"".join(bytes(img.getpixel((x, y))) for x in xs)
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 1)) + chunk(b"IDAT", zlib.compress(data)) + chunk(b"IEND", b"")
pics.append(("png_rgba8_adam7", adam7(src)))


pics.append(("bmp24", save(rgb, "BMP")))
pics.append(("bmp8", save(pal, "BMP")))
pics.append(("bmp1", save(src.convert("1"), "BMP")))
def bmp(bpp, rows, pal=b"", comp=0, masks=b"", hdr=40, top=False, nclr=0):
    stride = (W * bpp + 31) // 32 * 4
    body = b"".join(r + b"\0" * (stride - len(r)) for r in (rows if top else rows[::-1])) if comp in (0, 3) else rows
    info = struct.pack("<IiiHHIIiiII", hdr, W, -H if top else H, 1, bpp, comp, len(body), 2835, 2835, nclr, 0)
    if hdr > 40:
        info += masks + b"\0" * (hdr - 40 - len(masks))
    elif masks:
        info += masks
    off = 14 + len(info) + len(pal)
    return b"BM" + struct.pack("<IHHI", off + len(body), 0, 0, off) + info + pal + body
px = [[src.getpixel((x, y)) for x in range(W)] for y in range(H)]
rows32 = [b"".join(bytes((p[2], p[1], p[0], p[3])) for p in r) for r in px]
pics.append(("bmp32", bmp(32, rows32)))
pics.append(("bmp32_v5_alpha", bmp(32, rows32, comp=3, hdr=124, masks=struct.pack("<IIII", 0xff0000, 0xff00, 0xff, 0xff000000))))
pics.append(("bmp24_topdown", bmp(24, [b"".join(bytes((p[2], p[1], p[0])) for p in r) for r in px], top=True)))
rows555 = [b"".join(struct.pack("<H", (p[0] >> 3) << 10 | (p[1] >> 3) << 5 | p[2] >> 3) for p in r) for r in px]
pics.append(("bmp16_555", bmp(16, rows555)))
rows565 = [b"".join(struct.pack("<H", (p[0] >> 3) << 11 | (p[1] >> 2) << 5 | p[2] >> 3) for p in r) for r in px]
pics.append(("bmp16_565", bmp(16, rows565, comp=3, masks=struct.pack("<III", 0xf800, 0x07e0, 0x001f))))
p16 = bytes(b for i in range(16) for b in (i * 17, 255 - i * 17, (i * 53) & 255, 0))
idx = [[(x // 2 + y) % 16 for x in range(W)] for y in range(H)]
rows4 = [bytes(((r[i] << 4) | (r[i + 1] if i + 1 < W else 0)) for i in range(0, W, 2)) for r in idx]
pics.append(("bmp4", bmp(4, rows4, pal=p16, nclr=16)))
def rle8(rows):
    out = b""
    for r in rows[::-1]:
        i = 0
        while i < len(r):
            j = i
            while j < len(r) and r[j] == r[i] and j - i < 255: j += 1
            out += bytes((j - i, r[i])); i = j
        out += b"\0\0"
    return out + b"\0\1"
p256 = bytes(b for i in range(256) for b in ((i * 7) & 255, (i * 13) & 255, i, 0))
idx8 = [bytes(((x // 3) * 16 + y) & 255 for x in range(W)) for y in range(H)]
pics.append(("bmp8_rle", bmp(8, rle8(idx8), pal=p256, comp=1, nclr=256)))

pics.append(("jpeg_420", save(rgb, "JPEG", quality=90, subsampling=2)))
pics.append(("jpeg_444", save(rgb, "JPEG", quality=90, subsampling=0)))
pics.append(("jpeg_422", save(rgb, "JPEG", quality=75, subsampling=1)))
pics.append(("jpeg_progressive", save(rgb, "JPEG", quality=85, progressive=True)))
pics.append(("jpeg_gray", save(rgb.convert("L"), "JPEG", quality=90)))
pics.append(("jpeg_restart", save(rgb, "JPEG", quality=60, restart_marker_blocks=1)))
pics.append(("jpeg_cmyk", save(rgb.convert("CMYK"), "JPEG", quality=95)))

pics.append(("gif", save(pal, "GIF")))
pics.append(("gif_trns", save(src.convert("RGB").quantize(colors=8), "GIF", transparency=0)))
pics.append(("gif_interlaced", save(pal, "GIF", interlace=True)))
fr = [pal, pal.transpose(Image.Transpose.FLIP_LEFT_RIGHT)]
pics.append(("gif_anim", (lambda b: (fr[0].save(b, "GIF", save_all=True, append_images=fr[1:], duration=[50, 120], loop=0), b.getvalue())[1])(io.BytesIO())))

out = ["/* generated by imaging_gen.py: the probe's pictures */"]
for n, d in pics:
    out.append(f"static const unsigned char {n}[{len(d)}] = {{")
    for i in range(0, len(d), 24):
        out.append("    " + ", ".join(f"0x{b:02x}" for b in d[i:i + 24]) + ",")
    out.append("};")
out.append("static const struct { const char *name; const unsigned char *data; unsigned size; } pictures[] = {")
for n, d in pics:
    out.append(f'    {{ "{n}", {n}, sizeof {n} }},')
out.append("};")
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "imaging_data.h"), "w").write("\n".join(out) + "\n")
print(len(pics), "pictures", sum(len(d) for _, d in pics), "bytes")
