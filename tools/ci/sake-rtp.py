#!/usr/bin/env python3
"""Makes ippai's own RPG Maker run time packages (RTP) for sake's C:\\: the files games made with
RPG Maker look for in the RTP folder (Graphics/..., Audio/..., Fonts/), under the official
names and image sizes (tools/rtp/<rtp>.tsv), with content made here. Nothing of the official
RTP is in them: its license forbids redistributing it (docs/rtp.md). The pictures are simple
shapes in the layout the engine cuts them by (character sheets, faces, the window skin's
parts and text colors, tilesets, icons); the sounds are short tones. A game that uses RTP
material runs, but shows and plays these instead of the official art and music.

  tools/ci/sake-rtp.py OUT_DIR [RTP...]     (default: every tools/rtp/*.tsv)

OUT_DIR/<rtp>/ for each RTP (rpgvxace: RPG Maker VX Ace). Needs oggenc (apt vorbis-tools) for
the sounds and the VL Gothic fonts (apt fonts-vlgothic: VX Ace's RTP font, free to
redistribute). The same input makes the same files (randomness is seeded by each file's name).
"""
import glob, hashlib, math, os, random, shutil, struct, subprocess, sys, tempfile, wave, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFESTS = os.path.join(HERE, '..', 'rtp')
VLGOTHIC = ['/usr/share/fonts/truetype/vlgothic', '/usr/share/fonts/opentype/vlgothic']
RATE = 22050


class Image:
    """RGBA pixels, drawn with spans (whole-row slices: fast enough in Python)."""

    def __init__(self, w, h, fill=(0, 0, 0, 0)):
        self.w, self.h = w, h
        self.px = bytearray(bytes(fill) * (w * h))

    def rect(self, x, y, w, h, c):
        x0, x1 = max(0, x), min(self.w, x + w)
        if x1 <= x0:
            return
        row = bytes(c) * (x1 - x0)
        for yy in range(max(0, y), min(self.h, y + h)):
            o = (yy * self.w + x0) * 4
            self.px[o:o + len(row)] = row

    def ellipse(self, cx, cy, rx, ry, c):
        if rx <= 0 or ry <= 0:
            return
        for yy in range(int(cy - ry), int(cy + ry) + 1):
            d = 1 - ((yy + 0.5 - cy) / ry) ** 2
            if d <= 0:
                continue
            half = rx * math.sqrt(d)
            x0 = int(round(cx - half))
            self.rect(x0, yy, int(round(cx + half)) - x0, 1, c)

    def ring(self, cx, cy, r, t, c):
        for yy in range(int(cy - r), int(cy + r) + 1):
            dy = yy + 0.5 - cy
            for side in (-1, 1):
                if abs(dy) >= r:
                    continue
                outer = math.sqrt(r * r - dy * dy)
                inner = math.sqrt((r - t) ** 2 - dy * dy) if abs(dy) < r - t else 0
                a, b = (cx + inner, cx + outer) if side > 0 else (cx - outer, cx - inner)
                self.rect(int(round(a)), yy, max(1, int(round(b)) - int(round(a))), 1, c)

    def vgradient(self, x, y, w, h, top, bottom):
        for i in range(h):
            t = i / max(1, h - 1)
            self.rect(x, y + i, w, 1, tuple(int(top[k] + (bottom[k] - top[k]) * t) for k in range(4)))

    def png(self, path):
        raw = b''.join(b'\0' + bytes(self.px[y * self.w * 4:(y + 1) * self.w * 4]) for y in range(self.h))
        chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
        data = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', self.w, self.h, 8, 6, 0, 0, 0))
        data += chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')
        with open(path, 'wb') as f:
            f.write(data)


def rng(name):
    return random.Random(hashlib.sha256(name.encode()).digest())


