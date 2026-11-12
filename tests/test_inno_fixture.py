#!/usr/bin/env python3
"""Fault-inject a real ISCC-built fixture: test_inno_fixture.py SETUP.EXE."""
import importlib.util
import pathlib
import struct
import sys
import unittest
import zlib

spec = importlib.util.spec_from_file_location("inno_fixture", pathlib.Path(__file__).with_name("check-inno-fixture.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
DATA = pathlib.Path(sys.argv.pop(1)).read_bytes()


class InnoFixtureTest(unittest.TestCase):
    def test_real_compiler_output(self):
        fixture.check(DATA)

    def test_trailing_signature_is_allowed(self):
        fixture.check(DATA + bytes(128))

    def test_observed_four_byte_overlay_is_rejected(self):
        # The failed CI artifact ended four bytes after its PE sections, before
        # ISCC populated the loader table. Recreate this failure from a real EXE.
        at = DATA.index(fixture.TABLE_ID)
        fields = fixture.TABLE.unpack_from(DATA, at)
        broken = bytearray(DATA[:fields[7] + 4])  # Offset1: embedded files begin
        broken[at:at + fixture.TABLE.size] = bytes(fixture.TABLE.size)
        with self.assertRaisesRegex(ValueError, "missing completed"):
            fixture.check(broken)

    def test_truncated_payload_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "unfinished Inno payload"):
            fixture.check(DATA[:-1])

    def test_damaged_table_is_rejected(self):
        broken = bytearray(DATA)
        at = broken.index(fixture.TABLE_ID)
        broken[at + 32] ^= 1
        with self.assertRaisesRegex(ValueError, "CRC mismatch"):
            fixture.check(broken)

    def test_bad_bounds_with_valid_crc_are_rejected(self):
        broken = bytearray(DATA)
        at = broken.index(fixture.TABLE_ID)
        struct.pack_into("<q", broken, at + 24, len(DATA) + 1)  # OffsetEXE
        struct.pack_into("<I", broken, at + 60, zlib.crc32(broken[at:at + 60]))
        with self.assertRaisesRegex(ValueError, "unfinished Inno payload"):
            fixture.check(broken)


if __name__ == "__main__":
    unittest.main()
