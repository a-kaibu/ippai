#!/usr/bin/env python3
"""Screenshots of corpus titles: PNG in and out, halving, and the comparison of a sake shot
with Windows' (tests/corpus/golden). Standard library only (the Windows runner and check.py).

  shots.py compare WANT.png GOT.png        prints the comparison, exit 1 when it differs
  shots.py half IN.png OUT.png             a golden from a Windows shot (half size)

Pictures are (w, h, rgb): rgb is w*h*3 bytes, rows top down.

The comparison is meant for "is this the same screen, drawn the same way": both pictures go to a
grid of CELLS_X x CELLS_Y cells (area averages, so the halved golden and a full-size shot meet),
and a cell differs when its average colour is more than CELL_TOL away in any channel. They
match when the size (aspect) agrees, the mean difference over all cells is below MEAN_TOL and
at most BAD_MAX of the cells differ. Animation, a blinking cursor or a different text speed
moves a few cells; a missing layer, wrong colours, a black screen or another screen move many.
"""
import struct, sys, zlib

CELLS_X, CELLS_Y = 32, 24
CELL_TOL = 24
MEAN_TOL = 6.0
BAD_MAX = 0.04


def _chunks(b):
    if b[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('not a PNG')
    i = 8
    while i + 8 <= len(b):
        n, t = struct.unpack('>I4s', b[i:i + 8])
        yield t, b[i + 8:i + 8 + n]
        i += 12 + n


def _paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else b if pb <= pc else c


def decode(b):
    """PNG bytes -> (w, h, rgb). 8-bit RGB or RGBA (alpha dropped), not interlaced: what
    corpus.mjs and winplay.py write (filter 0 rows, the fast path) and what an image tool writes."""
    ihdr, idat = None, []
    for t, d in _chunks(b):
        if t == b'IHDR': ihdr = d
        elif t == b'IDAT': idat.append(d)
        elif t == b'IEND': break
    if ihdr is None: raise ValueError('no IHDR')
    w, h, depth, ctype, _, _, inter = struct.unpack('>IIBBBBB', ihdr)
    if depth != 8 or ctype not in (2, 6) or inter:
        raise ValueError(f'unsupported PNG: depth {depth} colour type {ctype} interlace {inter}')
    bpp = 3 if ctype == 2 else 4
    raw = zlib.decompress(b''.join(idat))
    stride = w * bpp
    if len(raw) < (stride + 1) * h: raise ValueError('short image data')
    rows, prev = [], bytes(stride)
    for y in range(h):
        o = y * (stride + 1)
        f, line = raw[o], raw[o + 1:o + 1 + stride]
        if f == 0:
            cur = line
        else:
            cur = bytearray(line)
            if f == 1:
                for i in range(bpp, stride): cur[i] = (cur[i] + cur[i - bpp]) & 255
            elif f == 2:
                for i in range(stride): cur[i] = (cur[i] + prev[i]) & 255
            elif f == 3:
                for i in range(stride): cur[i] = (cur[i] + (((cur[i - bpp] if i >= bpp else 0) + prev[i]) >> 1)) & 255
            elif f == 4:
                for i in range(stride):
                    a = cur[i - bpp] if i >= bpp else 0
                    c = prev[i - bpp] if i >= bpp else 0
                    cur[i] = (cur[i] + _paeth(a, prev[i], c)) & 255
            else:
                raise ValueError(f'bad filter {f} in row {y}')
            cur = bytes(cur)
        rows.append(cur)
        prev = cur
    if bpp == 4:
        rows = [bytes(r[i] for i in range(len(r)) if i % 4 != 3) for r in rows]
    return w, h, b''.join(rows)


def encode(w, h, rgb, level=9):
    """(w, h, rgb) -> PNG bytes (filter 0 rows: decode's fast path)"""
    if len(rgb) != w * h * 3: raise ValueError('size')
    s = w * 3
    raw = b''.join(b'\0' + rgb[y * s:(y + 1) * s] for y in range(h))
    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) +
            chunk(b'IDAT', zlib.compress(raw, level)) + chunk(b'IEND', b''))


