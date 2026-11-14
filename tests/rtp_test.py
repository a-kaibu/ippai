#!/usr/bin/env python3
"""tools/ci/sake-rtp.py (ippai's own RPG Maker RTPs, docs/rtp.md): every manifest picture comes
out as a PNG of its size; the parts the engine reads by position are where it reads them (the
window skin's text colors, the empty first icon and tile B, the character sheets' frames); the
same name makes the same bytes; random manifest-like inputs never fail. No oggenc or fonts
needed: the sounds' samples are checked before encoding."""
import importlib.util, os, random, struct, sys, tempfile, unittest, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('sake_rtp', os.path.join(HERE, '..', 'tools', 'ci', 'sake-rtp.py'))
rtp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rtp)


def slurp(path):
    with open(path, 'rb') as f:
        return f.read()


def read_png(path):
    """(width, height, RGBA rows) of an 8-bit RGBA PNG with filter 0 rows (what Image.png writes)."""
    d = slurp(path)
    assert d[:8] == b'\x89PNG\r\n\x1a\n'
    p, idat = 8, b''
    w = h = 0
    while p < len(d):
        n, t = struct.unpack('>I4s', d[p:p + 8])
        body = d[p + 8:p + 8 + n]
        assert zlib.crc32(t + body) & 0xffffffff == struct.unpack('>I', d[p + 8 + n:p + 12 + n])[0]
        if t == b'IHDR':
            w, h, depth, ctype = struct.unpack('>IIBB', body[:10])
            assert (depth, ctype) == (8, 6)
        elif t == b'IDAT':
            idat += body
        p += 12 + n
    raw = zlib.decompress(idat)
    stride = 1 + 4 * w
    assert len(raw) == stride * h
    return w, h, [raw[y * stride + 1:(y + 1) * stride] for y in range(h)]


def pixel(rows, x, y):
    return tuple(rows[y][4 * x:4 * x + 4])


class RtpTest(unittest.TestCase):
    def test_every_picture_has_its_size(self):
        rows = list(rtp.manifest('rpgvxace'))
        pics = [r for r in rows if r[0].lower().endswith('.png')]
        self.assertGreater(len(pics), 400)
        self.assertTrue(all(len(r) == 3 for r in pics), 'pictures have width and height')
        self.assertTrue(all(r[0].lower().endswith('.ogg') for r in rows if len(r) == 1))
        with tempfile.TemporaryDirectory() as tmp:
            for path, w, h in pics:
                out = os.path.join(tmp, 'a.png')
                rtp.draw(path, int(w), int(h)).png(out)
                pw, ph, _ = read_png(out)
                self.assertEqual((pw, ph), (int(w), int(h)), path)

    def test_window_skin_layout(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, 'w.png')
            rtp.draw('Graphics/System/Window.png', 128, 128).png(out)
            _, _, rows = read_png(out)
        # text color n: the middle of cell n of the 8x4 grid at (64, 96)
        for n, c in enumerate(rtp.TEXT_COLORS):
            self.assertEqual(pixel(rows, 64 + n % 8 * 8 + 4, 96 + n // 8 * 8 + 4), c + (255,), n)
        self.assertEqual(pixel(rows, 68, 100)[:3], (255, 255, 255), 'normal text is white')
        self.assertEqual(pixel(rows, 32, 32)[3], 255, 'the back is opaque')

    def test_empty_first_icon_and_tile(self):
        with tempfile.TemporaryDirectory() as tmp:
            for path, w, h, cell in (('Graphics/System/IconSet.png', 384, 936, 24), ('Graphics/Tilesets/World_B.png', 512, 512, 32)):
                out = os.path.join(tmp, 'a.png')
                rtp.draw(path, w, h).png(out)
                _, _, rows = read_png(out)
                self.assertTrue(all(pixel(rows, x, y)[3] == 0 for x in range(cell) for y in range(cell)), path)

    def test_character_frames_are_drawn_in_each_cell(self):
        with tempfile.TemporaryDirectory() as tmp:
            for path, w, h, cols, rows_ in (('Graphics/Characters/Actor1.png', 384, 256, 12, 8), ('Graphics/Characters/$BigMonster1.png', 288, 512, 3, 4)):
                out = os.path.join(tmp, 'a.png')
                rtp.draw(path, w, h).png(out)
                _, _, rows = read_png(out)
                fw, fh = w // cols, h // rows_
                for cx in range(cols):
                    for cy in range(rows_):
                        opaque = sum(pixel(rows, x, y)[3] == 255 for x in range(cx * fw, (cx + 1) * fw) for y in range(cy * fh, (cy + 1) * fh))
                        self.assertGreater(opaque, fw * fh // 10, f'{path} frame {cx},{cy}')
                        # (the frame's corner stays clear: frames don't run into each other)
                        self.assertEqual(pixel(rows, cx * fw, cy * fh)[3], 0)

    def test_same_name_same_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = os.path.join(tmp, 'a.png'), os.path.join(tmp, 'b.png')
            rtp.draw('Graphics/Faces/Actor1.png', 384, 192).png(a)
            rtp.draw('Graphics/Faces/Actor1.png', 384, 192).png(b)
            self.assertEqual(slurp(a), slurp(b))
            rtp.draw('Graphics/Faces/Actor2.png', 384, 192).png(b)
            self.assertNotEqual(slurp(a), slurp(b))
        self.assertEqual(rtp.tone('Audio/SE/Cursor1.ogg'), rtp.tone('Audio/SE/Cursor1.ogg'))

    def test_sound_lengths(self):
        for path, lo, hi in (('Audio/BGM/Town1.ogg', 7.9, 8.1), ('Audio/BGS/Rain.ogg', 3.9, 4.1), ('Audio/ME/Fanfare1.ogg', 2.9, 3.1), ('Audio/SE/Cursor1.ogg', 0.2, 0.55)):
            pcm = rtp.tone(path)
            self.assertTrue(lo <= len(pcm) / 2 / rtp.RATE <= hi, path)
            peak = max(abs(v) for (v,) in struct.iter_unpack('<h', pcm))
            self.assertTrue(1000 < peak <= 32000, f'{path}: audible, not clipped ({peak})')

    def test_random_inputs(self):
        """Property: any folder, name and size from 1 to 600 makes a PNG of that size."""
        r = random.Random(155)
        folders = ['Characters', 'Faces', 'Battlers', 'Animations', 'Tilesets', 'System', 'Battlebacks2', 'Titles2', 'Parallaxes', 'Titles1', 'Pictures']
        names = ['Window', 'IconSet', 'Balloon', 'Shadow', 'GameOver', 'BattleStart', 'A_B', '!$x', '$y', '!z', 'Inside_A2', 'q']
        with tempfile.TemporaryDirectory() as tmp:
            for _ in range(150):
                w, h = r.randint(1, 600), r.randint(1, 600)
                path = f'Graphics/{r.choice(folders)}/{r.choice(names)}.png'
                out = os.path.join(tmp, 'a.png')
                rtp.draw(path, w, h).png(out)
                self.assertEqual(read_png(out)[:2], (w, h), path)


if __name__ == '__main__':
    unittest.main()
