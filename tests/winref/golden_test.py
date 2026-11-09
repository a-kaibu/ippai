#!/usr/bin/env python3
"""Tests of tests/winref/golden.py's import: Windows' answers replace the golden, except a
failed run of an unchanged probe whose answer had exited 0 (the runner crashed: the answer
stays); a changed probe, or one that never exited 0, takes what Windows says.

  python3 tests/winref/golden_test.py
"""
import contextlib, io, os, shutil, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import golden

PROBE = "d3d9state-32"


class Import(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.saved = golden.GOLDEN
        golden.GOLDEN = os.path.join(self.tmp, "golden")

    def tearDown(self):
        golden.GOLDEN = self.saved
        shutil.rmtree(self.tmp)

    def run_(self, text, exit):
        d = os.path.join(self.tmp, "ref", PROBE)
        os.makedirs(d, exist_ok=True)
        open(os.path.join(d, "stdout.txt"), "w").write(text)
        open(os.path.join(d, "exit"), "w").write(exit + "\n")

    def import_(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            golden.import_(os.path.join(self.tmp, "ref"), set())
        return out.getvalue().split(), err.getvalue()

    def answer(self):
        g = os.path.join(golden.GOLDEN, PROBE)
        return open(os.path.join(g, "stdout.txt")).read(), open(os.path.join(g, "exit")).read().strip()

    def test_a_first_answer_is_taken_even_a_crash(self):
        self.run_("half\n", "139")
        self.assertEqual(self.import_()[0], [PROBE])
        self.assertEqual(self.answer(), ("half\n", "139"))

    def test_a_crash_of_the_same_probe_keeps_the_answer(self):
        self.run_("all\nlines\n", "0")
        self.import_()
        self.run_("all\n", "139")
        took, said = self.import_()
        self.assertEqual(took, [])
        self.assertIn("exit 139", said)
        self.assertEqual(self.answer(), ("all\nlines\n", "0"))

    def test_a_changed_probe_takes_the_crash(self):
        self.run_("all\nlines\n", "0")
        self.import_()
        open(os.path.join(golden.GOLDEN, PROBE, "src"), "w").write("older sources\n")
        self.run_("all\n", "139")
        self.assertEqual(self.import_()[0], [PROBE])
        self.assertEqual(self.answer(), ("all\n", "139"))

    def test_a_probe_that_failed_before_takes_the_new_answer(self):
        self.run_("x\n", "3")
        self.import_()
        self.run_("y\n", "139")
        self.assertEqual(self.import_()[0], [PROBE])
        self.assertEqual(self.answer(), ("y\n", "139"))

    def test_a_good_run_replaces_a_good_answer(self):
        self.run_("old\n", "0")
        self.import_()
        self.run_("new\n", "0")
        self.assertEqual(self.import_()[0], [PROBE])
        self.assertEqual(self.answer(), ("new\n", "0"))


if __name__ == "__main__":
    unittest.main()
