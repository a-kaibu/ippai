#!/usr/bin/env python3
"""Issue-backed quarantine never hides a new probe, crash, timeout or missing output."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import compare

HERE = Path(__file__).resolve().parent
ISSUE = "https://github.com/a-kaibu/ippai/issues/105"


class Quarantine(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.ref, self.got = self.root / "ref", self.root / "got"
        self.ref.mkdir()
        self.got.mkdir()
        self.quarantine = self.root / "quarantine.tsv"
        self.quarantine.write_text(f"old-probe-32\t{ISSUE}\tExisting compatibility gap.\n")

    def probe(self, name="old-probe-32", result="different\n", exit="0"):
        for root, text, code in [(self.ref, "Windows\n", "0"), (self.got, result, exit)]:
            d = root / name
            d.mkdir()
            if text is not None:
                (d / "stdout.txt").write_text(text)
            (d / "exit").write_text(code + "\n")

    def run_compare(self, quarantine=True):
        args = [sys.executable, str(HERE / "compare.py"), str(self.ref), str(self.got)]
        if quarantine:
            args += ["--quarantine", str(self.quarantine)]
        return subprocess.run(args, capture_output=True, text=True, check=False)

    def test_existing_gap_is_visible_and_linked(self):
        self.probe()
        self.assertEqual(self.run_compare(False).returncode, 1)
        p = self.run_compare()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("1 skipped", p.stdout)
        self.assertIn(ISSUE, p.stdout)
        self.assertIn("different", p.stdout)

    def test_new_probe_still_fails(self):
        self.probe("new-probe-32")
        self.assertEqual(self.run_compare().returncode, 1)

    def test_crashes_and_timeouts_still_fail(self):
        self.probe(exit="139")
        self.assertEqual(self.run_compare().returncode, 1)
        (self.got / "old-probe-32/exit").write_text("?\n")
        self.assertEqual(self.run_compare().returncode, 1)

    def test_missing_and_empty_output_still_fail(self):
        self.probe(result=None)
        self.assertEqual(self.run_compare().returncode, 1)
        (self.got / "old-probe-32/stdout.txt").write_text("")
        self.assertEqual(self.run_compare().returncode, 1)

    def test_fixed_probe_is_reported_as_passing_not_skipped(self):
        self.probe(result="Windows\n")
        p = self.run_compare()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("1 ok", p.stdout)
        self.assertIn("0 skipped", p.stdout)

    def test_issue_and_reason_are_required_and_blanket_globs_rejected(self):
        for line in ["*\t" + ISSUE + "\tgap", "old-probe-32\tTODO\tgap", "old-probe-32\t" + ISSUE + "\t"]:
            self.quarantine.write_text(line + "\n")
            with self.assertRaises(ValueError):
                compare.read_quarantine(self.quarantine)


if __name__ == "__main__":
    unittest.main()
