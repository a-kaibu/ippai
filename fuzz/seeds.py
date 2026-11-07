#!/usr/bin/env python3
"""Writes the fuzz targets' starting corpus to fuzz/seeds/<target>/ (not in git: made where
the fuzzers run). x86: real compiler output (gcc -m32; needs gcc-multilib), images: small
BMP/PNG (and JPEG when ImageMagick is there), acm: IMA / MS ADPCM blocks.

    python3 -I fuzz/seeds.py
"""
import os, shutil, struct, subprocess, sys, tempfile, zlib

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "seeds")

C = r"""
int sum(int*a,int n){int s=0;for(int i=0;i<n&&i<64;i++)s+=a[i]*3^i;return s;}
unsigned fnv(const unsigned char*p,int n){unsigned h=2166136261u;n&=63;while(n--){h^=*p++;h*=16777619u;}return h;}
double dot(double*a,float*b,int n){double s=0;for(int i=0;i<(n&31);i++)s+=a[i]*b[i];return s;}
long long mul(long long a,long long b){return a*b/(b|1);}
unsigned long long shl(unsigned long long a,int n){return (a<<(n&63))|(a>>(n&7));}
int sel(int a,int b){return a<b?a-b:(a>>3)|b;}
int sdiv(int a,int b){return b?a/b+a%b:a;}
void blend(unsigned char*d,const unsigned char*s,int n,int al){for(int i=0;i<(n&127);i++)d[i]=(s[i]*al+d[i]*(255-al))>>8;}
float lerp(float a,float b,float t){return a+(b-a)*t;}
int cvt(double x){return (int)x+(int)(float)x;}
unsigned bits(unsigned x){return __builtin_popcount(x)+__builtin_ctz(x|1)+__builtin_clz(x|1);}
void copy(char*d,const char*s,int n){while(n-->0&&*s)*d++=*s++;*d=0;}
"""

STATE_PTRS = 0xEF  # every register but ESP an address in the data window


def x86():
    out = []
    tmp = tempfile.mkdtemp()
    src = os.path.join(tmp, "s.c")
    open(src, "w").write(C)
    for opt in ["-O0", "-O1", "-O2", "-O2 -msse2 -mfpmath=sse", "-O3 -msse4.1 -mfpmath=sse", "-Os"]:
        obj = os.path.join(tmp, "s.o")
        r = subprocess.run(["gcc", "-m32", "-fno-pic", "-ffunction-sections", "-fno-asynchronous-unwind-tables", *opt.split(), "-c", src, "-o", obj])
        if r.returncode != 0:
            print("gcc -m32 failed (gcc-multilib?): no x86 seeds", file=sys.stderr)
            return []
        secs = subprocess.run(["objdump", "-h", obj], capture_output=True, text=True).stdout
        for line in secs.splitlines():
            p = line.split()
            if len(p) > 1 and p[1].startswith(".text."):
                b = os.path.join(tmp, "f.bin")
                subprocess.run(["objcopy", "-O", "binary", "-j", p[1], obj, b], check=True)
                out.append(open(b, "rb").read())
    shutil.rmtree(tmp)
    return out


def state(n, seed):
    # 8 registers, 64 bytes of XMM/MMX values (the last byte: which registers are addresses)
    regs = b"".join(struct.pack("<I", (seed * 0x9E3779B1 + i * 0x1234567) & 0xFFFFFFFF) for i in range(8))
    vec = bytes((seed * 7 + i * 13) & 0xFF for i in range(63))
    return regs + vec + bytes([STATE_PTRS])[:n]


def bmp(w, h, bpp):
    stride = (w * bpp + 31) // 32 * 4
    pal = b"".join(bytes([i, 255 - i, i // 2, 0]) for i in range(256)) if bpp == 8 else b""
    off = 54 + len(pal)
    rows = b"".join(bytes((x * 31 + y * 17) & 0xFF for x in range(stride)) for y in range(abs(h)))
    hdr = b"BM" + struct.pack("<IHHI", off + len(rows), 0, 0, off)
    info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, bpp, 0, len(rows), 2835, 2835, 0, 0)
    return hdr + info + pal + rows


def png(w, h, color, depth=8):
    ch = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color]
    raw = b"".join(b"\0" + bytes((x * 29 + y * 7) & 0xFF for x in range(w * ch * depth // 8)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    body = chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, color, 0, 0, 0))
    if color == 3:
        body += chunk(b"PLTE", bytes(range(48)))
    return b"\x89PNG\r\n\x1a\n" + body + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def images():
    out = [bmp(5, 3, 24), bmp(4, 4, 32), bmp(7, 2, 8), bmp(3, -2, 24)]
    out += [png(3, 2, 0), png(4, 3, 2), png(2, 2, 6), png(3, 3, 4), png(5, 1, 3), png(2, 2, 2, 16)]
    if shutil.which("convert"):
        tmp = tempfile.mkdtemp()
        for i, spec in enumerate(["gradient:red-blue", "plasma:", "xc:gray"]):
            f = os.path.join(tmp, f"{i}.jpg")
            if subprocess.run(["convert", "-size", "16x8", spec, f], capture_output=True).returncode == 0:
                out.append(open(f, "rb").read())
        f = os.path.join(tmp, "g.jpg")
        if subprocess.run(["convert", "-size", "8x8", "gradient:", "-colorspace", "Gray", f], capture_output=True).returncode == 0:
            out.append(open(f, "rb").read())
        shutil.rmtree(tmp)
    return out


def acm():
    out = []
    for ch in (1, 2):
        # IMA: 4-byte header per channel, then 4 bytes per channel per 8 samples
        spb = 1 + 8 * 4
        ima = b"".join(struct.pack("<hBB", 100 * c, 20, 0) for c in range(ch)) + bytes((i * 37) & 0xFF for i in range(16 * ch))
        out.append(bytes([0x80 | (ch - 1)]) + struct.pack("<H", spb) + ima)
        # MS ADPCM: predictor, delta, sample1, sample2 per channel, then nibbles
        spb = 2 + 32
        ms = bytes(c % 7 for c in range(ch)) + b"".join(struct.pack("<h", 16) for _ in range(ch)) + b"".join(struct.pack("<h", 50 * c) for c in range(2 * ch))
        ms += bytes((i * 53) & 0xFF for i in range(16 * ch))
        out.append(bytes([ch - 1]) + struct.pack("<H", spb) + ms)
    return out


def write(target, items):
    d = os.path.join(ROOT, target)
    os.makedirs(d, exist_ok=True)
    for i, b in enumerate(items):
        open(os.path.join(d, f"seed{i:03}"), "wb").write(b)
    print(f"{target}: {len(items)} seeds")


code = x86()
write("x86_jit", [state(96, i) + c[:1024] for i, c in enumerate(code)])
write("x86_interp", [state(32, i) + bytes([0x0F, 0x93]) + c for i, c in enumerate(code)])
write("image", images())
write("acm", acm())


def vorbis():
    # the test streams (several channel counts, rates, qualities), cut short: the fuzzer
    # mutates headers and the first audio packets
    d = os.path.join(os.path.dirname(ROOT), "..", "crates", "vorbis", "testdata")
    return [open(os.path.join(d, f), "rb").read()[:16384] for f in sorted(os.listdir(d)) if f.endswith(".ogg")]


write("vorbis", vorbis())
