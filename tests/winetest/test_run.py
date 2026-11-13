"""run.py's reading of a test's output: python3 -m unittest tests/winetest/test_run.py"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
sys.dont_write_bytecode = True
import run  # noqa: E402

SUM = "{pid}:virtual: {n} tests executed (0 marked as todo, 0 as flaky, {f} failures), 0 skipped."


class Parse(unittest.TestCase):
    def status(self, *lines):
        return run.parse("virtual", "\n".join(lines) + "\n", False, 0)

    def test_finished_test_is_ok_or_fail(self):
        self.assertEqual(self.status(SUM.format(pid="0010", n=5, f=0))["status"], "ok")
        r = self.status("virtual.c:1: Test failed: x", SUM.format(pid="0010", n=5, f=1))
        self.assertEqual((r["status"], r["failures"], r["executed"]), ("fail", 1, 5))

    def test_a_childs_summary_does_not_finish_a_crashed_test(self):
        r = self.status(
            "virtual.c:174: Test failed: x",
            "0010:virtual: unhandled exception c0000005 at 7FEF01B2",
            SUM.format(pid="10010", n=0, f=0),
        )
        self.assertEqual(r["status"], "crash")
        self.assertEqual(r["failures"], 1)

    def test_childrens_counts_add_to_the_parents(self):
        r = self.status(SUM.format(pid="10010", n=3, f=1), SUM.format(pid="0010", n=5, f=0))
        self.assertEqual((r["status"], r["executed"], r["failures"]), ("fail", 8, 1))

    def test_output_without_pids(self):
        self.assertEqual(self.status("virtual: 2 tests executed (0 marked as todo, 0 failures), 0 skipped.")["status"], "ok")


if __name__ == "__main__":
    unittest.main()
