#!/usr/bin/env python3
"""Tests of tests/corpus/shots.py: PNG decoding against an independent filtering encoder (all
five filters, random pictures), against what corpus.mjs writes (node) and against zlib streams
split over several IDATs; halving and the cell grid against direct sums; the comparison on
pictures with known differences (property tests over random pictures, fixed seeds).

  python3 tests/corpus/shots_test.py
"""
import os, random, shutil, struct, subprocess, sys, tempfile, unittest, zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shots

HERE = os.path.dirname(os.path.abspath(__file__))


def rand_pic(r, w, h, smooth=False):
    if not smooth:
        return w, h, bytes(r.randrange(256) for _ in range(w * h * 3))
    # (a picture like a screen: blocks of colour with a little noise)
    base = [[(r.randrange(256), r.randrange(256), r.randrange(256)) for _ in range(8)] for _ in range(8)]
    out = bytearray()
    for y in range(h):
        for x in range(w):
            c = base[y * 8 // h][x * 8 // w]
            out += bytes(max(0, min(255, v + r.randrange(-3, 4))) for v in c)
    return w, h, bytes(out)


def ref_encode(w, h, px, bpp, filters, parts=1):
    """an encoder that applies the given filter per row (cycled), written from the PNG spec"""
    s = w * bpp
    rows = [px[y * s:(y + 1) * s] for y in range(h)]
    raw, prev = bytearray(), bytes(s)
    for y, row in enumerate(rows):
        f = filters[y % len(filters)]
        out = bytearray()
        for i in range(s):
            a = row[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            p = [0, a, b, (a + b) >> 1, shots._paeth(a, b, c)][f]
            out.append((row[i] - p) & 255)
        raw += bytes([f]) + out
        prev = row
    z = zlib.compress(bytes(raw))
    cut = [len(z) * k // parts for k in range(parts + 1)]
    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d))
    ct = 2 if bpp == 3 else 6
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, ct, 0, 0, 0)) +
            b''.join(chunk(b'IDAT', z[cut[k]:cut[k + 1]]) for k in range(parts)) + chunk(b'IEND', b''))


class Png(unittest.TestCase):
    def test_round_trip(self):
        r = random.Random(1)
        for _ in range(40):
            p = rand_pic(r, r.randrange(1, 40), r.randrange(1, 30))
            self.assertEqual(shots.decode(shots.encode(*p)), p)

    def test_every_filter_against_reference_encoder(self):
        r = random.Random(2)
        for k in range(60):
            w, h, rgb = rand_pic(r, r.randrange(1, 33), r.randrange(1, 17), smooth=k % 2 == 0)
            filters = [r.randrange(5) for _ in range(r.randrange(1, 6))]
            parts = r.randrange(1, 4)
            self.assertEqual(shots.decode(ref_encode(w, h, rgb, 3, filters, parts)), (w, h, rgb), (w, h, filters))

    def test_rgba_drops_alpha(self):
        r = random.Random(3)
        for _ in range(20):
            w, h = r.randrange(1, 20), r.randrange(1, 20)
            rgba = bytes(r.randrange(256) for _ in range(w * h * 4))
            rgb = bytes(rgba[i] for i in range(len(rgba)) if i % 4 != 3)
            self.assertEqual(shots.decode(ref_encode(w, h, rgba, 4, [r.randrange(5)])), (w, h, rgb))

    def test_rejects(self):
        with self.assertRaises(ValueError): shots.decode(b'GIF89a')
        good = shots.encode(2, 2, bytes(12))
        bad = bytearray(good)
        bad[24] = 16  # (bit depth)
        with self.assertRaises(ValueError): shots.decode(bytes(bad))
        with self.assertRaises(ValueError): shots.encode(2, 2, bytes(11))

    @unittest.skipUnless(shutil.which('node'), 'no node')
    def test_reads_what_corpus_mjs_writes(self):
        # corpus.mjs's png() (BGRA in, RGB out), lifted from the file so the test follows it
        src = open(f'{HERE}/../corpus.mjs', encoding='utf-8').read()
        a = src.index('const CRC =')
        b = src.index('// a web game')
        r = random.Random(4)
        w, h = 37, 11
        bgra = bytes(r.randrange(256) for _ in range(w * h * 4))
        with tempfile.TemporaryDirectory() as d:
            open(f'{d}/in.bin', 'wb').write(bgra)
            open(f'{d}/t.mjs', 'w').write('import { readFileSync, writeFileSync } from "node:fs";\n'
                                          'import { deflateSync } from "node:zlib";\n' + src[a:b] +
                                          f'\nwriteFileSync("{d}/o.png", png({w}, {h}, readFileSync("{d}/in.bin")));\n')
            subprocess.run(['node', f'{d}/t.mjs'], check=True)
            got = shots.read(f'{d}/o.png')
        want = bytearray()
        for i in range(0, len(bgra), 4): want += bytes([bgra[i + 2], bgra[i + 1], bgra[i]])
        self.assertEqual(got, (w, h, bytes(want)))


