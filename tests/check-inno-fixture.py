#!/usr/bin/env python3
"""Check the pinned Inno 6.7.4 host-test fixture before running its installer.

ISCC has returned 0 with an unfinished PE + four-byte overlay (#101). Its final
loader table must exist, have a valid CRC, and describe data inside the file.
This checks structure, not compressed payload contents; innoinstall still runs
installation and uninstallation to verify those. The format is documented by
jrsoftware/issrc is-6_7_4, Projects/Src/Shared.Struct.pas (TSetupLdrOffsetTable).
"""
import pathlib
import struct
import sys
import zlib

TABLE_ID = b"rDlPtS\xcd\xe6\xd7\x7b\x0b\x2a"
TABLE = struct.Struct("<12sIqqIIqqII")


def check(data):
    def require(ok, message):
        if not ok:
            raise ValueError(message)

    def u16(at):
        return struct.unpack_from("<H", data, at)[0]

    def u32(at):
        return struct.unpack_from("<I", data, at)[0]

    require(data[:2] == b"MZ", "missing DOS signature")
    pe = u32(0x3c)
    require(data[pe:pe + 4] == b"PE\0\0", "missing PE signature")
    sections = pe + 24 + u16(pe + 20)
    image_end = 0
    resource = None
    for i in range(u16(pe + 6)):
        at = sections + 40 * i
        size, offset = u32(at + 16), u32(at + 20)
        require(offset + size <= len(data), "truncated PE section")
        image_end = max(image_end, offset + size)
        if data[at:at + 8].rstrip(b"\0") == b".rsrc":
            resource = data[offset:offset + size]
    require(resource is not None, "missing resource section")
    at = resource.find(TABLE_ID)
    require(at >= 0, "missing completed Inno loader table")
    require(resource.find(TABLE_ID, at + 1) < 0, "ambiguous Inno loader table")
    table = resource[at:at + TABLE.size]
    _, version, total, exe, raw_size, _, header, files, padding, crc = TABLE.unpack(table)
    require(version == 2 and padding == 0, "unexpected Inno loader table format")
    require(zlib.crc32(table[:-4]) == crc, "Inno loader table CRC mismatch")
    # This fixture embeds its files (DiskSpanning=no), followed by setup headers
    # and the compressed installer executable. Signatures may follow TotalSize.
    require(image_end <= files < header < exe < total <= len(data),
            f"unfinished Inno payload: file={len(data)} image={image_end} "
            f"files={files} headers={header} exe={exe} total={total}")
    require(raw_size > 0, "empty embedded installer executable")
    return f"file={len(data)} image={image_end} payload={total - image_end} table_crc={crc:08x}"


def main():
    try:
        print("Inno fixture: " + check(pathlib.Path(sys.argv[1]).read_bytes()))
    except (OSError, ValueError, struct.error) as e:
        sys.exit(f"invalid Inno fixture: {e}")


if __name__ == "__main__":
    main()
