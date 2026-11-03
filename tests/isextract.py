#!/usr/bin/env python3
"""isextract.py SETUP.EXE OUTDIR: unpacks an InstallShield setup.exe whose payload is an
"ISSetupStream" / "InstallShield" overlay (files obfuscated with a name-derived key, the
ISSetupStream ones also zlib-compressed). Prints the file names."""
import sys, struct, zlib, os
M = [0x13, 0x35, 0x86, 0x07]
# T[k]: byte b -> ~(k ^ (b rotated by 4)), as bytes.translate tables
T = [bytes((~(k ^ (((b << 4) | (b >> 4)) & 0xff))) & 0xff for b in range(256)) for k in range(256)]

def decode(buf, key, start):
    # the key restarts every 1024 bytes: each of the 1024 positions has one table
    kl = len(key)
    out = bytearray(buf)
    for j in range(min(1024, len(buf))):
        out[j::1024] = buf[j::1024].translate(T[key[j % kl]])
    return bytes(out)

def pe_end(f):
    f.seek(0); d = f.read(4096)
    pe = struct.unpack_from('<I', d, 0x3c)[0]
    ns = struct.unpack_from('<H', d, pe + 6)[0]; opt = struct.unpack_from('<H', d, pe + 20)[0]
    sec = pe + 24 + opt; end = 0
    for i in range(ns):
        raw, ptr = struct.unpack_from('<II', d, sec + 40 * i + 16); end = max(end, raw + ptr)
    return end

def main(path, out):
    f = open(path, 'rb')
    end = pe_end(f)
    f.seek(end); tail = f.read(4096)
    # (some setups have debug info between the image and the stream)
    at = min((i for i in (tail.find(b'ISSetupStream\0'), tail.find(b'InstallShield\0')) if i >= 0), default=0)
    f.seek(end + at); h = f.read(46)
    sig = h[:14]
    n = struct.unpack_from('<H', h, 14)[0]
    if sig == b'ISSetupStream\0':
        ver = struct.unpack_from('<H', h, 16)[0]
        for _ in range(n):
            if ver >= 4:  # (newer setups: 64-bit sizes, 48-byte entry headers)
                nl, flags, _x, ln = struct.unpack('<IIHQ', f.read(18)); f.read(30)
            else:
                nl, flags, _x, ln = struct.unpack('<IIHI', f.read(14)); f.read(10)
            name = f.read(nl).decode('utf-16le')
            data = f.read(ln)
            if flags & 6:
                key = [c ^ M[i % 4] for i, c in enumerate(name.encode('utf-8'))]
                data = decode(data, key, 0)
            if data[:2] == b'x\x9c':
                data = zlib.decompress(data)
            elif flags & 4:
                print(f'  ({name}: flags {flags:#x}, starts {data[:8]!r}; left as is)', file=sys.stderr)
            save(out, name, data)
    elif sig == b'InstallShield\0':
        for _ in range(n):
            a = f.read(312)
            name = a[:260].split(b'\0')[0].decode('cp932')
            flags, _x, ln = struct.unpack_from('<III', a, 260)
            data = f.read(ln)
            if flags & 6:
                key = [c ^ M[i % 4] for i, c in enumerate(name.encode('cp932'))]
                data = decode(data, key, 0)
            if data[:2] == b'x\x9c':
                data = zlib.decompress(data)
            save(out, name, data)
    else:
        sys.exit('not an InstallShield stream setup')

def save(out, name, data):
    p = os.path.join(out, name.replace('\\', '/'))
    os.makedirs(os.path.dirname(p) or out, exist_ok=True)
    open(p, 'wb').write(data)
    print(name, len(data))

main(*sys.argv[1:3])
