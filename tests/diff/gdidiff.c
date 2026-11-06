/* Differential test of GDI drawing into DIB sections (tests/diff/run.sh): random operations,
 * each one drawn on a fresh pseudo-random canvas, the canvas hashed after it. The same program
 * runs on a reference (Wine's GDI, whose DIB engine follows Windows pixel for pixel) and on
 * sake; a different hash names the first operation kind that draws differently.
 *
 *   gdidiff.exe <seed> <count> [dump <op>]
 * prints "<index> <bpp> <op> <params> <hash>" per operation; with "dump <i>" also the canvas
 * of operation i as hex rows (to compare pixel by pixel). */
#define __USE_MINGW_ANSI_STDIO 0
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>

#define W 61
#define H 47

/* output through WriteFile only: a C runtime's stdout (text mode, buffering, which stream) is
 * not what this tests */
static char obuf[1 << 16];
static int olen;
static void oflush(void) {
    DWORD n;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), obuf, olen, &n, NULL);
    olen = 0;
}
static void out(const char *fmt, ...) {
    va_list va;
    if (olen > (int)sizeof(obuf) - 4096) oflush();
    va_start(va, fmt);
    olen += vsprintf(obuf + olen, fmt, va);
    va_end(va);
}

static unsigned rng_state;
static unsigned rnd(void) {
    rng_state ^= rng_state << 13; rng_state ^= rng_state >> 17; rng_state ^= rng_state << 5;
    return rng_state;
}
static int rr(int lo, int hi) { return lo + (int)(rnd() % (unsigned)(hi - lo + 1)); }
static COLORREF rcolor(void) {
    /* a few exact colors (black, white, primaries) are where conversions round differently */
    static const COLORREF fixed[] = { 0, 0xffffff, 0xff, 0xff00, 0xff0000, 0x808080, 0x7f7f7f };
    if (rnd() % 3 == 0) return fixed[rnd() % 7];
    return rnd() & 0xffffff;
}

static HBITMAP make_dib(HDC dc, int bpp, int w, int h, void **bits, int topdown) {
    struct { BITMAPINFOHEADER h; RGBQUAD pal[256]; } bi;
    memset(&bi, 0, sizeof(bi));
    bi.h.biSize = sizeof(bi.h);
    bi.h.biWidth = w;
    bi.h.biHeight = topdown ? -h : h;
    bi.h.biPlanes = 1;
    bi.h.biBitCount = bpp;
    bi.h.biCompression = BI_RGB;
    if (bpp == 8) {
        int i;
        for (i = 0; i < 256; i++) {
            bi.pal[i].rgbRed = (BYTE)(i * 37); bi.pal[i].rgbGreen = (BYTE)(i * 91); bi.pal[i].rgbBlue = (BYTE)(i * 53);
        }
        bi.pal[0].rgbRed = bi.pal[0].rgbGreen = bi.pal[0].rgbBlue = 0;
        bi.pal[255].rgbRed = bi.pal[255].rgbGreen = bi.pal[255].rgbBlue = 255;
        bi.h.biClrUsed = 256;
    }
    return CreateDIBSection(dc, (BITMAPINFO *)&bi, DIB_RGB_COLORS, bits, NULL, 0);
}
static int stride(int bpp, int w) { return ((w * bpp + 31) / 32) * 4; }

static unsigned fnv(const unsigned char *p, int n) {
    unsigned h = 2166136261u;
    while (n--) { h ^= *p++; h *= 16777619u; }
    return h;
}

