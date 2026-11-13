#!/usr/bin/env python3
"""tests/corpus-prep.sh on a zip holding an LHa self-extractor (痴漢車両2号車R's trial
discovery_0037t.zip: ChikanSyaryou2TaikenData.exe carries the game folder, lsar fails on it):
prep unpacks it with 7z next to itself, mends and converts its Shift-JIS names as for an outer
archive, and picks the game, not the extractor. Also an extractor without programs (stays), a
damaged one (stays, nothing half-unpacked left), and a seeded property run: any tree of names
(Shift-JIS with 0x5C trail bytes among them) and contents comes out whole under its UTF-8 names.
Needs 7z (p7zip-full) and convmv, as prep."""
import os, pathlib, random, shutil, struct, subprocess, tempfile, unittest, zipfile

HERE = pathlib.Path(__file__).resolve().parent
STUB = b"MZ" + bytes(30000)


def crc16(d):
    c = 0
    for b in d:
        c ^= b
        for _ in range(8):
            c = (c >> 1) ^ 0xA001 if c & 1 else c >> 1
    return c


def lzh(members):
    """A level-0 LHa archive of stored (-lh0-) members, names with Windows separators."""
    out = b""
    for name, data in members:
        n = name.replace("/", "\\").encode("cp932")
        body = b"-lh0-" + struct.pack("<IIIBBB", len(data), len(data), 0x4d210000, 0x20, 0, len(n)) + n + struct.pack("<H", crc16(data))
        out += bytes([len(body), sum(body) & 0xff]) + body + data
    return out + b"\0"


def prep(tmp, members):
    z = os.path.join(tmp, "trial.zip")
    with zipfile.ZipFile(z, "w") as w:
        for name, data in members.items():
            w.writestr(name, data)
    env = dict(os.environ, TRIALS=os.path.join(tmp, "trials"))
    r = subprocess.run(["bash", str(HERE / "corpus-prep.sh"), "t", "file://" + z, "", ""], capture_output=True, text=True, env=env)
    return r, pathlib.Path(tmp, "trials", "t", "x")


@unittest.skipUnless(shutil.which("7z") and shutil.which("convmv"), "7z, convmv")
class LzhSfx(unittest.TestCase):
    def test_game_inside_the_extractor(self):
        game = b"MZ" + bytes(9000)
        sfx = STUB + lzh([("Chikan/Chikan.exe", game), ("Chikan/graphic.arc", b"g" * 700), ("Chikan/十六夜.txt", b"izayoi")])
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"d/TaikenData.exe": sfx})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "d/TaikenData.exe.lzh/Chikan/Chikan.exe")
            self.assertEqual((x / "d/TaikenData.exe.lzh/Chikan/Chikan.exe").read_bytes(), game)
            self.assertEqual((x / "d/TaikenData.exe.lzh/Chikan/graphic.arc").read_bytes(), b"g" * 700)
            # (十 = 8F 5C: 7z split the name there; mended and converted to UTF-8)
            self.assertEqual((x / "d/TaikenData.exe.lzh/Chikan/十六夜.txt").read_bytes(), b"izayoi")
            self.assertFalse((x / "d/TaikenData.exe").exists())

    def test_extractor_without_programs_stays(self):
        exe = STUB + lzh([("readme.txt", b"hello")])
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"game/start.exe": exe})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "game/start.exe")
            self.assertFalse((x / "game/start.exe.lzh").exists())

    def test_damaged_extractor_stays(self):
        a = bytearray(lzh([("G/Game.exe", b"MZ" + bytes(70000))]))
        a[60:-10] = b"\xff" * (len(a) - 70)  # the member's data overwritten: its CRC fails
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"t/sfx.exe": STUB + bytes(a)})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "t/sfx.exe")
            self.assertFalse((x / "t/sfx.exe.lzh").exists())

    def test_any_tree_comes_out_whole(self):
        # (十ソ表 have a 0x5C trail byte in Shift-JIS)
        parts = ["Game", "data", "体験版", "十六夜", "ソフト", "表示", "セーブ", "a b", "x1"]
        for seed in range(8):
            rnd = random.Random(seed)
            members = {}
            while len(members) < rnd.randint(2, 6):
                d = "/".join(rnd.choice(parts) for _ in range(rnd.randint(0, 2)))
                name = (d + "/" if d else "") + rnd.choice(parts) + rnd.choice([".dat", ".txt", ".arc"])
                if any(m.startswith(name + "/") or name.startswith(m + "/") for m in members):
                    continue
                members[name] = bytes(rnd.randrange(256) for _ in range(rnd.randint(0, 300)))
            members["Game/Game.exe"] = b"MZ" + bytes(rnd.randint(100, 2000))
            with tempfile.TemporaryDirectory() as tmp, self.subTest(seed=seed):
                r, x = prep(tmp, {"s.exe": STUB + lzh(list(members.items()))})
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout.strip().splitlines()[-1], "s.exe.lzh/Game/Game.exe")
                got = {str(p.relative_to(x / "s.exe.lzh")): p.read_bytes() for p in (x / "s.exe.lzh").rglob("*") if p.is_file()}
                self.assertEqual(got, members)


if __name__ == "__main__":
    unittest.main()