class Grid(unittest.TestCase):
    def test_half_against_direct_average(self):
        r = random.Random(5)
        for _ in range(20):
            w, h, rgb = rand_pic(r, r.randrange(2, 30), r.randrange(2, 30))
            W, H, out = shots.half(w, h, rgb)
            self.assertEqual((W, H), (w // 2, h // 2))
            for _ in range(20):
                x, y, c = r.randrange(W), r.randrange(H), r.randrange(3)
                px = lambda X, Y: rgb[(Y * w + X) * 3 + c]
                s = px(2 * x, 2 * y) + px(2 * x + 1, 2 * y) + px(2 * x, 2 * y + 1) + px(2 * x + 1, 2 * y + 1)
                self.assertEqual(out[(y * W + x) * 3 + c], (s + 2) // 4)

    def test_cells_against_direct_average(self):
        r = random.Random(6)
        for w, h in [(32, 24), (64, 48), (100, 77), (640, 480)]:
            p = rand_pic(r, w, h, smooth=True)
            g = shots.cells(*p)
            step = 2 if w * h > 400 * 300 else 1
            for _ in range(10):
                cx, cy, c = r.randrange(32), r.randrange(24), r.randrange(3)
                xs = range(cx * w // 32, (cx + 1) * w // 32, step)
                ys = range(cy * h // 24, (cy + 1) * h // 24, step)
                vals = [p[2][(y * w + x) * 3 + c] for y in ys for x in xs]
                self.assertAlmostEqual(g[cy][cx][c], sum(vals) / len(vals), places=6)

    def test_too_small(self):
        with self.assertRaises(ValueError): shots.cells(10, 10, bytes(300))


def paint(p, x0, y0, x1, y1, col):
    w, h, rgb = p
    b = bytearray(rgb)
    for y in range(y0, y1):
        for x in range(x0, x1):
            b[(y * w + x) * 3:(y * w + x) * 3 + 3] = bytes(col)
    return w, h, bytes(b)


class Compare(unittest.TestCase):
    def test_same_picture_matches(self):
        r = random.Random(7)
        for _ in range(10):
            p = rand_pic(r, r.choice([640, 800]), r.choice([480, 600]), smooth=True)
            self.assertTrue(shots.compare(p, p)['ok'])
            # the golden is halved: it still matches the full-size shot
            self.assertTrue(shots.compare(shots.half(*p), p)['ok'], shots.compare(shots.half(*p), p))

    def test_small_changes_match(self):
        r = random.Random(8)
        p = rand_pic(r, 640, 480, smooth=True)
        # noise of a few levels everywhere (another resampler, dithering)
        noisy = (640, 480, bytes(max(0, min(255, v + r.randrange(-4, 5))) for v in p[2]))
        self.assertTrue(shots.compare(p, noisy)['ok'])
        # a blinking cursor: one cell
        self.assertTrue(shots.compare(p, paint(p, 600, 440, 620, 460, (255, 255, 255)))['ok'])

    def test_real_differences_fail(self):
        r = random.Random(9)
        p = rand_pic(r, 640, 480, smooth=True)
        black = (640, 480, bytes(640 * 480 * 3))
        for name, q in [('black', black),
                        ('a missing layer (a sixth of the screen)', paint(p, 0, 400, 640, 480, (0, 0, 0))),
                        ('wrong colours', (640, 480, bytes(v ^ 0x80 for v in p[2]))),
                        ('red and blue swapped', (640, 480, bytes(p[2][i - i % 3 + (2 - i % 3)] for i in range(len(p[2]))))),
                        ('another screen', rand_pic(random.Random(10), 640, 480, smooth=True))]:
            res = shots.compare(p, q)
            self.assertFalse(res['ok'], name)
            self.assertTrue(res['why'], name)

    def test_size(self):
        r = random.Random(11)
        a, b = rand_pic(r, 640, 480, True), rand_pic(r, 800, 450, True)
        res = shots.compare(a, b)
        self.assertFalse(res['ok'])
        self.assertIn('size', res['why'])
        # same aspect, other size (fullscreen on one side): compared by content
        self.assertTrue(shots.compare(a, _double(a))['ok'])

    def test_symmetric_property(self):
        r = random.Random(12)
        for _ in range(8):
            a = rand_pic(r, 320, 240, True)
            b = paint(a, r.randrange(300), r.randrange(220), 320, 240, (r.randrange(256),) * 3)
            self.assertEqual(shots.compare(a, b)['ok'], shots.compare(b, a)['ok'])


def _double(p):
    w, h, rgb = p
    out = bytearray()
    for y in range(h):
        row = b''.join(rgb[(y * w + x) * 3:(y * w + x) * 3 + 3] * 2 for x in range(w))
        out += row + row
    return 2 * w, 2 * h, bytes(out)


if __name__ == '__main__':
    unittest.main()