def read(path):
    with open(path, 'rb') as f: return decode(f.read())


def write(path, w, h, rgb):
    with open(path, 'wb') as f: f.write(encode(w, h, rgb))


def half(w, h, rgb):
    """2x2 box average (rounded), odd last row/column dropped"""
    W, H, s = w // 2, h // 2, w * 3
    out = bytearray(W * H * 3)
    for y in range(H):
        a, b = rgb[2 * y * s:(2 * y + 1) * s], rgb[(2 * y + 1) * s:(2 * y + 2) * s]
        o = y * W * 3
        for x in range(W):
            i = 6 * x
            for c in range(3):
                out[o + 3 * x + c] = (a[i + c] + a[i + 3 + c] + b[i + c] + b[i + 3 + c] + 2) >> 2
    return W, H, bytes(out)


def cells(w, h, rgb, nx=CELLS_X, ny=CELLS_Y):
    """the picture as ny rows of nx (r, g, b) area averages (float). Cell edges at x*w//nx; a
    big picture is sampled every second pixel and row (the averages barely move)"""
    if w < nx or h < ny: raise ValueError(f'picture {w}x{h} is smaller than the grid')
    step = 2 if w * h > 400 * 300 else 1
    s = w * 3
    xs = [x * w // nx for x in range(nx + 1)]
    out = []
    for cy in range(ny):
        y0, y1 = cy * h // ny, (cy + 1) * h // ny
        acc = [[0, 0, 0] for _ in range(nx)]
        n = [0] * nx
        for y in range(y0, y1, step):
            row = rgb[y * s:(y + 1) * s]
            for cx in range(nx):
                x0, x1 = xs[cx], xs[cx + 1]
                a = acc[cx]
                for c in range(3):
                    a[c] += sum(row[3 * x0 + c:3 * x1:3 * step])
                n[cx] += (x1 - x0 + step - 1) // step
        out.append([tuple(v / n[cx] for v in acc[cx]) for cx in range(nx)])
    return out


def compare(want, got):
    """want, got: (w, h, rgb); want may be the halved golden. -> dict(ok, why, mean, bad)"""
    (ww, wh, _), (gw, gh, _) = want, got
    # (same aspect, within a pixel of the halving)
    if abs(ww * gh - wh * gw) > max(ww, gw) + max(wh, gh) or not (ww and wh and gw and gh):
        return {'ok': False, 'why': f'size {gw}x{gh}, Windows {ww}x{wh}', 'mean': None, 'bad': None}
    a, b = cells(*want), cells(*got)
    tot, bad = 0.0, 0
    for ra, rb in zip(a, b):
        for ca, cb in zip(ra, rb):
            d = [abs(x - y) for x, y in zip(ca, cb)]
            tot += sum(d) / 3
            bad += max(d) > CELL_TOL
    mean, frac = tot / (CELLS_X * CELLS_Y), bad / (CELLS_X * CELLS_Y)
    ok = mean < MEAN_TOL and frac <= BAD_MAX
    why = '' if ok else f'{bad} of {CELLS_X * CELLS_Y} cells differ, mean {mean:.1f}'
    return {'ok': ok, 'why': why, 'mean': round(mean, 2), 'bad': round(frac, 4)}


if __name__ == '__main__':
    cmd = sys.argv[1] if len(sys.argv) > 1 else ''
    if cmd == 'compare' and len(sys.argv) == 4:
        r = compare(read(sys.argv[2]), read(sys.argv[3]))
        print(r)
        sys.exit(0 if r['ok'] else 1)
    if cmd == 'half' and len(sys.argv) == 4:
        write(sys.argv[3], *half(*read(sys.argv[2])))
        sys.exit(0)
    sys.exit(__doc__)
