#!/usr/bin/env python3
"""The node harness isolates probes, including simultaneous JIT/interpreter passes."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HARNESS = Path(__file__).with_name("run.sh").resolve()


class Prefixes(unittest.TestCase):
    def test_each_probe_and_pass_has_its_own_prefix(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            exe, tools = root / "exe", root / "bin"
            exe.mkdir()
            tools.mkdir()
            for name in ["probe-32.exe", "probe-64.exe"]:
                (exe / name).touch()
            node = tools / "node"
            node.write_text('#!/bin/sh\nprintf "%s\\n" "$SAKE_C"\n')
            node.chmod(0o755)
            env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", SAKE="unused.js")
            env.pop("SAKE_C", None)
            runs = [subprocess.Popen(["bash", str(HARNESS), str(exe), str(root / mode)], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE) for mode in ["jit", "interp"]]
            for run in runs:
                stdout, stderr = run.communicate(timeout=10)
                self.assertEqual(run.returncode, 0, (stdout, stderr))
            prefixes = []
            for mode in ["jit", "interp"]:
                for probe in ["probe-32", "probe-64"]:
                    out = root / mode / probe
                    self.assertEqual((out / "exit").read_text().strip(), "0")
                    prefix = Path((out / "stdout.txt").read_text().strip())
                    self.assertEqual(prefix, out / "drive_c")
                    self.assertTrue((prefix / "windows/temp").is_dir())
                    prefixes.append(prefix)
            self.assertEqual(len(set(prefixes)), 4)


if __name__ == "__main__":
    unittest.main()
