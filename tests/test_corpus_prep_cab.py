#!/usr/bin/env python3
"""tests/corpus-prep.sh on a zip holding a cabinet self-extractor (騎士姫クラリス's trial,
#124: the exe unpacks its appended cabinet with cabinet.dll's FDI and quits, the game is
Game.exe inside it): prep unpacks the cabinet next to it and picks the game, not the extractor.
Also an exe that only carries a cabinet without programs (stays as it is), and a damaged
cabinet (the extractor stays, nothing half-unpacked is left). Needs 7z (p7zip-full), as prep."""
import os, pathlib, shutil, subprocess, sys, tempfile, unittest, zipfile

HERE = pathlib.Path(__file__).resolve().parent
ns = {}
exec((HERE / "winref/probes/fdi_gen.py").read_text().split("A_RDONLY")[0], ns)
cab, text = ns["cab"], ns["text"]
STUB = b"MZ" + bytes(4094)


def prep(tmp, members):
    z = os.path.join(tmp, "trial.zip")
    with zipfile.ZipFile(z, "w") as w:
        for name, data in members.items():
            w.writestr(name, data)
    env = dict(os.environ, TRIALS=os.path.join(tmp, "trials"))
    r = subprocess.run(["bash", str(HERE / "corpus-prep.sh"), "t", "file://" + z, "", ""], capture_output=True, text=True, env=env)
    return r, pathlib.Path(tmp, "trials", "t", "x")


@unittest.skipUnless(shutil.which("7z"), "7z")
class CabSfx(unittest.TestCase):
    def test_game_inside_the_extractor(self):
        game = b"MZ" + bytes(9000)
        sfx = STUB + cab([(1, [(b"Game.exe", game, 0x20), (b"System\\RGSS301.dll", b"MZ" + bytes(100), 0x20),
                               (b"Game.ini", text(150, 2), 0x20)])])
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"taiken/trial.exe": sfx})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "taiken/trial.exe.cab/Game.exe")
            self.assertEqual((x / "taiken/trial.exe.cab/Game.exe").read_bytes(), game)
            self.assertEqual((x / "taiken/trial.exe.cab/Game.ini").read_bytes(), text(150, 2))
            self.assertFalse((x / "taiken/trial.exe").exists())

    def test_cabinet_without_programs_stays(self):
        exe = STUB + bytes(9000) + cab([(0, [(b"readme.txt", b"hello", 0x20)])])
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"game/start.exe": exe})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "game/start.exe")
            self.assertFalse((x / "game/start.exe.cab").exists())

    def test_damaged_cabinet_leaves_the_extractor(self):
        c = bytearray(cab([(1, [(b"Game.exe", b"MZ" + bytes(70000), 0x20)])]))
        c[-200:] = bytes(200)  # the folder's last block zeroed
        with tempfile.TemporaryDirectory() as tmp:
            r, x = prep(tmp, {"t/sfx.exe": STUB + bytes(c)})
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip().splitlines()[-1], "t/sfx.exe")
            self.assertFalse((x / "t/sfx.exe.cab").exists())


if __name__ == "__main__":
    unittest.main()
