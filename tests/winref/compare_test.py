#!/usr/bin/env python3
"""Issue-backed quarantine never hides a new probe, crash, timeout or missing output."""
from pathlib import Path
import subprocess
import json
import sys
import tempfile
import unittest

import compare

HERE = Path(__file__).resolve().parent
ISSUE = "https://github.com/a-kaibu/masu/issues/105"


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
        self.quarantine.with_suffix(".json").write_text(json.dumps({
            "old-probe-32": compare.quarantine_lines(["want Windows", "got  different"], {}, {})
        }))

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

    def test_new_difference_in_quarantined_probe_fails(self):
        self.probe(result="different\nnew corruption\n")
        self.assertEqual(self.run_compare().returncode, 1)

    def test_partly_fixed_probe_still_passes_and_a_changed_line_fails(self):
        # (the accepted difference, with a line both share)
        d_ref, d_got = self.ref / "old-probe-32", self.got / "old-probe-32"
        self.probe(result="different\n")
        (d_ref / "stdout.txt").write_text("Windows\nshared\n")
        (d_got / "stdout.txt").write_text("different\nshared\n")
        self.assertEqual(self.run_compare().returncode, 0)
        # (a line accepted as "got  different" that now reads differently)
        (d_got / "stdout.txt").write_text("different again\nshared\n")
        p = self.run_compare()
        self.assertEqual(p.returncode, 1)
        self.assertIn("1 accepted by its quarantine", p.stdout)
        self.assertIn("different again", p.stdout)
        self.assertIn("quarantine candidates", p.stdout)

    def test_fewer_lines_than_accepted_are_skipped_not_failed(self):
        accepted = compare.quarantine_lines(["want Windows", "got  different", "want two", "got  deux"], {}, {})
        self.quarantine.with_suffix(".json").write_text(json.dumps({"old-probe-32": accepted}))
        self.probe(result="different\n")
        p = self.run_compare()
        self.assertEqual(p.returncode, 0, p.stdout)
        self.assertIn("1 skipped", p.stdout)

    def test_missing_snapshot_does_not_allow_blanket_skip(self):
        self.probe()
        self.quarantine.with_suffix(".json").unlink()
        self.assertEqual(self.run_compare().returncode, 1)

    def test_same_image_summary_does_not_hide_different_pixels(self):
        want = {"frame": (2, 1, 0, ["000000000000"])}
        old = {"frame": (2, 1, 0, ["ff0000010000"])}
        new = {"frame": (2, 1, 0, ["ff0000000100"])}
        summary = [compare.image_diff("frame", want["frame"], old["frame"], None, "p")]
        self.assertEqual(summary, [compare.image_diff("frame", want["frame"], new["frame"], None, "p")])
        self.assertNotEqual(compare.quarantine_lines(summary, want, old),
                            compare.quarantine_lines(summary, want, new))

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


class EnvironmentPolicy(unittest.TestCase):
    """Exercise the shipped exclusions with real Windows output and injected faults."""

    def test_only_observed_environment_differences_are_allowed(self):
        for name in ("outptr2-32", "outptr2-64", "eventlog-32", "eventlog-64"):
            with self.subTest(probe=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                ref, got = root / "ref" / name, root / "got" / name
                ref.mkdir(parents=True)
                got.mkdir(parents=True)
                windows = (HERE / "golden" / name / "stdout.txt").read_text()
                (ref / "stdout.txt").write_text(windows)
                (ref / "exit").write_text("0\n")
                actual = windows.replace("DirectSoundCreate: half 1", "DirectSoundCreate: half 0")
                actual = actual.replace("DirectSoundCreate8: half 1", "DirectSoundCreate8: half 0")
                actual = actual.replace(
                    "backup: has a record 1 same number 0 same length 1 same bytes 0",
                    "backup: has a record 1 same number 1 same length 1 same bytes 1",
                )
                for call in ("GetNumberOfEventLogRecords", "BackupEventLogA"):
                    actual = actual.replace(f"{call}(NULL, NULL): 0 6", f"{call}(NULL, NULL): 0 87")

                def check(output, expected, code="0"):
                    if output is None:
                        (got / "stdout.txt").unlink(missing_ok=True)
                    else:
                        (got / "stdout.txt").write_text(output)
                    (got / "exit").write_text(code + "\n")
                    p = subprocess.run([
                        sys.executable, str(HERE / "compare.py"), str(ref.parent), str(got.parent),
                        "--known", str(HERE / "known.txt"),
                        "--quarantine", str(HERE / "quarantine.tsv"),
                    ], capture_output=True, text=True, check=False)
                    self.assertEqual(p.returncode, expected, p.stdout + p.stderr)
                    self.assertIn("0 skipped", p.stdout)

                check(windows, 0)
                check(actual, 0)
                lines = actual.splitlines(keepends=True)
                for i, line in enumerate(lines):
                    if line.startswith("DirectSoundCreate"):
                        mutated = line.replace("half 0", "half 2")
                    else:
                        mutated = line.rstrip("\n") + " unexpected\n"
                    with self.subTest(line=line.strip()):
                        check("".join(lines[:i] + [mutated] + lines[i + 1:]), 1)
                check(actual, 1, "139")
                check(actual, 1, "?")
                check("", 1)
                check(None, 1)


if __name__ == "__main__":
    unittest.main()