def hue(r, s=0.55, v=0.85, a=255):
    """A color of hue `r` (0..1) as RGBA."""
    i, f = int(r * 6) % 6, r * 6 - int(r * 6)
    p, q, t = v * (1 - s), v * (1 - s * f), v * (1 - s * (1 - f))
    rgb = [(v, t, p), (q, v, p), (p, v, t), (p, q, v), (t, p, v), (v, p, q)][i]
    return tuple(int(x * 255) for x in rgb) + (a,)


def dark(c, k=0.6):
    return tuple(int(x * k) for x in c[:3]) + (c[3],)


def figure(img, x, y, w, h, body, d, step):
    """A walking figure in the w x h frame at (x, y): facing d (0 down, 1 left, 2 right, 3 up), step -1/0/1."""
    cx = x + w / 2
    img.ellipse(cx, y + h * 0.9, w * 0.25, h * 0.06, (0, 0, 0, 90))
    for side in (-1, 1):
        lift = step * side * h * 0.05
        img.rect(int(cx + side * w * 0.1 - w * 0.06), int(y + h * 0.66 - lift), int(w * 0.12), int(h * 0.22), dark(body, 0.45))
    img.ellipse(cx, y + h * 0.58, w * 0.24, h * 0.2, body)
    img.ellipse(cx, y + h * 0.3, w * 0.2, h * 0.19, (250, 220, 190, 255))
    img.ellipse(cx, y + h * 0.2, w * 0.22, h * 0.12, dark(body, 0.5))
    if d != 3:
        off = {0: 0, 1: -w * 0.07, 2: w * 0.07}[d]
        for side in ((-1, 1) if d == 0 else (0,)):
            img.rect(int(cx + off + side * w * 0.08 - 1), int(y + h * 0.3), max(1, w // 16), max(1, h // 14), (30, 30, 40, 255))


def characters(img, name, r):
    base = os.path.basename(name)
    single = '$' in base
    cols, rows = (1, 1) if single else (4, 2)
    cw, ch = img.w // cols, img.h // rows
    fw, fh = cw // 3, ch // 4
    for c in range(cols * rows):
        body = hue(r.random())
        ox, oy = c % cols * cw, c // cols * ch
        for d in range(4):
            for s in range(3):
                x, y = ox + s * fw, oy + d * fh
                if base.startswith('!') and not single:
                    # (objects: chests, doors, switches; the frames open them)
                    img.rect(x + fw // 6, y + fh // 4 + s * fh // 16, fw * 2 // 3, fh * 2 // 3 - s * fh // 16, body)
                    img.rect(x + fw // 6, y + fh // 4, fw * 2 // 3, max(2, fh // 10), dark(body))
                else:
                    figure(img, x, y, fw, fh, body, d, s - 1)


def faces(img, name, r):
    for c in range(8):
        x, y = c % 4 * 96, c // 4 * 96
        hair = hue(r.random(), 0.5, 0.6)
        img.rect(x, y, 96, 96, hue(r.random(), 0.15, 0.9))
        img.ellipse(x + 48, y + 56, 30, 36, (250, 220, 190, 255))
        img.ellipse(x + 48, y + 28, 34, 20, hair)
        for side in (-1, 1):
            img.ellipse(x + 48 + side * 12, y + 56, 4, 6, (40, 40, 60, 255))
        img.rect(x + 40, y + 76, 16, 3, (170, 80, 80, 255))


def battler(img, name, r):
    w, h = img.w, img.h
    body = hue(r.random(), 0.6, 0.75)
    img.ellipse(w / 2, h * 0.6, w * 0.45, h * 0.38, body)
    img.ellipse(w / 2, h * 0.3, w * 0.28, h * 0.25, dark(body, 0.8))
    for side in (-1, 1):
        img.ellipse(w / 2 + side * w * 0.1, h * 0.28, max(1, w * 0.04), max(1, h * 0.04), (250, 240, 120, 255))


def animation(img, name, r):
    col = hue(r.random(), 0.5, 1.0)
    n = 0
    for y in range(0, img.h, 192):
        for x in range(0, img.w, 192):
            rad = 20 + (n % 10) * 7
            img.ring(x + 96, y + 96, rad, 8, col[:3] + (200 - (n % 10) * 15,))
            n += 1


def tileset(img, name, r):
    kind = os.path.basename(name).rsplit('_', 1)[-1].split('.')[0]
    t = 32
    if kind in ('B', 'C', 'D', 'E'):
        for y in range(0, img.h, t):
            for x in range(0, img.w, t):
                if (x, y) == (0, 0) or r.random() < 0.4:
                    continue  # (the first tile of B is the empty one)
                c = hue(r.random(), 0.45, 0.7)
                img.rect(x + 6, y + 8, t - 12, t - 12, c)
                img.rect(x + 6, y + 8, t - 12, 3, dark(c))
        return
    # A1..A5: autotile blocks and plain tiles in one color each, a darker edge per tile
    block = 64 if kind in ('A1', 'A2', 'A3', 'A4') else 32
    for y in range(0, img.h, block):
        for x in range(0, img.w, block):
            c = hue(r.random(), 0.4, 0.55 + 0.3 * r.random())
            img.rect(x, y, block, block, c)
            for yy in range(y, y + block, t):
                img.rect(x, yy + t - 2, block, 2, dark(c, 0.85))


def picture(img, name, r, alpha=255):
    a, b = hue(r.random(), 0.4, 0.8, alpha), hue(r.random(), 0.5, 0.35, alpha)
    img.vgradient(0, 0, img.w, img.h, a, b)
    for _ in range(12):
        img.ellipse(r.random() * img.w, r.random() * img.h, img.w * 0.08, img.h * 0.05, dark(a, 0.9))


def overlay(img, name, r):
    """Battlebacks2 / Titles2: drawn over another picture, mostly see-through."""
    c = hue(r.random(), 0.3, 0.4)
    img.rect(0, 0, img.w, img.h // 8, c)
    img.rect(0, img.h - img.h // 8, img.w, img.h // 8, c)
    for i in range(6):
        img.rect(int(img.w * (0.05 + 0.16 * i)), img.h // 8, img.w // 40, img.h * 3 // 4, dark(c, 0.8))


def window_skin(img, name, r):
    """VX Ace's window skin layout (128 x 128): back 0,0 64x64; back pattern 0,64; frame 64,0;
    cursor 64,64 32x32; pause sign 96,64 (2x2 of 16x16); text colors 64,96 (8x4 cells of 8x8:
    Window_Base#text_color reads the cell's middle pixel)."""
    img.vgradient(0, 0, 64, 64, (40, 60, 120, 255), (10, 20, 60, 255))
    for y in range(64, 128, 8):
        for x in range(0, 64, 8):
            if (x + y) // 8 % 2:
                img.rect(x, y, 8, 8, (255, 255, 255, 24))
    for k in range(3):  # frame: three nested borders on the 64x64 part
        c = (230 - k * 50, 230 - k * 50, 255, 255)
        img.rect(64 + k, k, 64 - 2 * k, 1, c)
        img.rect(64 + k, 63 - k, 64 - 2 * k, 1, c)
        img.rect(64 + k, k, 1, 64 - 2 * k, c)
        img.rect(127 - k, k, 1, 64 - 2 * k, c)
    for x, y, rot in ((88, 4, 0), (88, 52, 1), (68, 24, 2), (116, 24, 3)):  # scroll arrows
        for i in range(6):
            w = 2 * (6 - i) if rot in (0, 2) else 2 * (i + 1)
            if rot < 2:
                img.rect(x + 8 - w // 2, y + i, w, 1, (255, 255, 255, 220))
            else:
                img.rect(x + i, y + 8 - w // 2, 1, w, (255, 255, 255, 220))
    img.rect(64, 64, 32, 32, (255, 255, 255, 60))  # cursor
    for k in (0, 31):
        img.rect(64, 64 + k, 32, 1, (255, 255, 255, 200))
        img.rect(64 + k, 64, 1, 32, (255, 255, 255, 200))
    for i in range(4):  # pause sign: 4 frames
        x, y = 96 + i % 2 * 16, 64 + i // 2 * 16
        img.ellipse(x + 8, y + 8 + (i % 2), 4, 3, (255, 255, 255, 255))
    for n, c in enumerate(TEXT_COLORS):
        img.rect(64 + n % 8 * 8, 96 + n // 8 * 8, 8, 8, c + (255,))


# The 32 text colors (\C[n]); 0 normal, 16 system, 17 crisis, 18 knockout, 19 gauge back, 20-21
# HP, 22-23 MP, 24 power up, 25 power down, 28-29 TP (Window_Base).
TEXT_COLORS = [
    (255, 255, 255), (32, 160, 224), (255, 120, 76), (102, 204, 64), (153, 204, 255), (204, 192, 255), (255, 255, 160), (128, 128, 128),
    (192, 192, 192), (32, 128, 204), (255, 56, 16), (0, 160, 16), (64, 154, 222), (160, 152, 255), (255, 204, 32), (0, 0, 0),
    (132, 170, 255), (255, 255, 64), (255, 32, 32), (32, 32, 64), (224, 128, 64), (240, 192, 64), (64, 128, 192), (64, 192, 240),
    (128, 255, 128), (192, 128, 128), (128, 128, 255), (255, 128, 255), (0, 160, 64), (0, 224, 96), (160, 96, 224), (192, 128, 255),
]


def icons(img, name, r):
    for i in range(1, (img.w // 24) * (img.h // 24)):  # (icon 0 is none)
        x, y = i % (img.w // 24) * 24, i // (img.w // 24) * 24
        c = hue(r.random(), 0.6, 0.9)
        if i % 3 == 0:
            img.ellipse(x + 12, y + 12, 8, 8, c)
        elif i % 3 == 1:
            img.rect(x + 5, y + 5, 14, 14, c)
        else:
            for k in range(8):
                img.rect(x + 12 - k, y + 4 + k * 2, 2 * k + 1, 2, c)


def balloon(img, name, r):
    for row in range(img.h // 32):
        c = hue(row / 10, 0.6, 0.9)
        for f in range(img.w // 32):
            x, y = f * 32, row * 32
            s = min(f, 3)
            img.ellipse(x + 16, y + 14, 6 + s * 3, 5 + s * 2, (255, 255, 255, 255))
            img.rect(x + 14, y + 22, 4, 6, (255, 255, 255, 255))
            img.ellipse(x + 16, y + 14, 2 + s, 2 + s, c)


def system(img, name, r):
    base = os.path.basename(name).lower()
    if base == 'window.png':
        window_skin(img, name, r)
    elif base == 'iconset.png':
        icons(img, name, r)
    elif base == 'balloon.png':
        balloon(img, name, r)
    elif base == 'shadow.png':
        img.ellipse(img.w / 2, img.h * 0.75, img.w * 0.4, img.h * 0.15, (0, 0, 0, 120))
    elif base == 'gameover.png':
        img.vgradient(0, 0, img.w, img.h, (40, 0, 0, 255), (0, 0, 0, 255))
    elif base == 'battlestart.png':
        for y in range(0, img.h, 8):
            img.rect(0, y, img.w, 8, (r.randrange(64, 192),) * 3 + (255,))
    else:
        picture(img, name, r)


DRAW = {
    'characters': characters, 'faces': faces, 'battlers': battler, 'animations': animation, 'tilesets': tileset,
    'system': system, 'battlebacks2': overlay, 'titles2': overlay,
}


def draw(path, w, h):
    img = Image(w, h)
    folder = path.split('/')[1].lower() if path.count('/') >= 2 else ''
    DRAW.get(folder, picture)(img, path, rng(path))
    return img


def tone(path):
    """A short sound for an Audio/<kind>/ file: BGM and BGS loop, ME a phrase, SE a blip."""
    r = rng(path)
    kind = path.split('/')[1].upper()
    secs = {'BGM': 8.0, 'BGS': 4.0, 'ME': 3.0}.get(kind, 0.25 + 0.25 * r.random())
    n = int(RATE * secs)
    scale = [0, 2, 4, 5, 7, 9, 11, 12]
    root = 196 * 2 ** (r.randrange(12) / 12)
    notes = [root * 2 ** (scale[r.randrange(8)] / 12) for _ in range(16)]
    out = bytearray()
    for i in range(n):
        t = i / RATE
        if kind == 'BGS':
            v = (r.random() * 2 - 1) * 0.15 * (0.6 + 0.4 * math.sin(2 * math.pi * 0.25 * t))
        elif kind == 'SE':
            f = notes[0] * (1 + 2 * t)
            v = math.sin(2 * math.pi * f * t) * 0.4 * (1 - i / n)
        else:
            step = secs / len(notes)
            k = int(t / step)
            local = t - k * step
            f = notes[k % len(notes)]
            v = (math.sin(2 * math.pi * f * t) + 0.3 * math.sin(4 * math.pi * f * t)) * 0.25 * min(1, local * 40) * math.exp(-3 * local)
        out += struct.pack('<h', int(max(-1, min(1, v)) * 32000))
    return bytes(out)


def ogg(path, pcm):
    with tempfile.TemporaryDirectory() as tmp:
        w = os.path.join(tmp, 'a.wav')
        with wave.open(w, 'wb') as f:
            f.setnchannels(1)
            f.setsampwidth(2)
            f.setframerate(RATE)
            f.writeframes(pcm)
        # (a fixed serial number: the same file each time)
        subprocess.run(['oggenc', '-Q', '-q', '0', '-s', '1', '-o', path, w], check=True)


def manifest(rtp):
    for line in open(os.path.join(MANIFESTS, f'{rtp}.tsv'), encoding='utf-8'):
        if line.strip() and not line.startswith('#'):
            yield line.rstrip('\n').split('\t')


def make(out, rtp):
    root = os.path.join(out, rtp)
    if os.path.exists(root):
        shutil.rmtree(root)
    n = 0
    for row in manifest(rtp):
        path = row[0]
        dest = os.path.join(root, *path.split('/'))
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        if path.lower().endswith('.png'):
            draw(path, int(row[1]), int(row[2])).png(dest)
        elif path.lower().endswith('.ogg'):
            ogg(dest, tone(path))
        else:
            sys.exit(f'{rtp}: {path}: no maker for this kind of file')
        n += 1
    if rtp == 'rpgvxace':
        fonts = os.path.join(root, 'Fonts')
        os.makedirs(fonts, exist_ok=True)
        for f in ('VL-Gothic-Regular.ttf', 'VL-PGothic-Regular.ttf'):
            src = next((os.path.join(d, f) for d in VLGOTHIC if os.path.exists(os.path.join(d, f))), None)
            if not src:
                sys.exit(f'{f} not found in {VLGOTHIC} (apt install fonts-vlgothic)')
            shutil.copy(os.path.realpath(src), fonts)
            n += 1
        lic = '/usr/share/doc/fonts-vlgothic/copyright'
        if os.path.exists(lic):
            shutil.copy(lic, os.path.join(fonts, 'VL-Gothic-LICENSE.txt'))
    with open(os.path.join(root, 'README.txt'), 'w') as f:
        f.write(f'ippai\'s own stand-in for the {rtp} RTP: the official file names and picture sizes, with pictures\n'
                'and sounds made by tools/ci/sake-rtp.py. Nothing in here is from the official RTP.\n'
                + ('Fonts/: VL Gothic (VL-Gothic-LICENSE.txt).\n' if rtp == 'rpgvxace' else ''))
    print(f'{rtp}: {n} files in {root}')


def main():
    out = sys.argv[1]
    rtps = sys.argv[2:] or sorted(os.path.basename(p)[:-4] for p in glob.glob(os.path.join(MANIFESTS, '*.tsv')))
    for rtp in rtps:
        make(out, rtp)


if __name__ == '__main__':
    main()
