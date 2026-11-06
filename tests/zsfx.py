#!/usr/bin/env python3
"""zsfx.py SETUP.EXE OUTDIR: unpacks a setup.exe that carries its files as zlib streams after
an INI header (Ver=, AppName=, FileCount=, FileN=name,dir,No,size,...; LsUninst.tmp first:
WILL's PrincessWaltz_WebTrial2.exe). One stream per file, in FileN order; each stream's size
has to match the header's. Prints the file names."""
import sys, os, re, zlib, mmap

def streams(m, pos):
    """(offset, data) of each zlib stream from pos on"""
    while True:
        i = m.find(b'\x78\xda', pos)
        if i < 0:
            return
        z = zlib.decompressobj()
        out, j = [], i
        try:
            while not z.eof:
                chunk = m[j:j + (1 << 22)]
                if not chunk:
                    break
                j += len(chunk)
                out.append(z.decompress(chunk))
        except zlib.error:
            pos = i + 2
            continue
        if not z.eof:
            return
        yield i, b''.join(out)
        pos = j - len(z.unused_data)

def main(path, out):
    f = open(path, 'rb')
    m = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    it = streams(m, 0)
    for _, head in it:
        if b'FileCount=' in head and b'\nFile0=' in head:
            break
    else:
        sys.exit('no file list')
    ini = head.decode('cp932')
    # sub folders made with MAKESUBDIR: their last path part
    sub = [v.rstrip('"').rsplit('\\', 1)[-1] for v in re.findall(r'^Val\d+=.*?MAKESUBDIR,"([^"]*)"', ini, re.M)]
    files = []
    for name, d, size in re.findall(r'^File\d+=([^,]*),([^,]*),[^,]*,(\d+),', ini, re.M):
        files.append((os.path.join(sub[0], name) if 'MAKESUBDIR' in d and sub else name, int(size)))
    for name, size in files:
        _, data = next(it)
        if len(data) != size:
            sys.exit(f'{name}: {len(data)} bytes, the list says {size}')
        p = os.path.join(out, name)
        os.makedirs(os.path.dirname(p) or out, exist_ok=True)
        open(p, 'wb').write(data)
        print(name, size)

main(*sys.argv[1:3])
