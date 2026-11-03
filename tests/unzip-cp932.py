#!/usr/bin/env python3
"""unzip-cp932.py ZIP DIR: extracts a zip (also a self-extracting .exe) whose names are
Shift-JIS without the UTF-8 flag, as Japanese zips are."""
import os
import sys
import zipfile

src, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(src) as z:
    for i in z.infolist():
        name = i.filename
        if not i.flag_bits & 0x800:
            try:
                name = name.encode('cp437').decode('cp932')
            except UnicodeError:
                pass
        dst = os.path.normpath(os.path.join(out, name.replace('\\', '/')))
        if not dst.startswith(os.path.normpath(out) + os.sep):
            continue
        if name.endswith(('/', '\\')):
            os.makedirs(dst, exist_ok=True)
            continue
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with z.open(i) as f, open(dst, 'wb') as g:
            while chunk := f.read(1 << 20):
                g.write(chunk)