static HPEN rpen(char *desc) {
    static const int styles[] = { PS_SOLID, PS_DASH, PS_DOT, PS_DASHDOT, PS_NULL, PS_INSIDEFRAME };
    int s = styles[rnd() % 6], w = rr(0, 4);
    COLORREF c = rcolor();
    sprintf(desc + strlen(desc), " pen(%d,%d,%06lx)", s, w, (unsigned long)c);
    return CreatePen(s, w, c);
}
static HBRUSH rbrush(char *desc) {
    COLORREF c = rcolor();
    int k = rnd() % 4;
    if (k == 0) {
        int hs = rnd() % 6;
        sprintf(desc + strlen(desc), " hatch(%d,%06lx)", hs, (unsigned long)c);
        return CreateHatchBrush(hs, c);
    }
    if (k == 1) { strcat(desc, " null-brush"); return GetStockObject(NULL_BRUSH); }
    sprintf(desc + strlen(desc), " brush(%06lx)", (unsigned long)c);
    return CreateSolidBrush(c);
}

int main(int argc, char **argv) {
    static const int bpps[] = { 32, 24, 16, 8 };
    unsigned seed = argc > 1 ? strtoul(argv[1], NULL, 0) : 1;
    int count = argc > 2 ? atoi(argv[2]) : 200, dump = argc > 4 ? atoi(argv[4]) : -1, i;
    HDC screen = GetDC(NULL);
    HDC dc = CreateCompatibleDC(screen), sdc = CreateCompatibleDC(screen);
    void *sbits;
    HBITMAP src = make_dib(screen, 32, 23, 19, &sbits, 1);
    HGDIOBJ stock = GetCurrentObject(dc, OBJ_BITMAP);
    SelectObject(sdc, src);

    for (i = 0; i < count; i++) {
        int bpp = bpps[(i / 4) % 4], op, n, k;
        char desc[512] = "";
        void *bits;
        HBITMAP bmp;
        HGDIOBJ oldpen, oldbrush;
        HPEN pen;
        HBRUSH brush;
        POINT pts[8];
        rng_state = seed * 2654435761u + (unsigned)i * 40503u + 1;
        rnd(); rnd();
        bmp = make_dib(screen, bpp, W, H, &bits, rnd() & 1);
        SelectObject(dc, bmp);
        /* the canvas and the source: deterministic noise */
        for (n = 0; n < stride(bpp, W) * H; n++) ((unsigned char *)bits)[n] = (unsigned char)rnd();
        for (n = 0; n < 23 * 19 * 4; n++) ((unsigned char *)sbits)[n] = (unsigned char)rnd();
        GdiFlush();
        {   /* (the values set, not read back: the line must only differ by its pixels) */
            int rop2 = rnd() % 3 == 0 ? rr(1, 16) : R2_COPYPEN, bkm = rnd() & 1 ? OPAQUE : TRANSPARENT;
            COLORREF bk = rcolor();
            int fill = rnd() & 1 ? ALTERNATE : WINDING;
            SetROP2(dc, rop2);
            SetBkMode(dc, bkm);
            SetBkColor(dc, bk);
            SetPolyFillMode(dc, fill);
            sprintf(desc, "rop2=%d bk=%d/%06lx fill=%d", rop2, bkm, (unsigned long)bk, fill);
        }
        pen = rpen(desc);
        brush = rbrush(desc);
        oldpen = SelectObject(dc, pen);
        oldbrush = SelectObject(dc, brush);
        op = rnd() % 14;
        switch (op) {
        case 0: {
            int x = rr(-2, W + 1), y = rr(-2, H + 1);
            COLORREF c = rcolor();
            out("%d %d SetPixel %d,%d %06lx %s -> %06lx", i, bpp, x, y, (unsigned long)c, desc, (unsigned long)SetPixel(dc, x, y, c));
            break;
        }
        case 1: {
            static const DWORD rops[] = { PATCOPY, PATINVERT, DSTINVERT, BLACKNESS, WHITENESS };
            DWORD rop = rops[rnd() % 5];
            int x = rr(-5, W), y = rr(-5, H), w = rr(-10, 40), h = rr(-10, 40);
            out("%d %d PatBlt %d,%d %dx%d %08lx %s", i, bpp, x, y, w, h, (unsigned long)rop, desc);
            PatBlt(dc, x, y, w, h, rop);
            break;
        }
        case 2: case 3: case 4: {
            int l = rr(-5, W), t = rr(-5, H), r = rr(-5, W + 5), b = rr(-5, H + 5);
            static const char *names[] = { "Rectangle", "Ellipse", "RoundRect" };
            out("%d %d %s %d,%d,%d,%d %s", i, bpp, names[op - 2], l, t, r, b, desc);
            if (op == 2) Rectangle(dc, l, t, r, b);
            else if (op == 3) Ellipse(dc, l, t, r, b);
            else { int ew = rr(0, 20), eh = rr(0, 20); out(" %d,%d", ew, eh); RoundRect(dc, l, t, r, b, ew, eh); }
            break;
        }
        case 5: case 6: {
            n = rr(2, 8);
            out("%d %d %s %s", i, bpp, op == 5 ? "Polygon" : "Polyline", desc);
            for (k = 0; k < n; k++) { pts[k].x = rr(-8, W + 8); pts[k].y = rr(-8, H + 8); out(" %ld,%ld", pts[k].x, pts[k].y); }
            if (op == 5) Polygon(dc, pts, n); else Polyline(dc, pts, n);
            break;
        }
        case 7: {
            int x0 = rr(-5, W + 5), y0 = rr(-5, H + 5), x1 = rr(-5, W + 5), y1 = rr(-5, H + 5);
            out("%d %d LineTo %d,%d-%d,%d %s", i, bpp, x0, y0, x1, y1, desc);
            MoveToEx(dc, x0, y0, NULL);
            LineTo(dc, x1, y1);
            break;
        }
        case 8: {
            RECT r;
            HBRUSH solid = CreateSolidBrush(rcolor());
            SetRect(&r, rr(-5, W), rr(-5, H), rr(-5, W + 5), rr(-5, H + 5));
            k = rnd() % 3;
            out("%d %d %s %ld,%ld,%ld,%ld", i, bpp, k == 0 ? "FillRect" : k == 1 ? "FrameRect" : "InvertRect", r.left, r.top, r.right, r.bottom);
            if (k == 0) FillRect(dc, &r, solid); else if (k == 1) FrameRect(dc, &r, solid); else InvertRect(dc, &r);
            DeleteObject(solid);
            break;
        }
        case 9: {
            DWORD rop = ((rnd() & 0xff) << 16) | 0x0000; /* any ROP3: the low word is ignored by GDI */
            static const DWORD named[] = { SRCCOPY, SRCPAINT, SRCAND, SRCINVERT, SRCERASE, NOTSRCCOPY, NOTSRCERASE, MERGECOPY, MERGEPAINT, PATPAINT };
            int x = rr(-5, W), y = rr(-5, H), w = rr(-3, 26), h = rr(-3, 22), sx = rr(-2, 10), sy = rr(-2, 8);
            if (rnd() & 1) rop = named[rnd() % 10];
            out("%d %d BitBlt %d,%d %dx%d from %d,%d rop=%08lx %s", i, bpp, x, y, w, h, sx, sy, (unsigned long)rop, desc);
            BitBlt(dc, x, y, w, h, sdc, sx, sy, rop);
            break;
        }
        case 10: {
            static const int modes[] = { BLACKONWHITE, WHITEONBLACK, COLORONCOLOR, HALFTONE };
            int m = modes[rnd() % 4];
            int x = rr(-5, W), y = rr(-5, H), w = rr(-40, 70), h = rr(-40, 60), sx = rr(0, 10), sy = rr(0, 8), sw = rr(-12, 20), sh = rr(-10, 18);
            DWORD rop = rnd() & 1 ? SRCCOPY : SRCINVERT;
            SetStretchBltMode(dc, m);
            if (m == HALFTONE) SetBrushOrgEx(dc, 0, 0, NULL);
            out("%d %d StretchBlt mode=%d %d,%d %dx%d from %d,%d %dx%d rop=%08lx", i, bpp, m, x, y, w, h, sx, sy, sw, sh, (unsigned long)rop);
            StretchBlt(dc, x, y, w, h, sdc, sx, sy, sw, sh, rop);
            break;
        }
        case 11: {
            BLENDFUNCTION bf;
            int x = rr(-5, W), y = rr(0, H), w = rr(1, 40), h = rr(1, 30), sx = rr(0, 10), sy = rr(0, 8), sw = rnd() & 1 ? w : rr(1, 13), sh = rnd() & 1 ? h : rr(1, 11);
            if (sx + sw > 23) sw = 23 - sx;
            if (sy + sh > 19) sh = 19 - sy;
            bf.BlendOp = AC_SRC_OVER; bf.BlendFlags = 0;
            bf.SourceConstantAlpha = rnd() & 1 ? 255 : (BYTE)rnd();
            bf.AlphaFormat = rnd() & 1 ? AC_SRC_ALPHA : 0;
            if (bf.AlphaFormat) /* premultiplied: no channel above alpha */
                for (n = 0; n < 23 * 19; n++) {
                    unsigned char *p = (unsigned char *)sbits + 4 * n;
                    for (k = 0; k < 3; k++) if (p[k] > p[3]) p[k] = p[3];
                }
            out("%d %d AlphaBlend %d,%d %dx%d from %d,%d %dx%d const=%d fmt=%d -> %d", i, bpp, x, y, w, h, sx, sy, sw, sh,
                   bf.SourceConstantAlpha, bf.AlphaFormat, GdiAlphaBlend(dc, x, y, w, h, sdc, sx, sy, sw, sh, bf));
            break;
        }
        case 12: {
            int x = rr(0, W - 1), y = rr(0, H - 1), w = rr(1, 30), h = rr(1, 25), sw = rnd() & 1 ? w : rr(1, 23), sh = rnd() & 1 ? h : rr(1, 19);
            COLORREF key = rnd() & 1 ? GetPixel(sdc, rr(0, 22), rr(0, 18)) : rcolor();
            if (sw > 23) sw = 23;
            if (sh > 19) sh = 19;
            out("%d %d TransparentBlt %d,%d %dx%d from 0,0 %dx%d key=%06lx -> %d", i, bpp, x, y, w, h, sw, sh, (unsigned long)key,
                   TransparentBlt(dc, x, y, w, h, sdc, 0, 0, sw, sh, key));
            break;
        }
        case 13: {
            struct { BITMAPINFOHEADER h; DWORD masks[3]; } bi;
            int x = rr(-5, W), y = rr(-5, H), w = rr(-30, 50), h = rr(-30, 40), sw = rr(1, 23), sh = rr(1, 19);
            memset(&bi, 0, sizeof(bi));
            bi.h.biSize = sizeof(bi.h); bi.h.biWidth = 23; bi.h.biHeight = rnd() & 1 ? -19 : 19;
            bi.h.biPlanes = 1; bi.h.biBitCount = 32; bi.h.biCompression = BI_RGB;
            out("%d %d StretchDIBits %d,%d %dx%d from 0,0 %dx%d h=%ld", i, bpp, x, y, w, h, sw, sh, bi.h.biHeight);
            StretchDIBits(dc, x, y, w, h, 0, 0, sw, sh, sbits, (BITMAPINFO *)&bi, DIB_RGB_COLORS, SRCCOPY);
            break;
        }
        }
        GdiFlush();
        out(" %08x\n", fnv(bits, stride(bpp, W) * H));
        if (i == dump) {
            for (k = 0; k < H; k++) {
                for (n = 0; n < stride(bpp, W); n++) out("%02x", ((unsigned char *)bits)[k * stride(bpp, W) + n]);
                out("\n");
            }
        }
        SelectObject(dc, oldpen);
        SelectObject(dc, oldbrush);
        DeleteObject(pen);
        DeleteObject(brush);
        SelectObject(dc, stock);
        DeleteObject(bmp);
    }
    oflush();
    return 0;
}
