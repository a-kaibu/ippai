/* GDI+ images as engines load and convert them: bitmaps decoded from in-memory BMP (32-bit
 * with an alpha byte, 24-bit bottom-up and top-down, 8-bit palette, 1-bit) and PNG (RGBA,
 * gray+alpha, palette with tRNS) streams, their pixel formats, palettes and flags, GetPixel,
 * LockBits into other formats (ARGB <-> PARGB premultiplication rounding, 24/16-bit, read
 * and write locks, rectangles, errors), CreateBitmapFromScan0, Clone, and
 * CreateHBITMAPFromBitmap's blend over a background colour. Pixel values are printed exactly.
 * libs: -lgdiplus -lole32 -lgdi32 */
#include "../probe.h"
#include <objbase.h>

typedef int GpStatus;
typedef void GpImage, GpBitmap;
typedef struct { UINT32 GdiplusVersion; void *DebugEventCallback; BOOL SuppressBackgroundThread, SuppressExternalCodecs; } StartupInput;
typedef struct { UINT Width, Height; INT Stride; INT PixelFormat; void *Scan0; UINT_PTR Reserved; } BitmapData;
typedef struct { INT X, Y, Width, Height; } GpRect;
GpStatus WINAPI GdiplusStartup(ULONG_PTR *, const StartupInput *, void *);
GpStatus WINAPI GdipCreateBitmapFromStream(IStream *, GpBitmap **);
GpStatus WINAPI GdipCreateBitmapFromScan0(INT, INT, INT, INT, BYTE *, GpBitmap **);
GpStatus WINAPI GdipGetImageWidth(GpImage *, UINT *);
GpStatus WINAPI GdipGetImageHeight(GpImage *, UINT *);
GpStatus WINAPI GdipGetImagePixelFormat(GpImage *, INT *);
GpStatus WINAPI GdipGetImageFlags(GpImage *, UINT *);
GpStatus WINAPI GdipGetImagePaletteSize(GpImage *, INT *);
GpStatus WINAPI GdipGetImagePalette(GpImage *, void *, INT);
GpStatus WINAPI GdipGetImageType(GpImage *, INT *);
GpStatus WINAPI GdipBitmapGetPixel(GpBitmap *, INT, INT, DWORD *);
GpStatus WINAPI GdipBitmapSetPixel(GpBitmap *, INT, INT, DWORD);
GpStatus WINAPI GdipBitmapLockBits(GpBitmap *, const GpRect *, UINT, INT, BitmapData *);
GpStatus WINAPI GdipBitmapUnlockBits(GpBitmap *, BitmapData *);
GpStatus WINAPI GdipCloneImage(GpImage *, GpImage **);
GpStatus WINAPI GdipDisposeImage(GpImage *);
GpStatus WINAPI GdipCreateHBITMAPFromBitmap(GpBitmap *, HBITMAP *, DWORD);
GpStatus WINAPI GdipGetImageDecodersSize(UINT *, UINT *);

#define PF_1 0x00030101
#define PF_8 0x00030803
#define PF_565 0x00021005
#define PF_555 0x00021005 /* (not used) */
#define PF_24 0x00021808
#define PF_32RGB 0x00022009
#define PF_ARGB 0x0026200a
#define PF_PARGB 0x000e200b

/* ---- building the test files ---- */
static BYTE buf[8192];
static int len;
static void put(const void *p, int n) { memcpy(buf + len, p, n); len += n; }
static void p8(int v) { BYTE b = v; put(&b, 1); }
static void p16(int v) { p8(v); p8(v >> 8); }
static void p32(DWORD v) { p16(v); p16(v >> 16); }
static void b32(DWORD v) { p8(v >> 24); p8(v >> 16); p8(v >> 8); p8(v); }

static void bmp(int w, int h, int bpp, int ncolors, const DWORD *pal, const BYTE *rows, int stride) {
    len = 0;
    int off = 14 + 40 + 4 * ncolors;
    int ah = h < 0 ? -h : h;
    put("BM", 2); p32(off + stride * ah); p32(0); p32(off);
    p32(40); p32(w); p32(h); p16(1); p16(bpp); p32(0); p32(stride * ah); p32(2835); p32(2835); p32(ncolors); p32(0);
    for (int i = 0; i < ncolors; i++) p32(pal[i]);
    put(rows, stride * ah);
}

static DWORD crc_table[256];
static DWORD crc(const BYTE *p, int n) {
    if (!crc_table[1]) for (DWORD i = 0; i < 256; i++) { DWORD c = i; for (int k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >> 1) : c >> 1; crc_table[i] = c; }
    DWORD c = 0xffffffff;
    for (int i = 0; i < n; i++) c = crc_table[(c ^ p[i]) & 255] ^ (c >> 8);
    return c ^ 0xffffffff;
}
static void chunk(const char *type, const BYTE *d, int n) {
    b32(n);
    int at = len;
    put(type, 4); put(d, n);
    b32(crc(buf + at, n + 4));
}
/* a PNG with the raw (filter 0) rows in one stored deflate block */
static void png(int w, int h, int depth, int color, const BYTE *raw, int rawlen, const BYTE *plte, int nplte, const BYTE *trns, int ntrns) {
    len = 0;
    put("\x89PNG\r\n\x1a\n", 8);
    BYTE ih[13] = { w >> 24, w >> 16, w >> 8, w, h >> 24, h >> 16, h >> 8, h, depth, color, 0, 0, 0 };
    chunk("IHDR", ih, 13);
    if (nplte) chunk("PLTE", plte, nplte);
    if (ntrns) chunk("tRNS", trns, ntrns);
    static BYTE z[4096];
    int n = 0;
    z[n++] = 0x78; z[n++] = 0x01; z[n++] = 1; z[n++] = rawlen; z[n++] = rawlen >> 8; z[n++] = ~rawlen; z[n++] = ~rawlen >> 8;
    memcpy(z + n, raw, rawlen); n += rawlen;
    DWORD a = 1, b = 0;
    for (int i = 0; i < rawlen; i++) { a = (a + raw[i]) % 65521; b = (b + a) % 65521; }
    DWORD ad = b << 16 | a;
    z[n++] = ad >> 24; z[n++] = ad >> 16; z[n++] = ad >> 8; z[n++] = ad;
    chunk("IDAT", z, n);
    chunk("IEND", NULL, 0);
}

static GpBitmap *load(void) {
    HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE, len);
    memcpy(GlobalLock(g), buf, len);
    GlobalUnlock(g);
    IStream *s;
    CreateStreamOnHGlobal(g, TRUE, &s);
    GpBitmap *b = NULL;
    GpStatus st = GdipCreateBitmapFromStream(s, &b);
    s->lpVtbl->Release(s);
    printf("  load %d\n", st);
    return st ? NULL : b;
}

static void info(GpBitmap *b) {
    UINT w = 0, h = 0, fl = 0;
    INT pf = 0, ps = 0, ty = 0;
    GdipGetImageWidth(b, &w); GdipGetImageHeight(b, &h); GdipGetImagePixelFormat(b, &pf); GdipGetImageType(b, &ty);
    GpStatus fs = GdipGetImageFlags(b, &fl);
    GdipGetImagePaletteSize(b, &ps);
    printf("  %ux%u format %08x type %d flags %d:%x palette size %d", w, h, pf, ty, fs, fl, ps);
    if (ps > 8) {
        static BYTE pal[1032];
        GdipGetImagePalette(b, pal, ps);
        UINT *p = (UINT *)pal;
        printf(" pflags %x count %u:", p[0], p[1]);
        for (UINT i = 0; i < p[1] && i < 6; i++) printf(" %08x", p[2 + i]);
    }
    printf("\n  pixels");
    for (UINT y = 0; y < h && y < 3; y++)
        for (UINT x = 0; x < w && x < 4; x++) {
            DWORD c = 0;
            GpStatus st = GdipBitmapGetPixel(b, x, y, &c);
            printf(" %08lx", st ? 0xbad00000 | st : c);
        }
    printf("\n");
}

static void lock(GpBitmap *b, int pf, UINT mode, const GpRect *r) {
    BitmapData d;
    memset(&d, 0, sizeof d);
    GpStatus st = GdipBitmapLockBits(b, r, mode, pf, &d);
    printf("  lock %08x mode %u%s: %d", pf, mode, r ? " rect" : "", st);
    if (!st) {
        printf(" %ux%u stride %d format %08x:", d.Width, d.Height, d.Stride, d.PixelFormat);
        int bpp = (pf >> 8) & 0xff;
        for (UINT y = 0; y < d.Height && y < 2; y++) {
            BYTE *row = (BYTE *)d.Scan0 + y * d.Stride;
            int n = (d.Width * bpp + 7) / 8;
            printf(" ");
            for (int i = 0; i < n && i < 16; i++) printf("%02x", row[i]);
        }
        if (mode & 2) {
            /* write: the first pixel becomes half-transparent red in the locked format */
            BYTE *p = d.Scan0;
            if (pf == PF_ARGB) *(DWORD *)p = 0x80ff0000;
            else if (pf == PF_PARGB) *(DWORD *)p = 0x80800000;
            else if (pf == PF_24) { p[0] = 0; p[1] = 0; p[2] = 0xff; }
        }
        printf(" unlock %d", GdipBitmapUnlockBits(b, &d));
    }
    printf("\n");
}

static void all_locks(GpBitmap *b) {
    int fmts[] = { PF_ARGB, PF_PARGB, PF_32RGB, PF_24, PF_565 };
    for (int i = 0; i < 5; i++) lock(b, fmts[i], 1, NULL);
}

int main(void) {
    ULONG_PTR tok;
    StartupInput si = { 1, NULL, FALSE, FALSE };
    printf("startup %d\n", GdiplusStartup(&tok, &si, NULL));

    /* 32-bit BMP with "alpha" bytes (BI_RGB: GDI+ decides whether they count) */
    BYTE r32[2 * 8] = { 0x10, 0x20, 0x30, 0x80, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0x40, 0x80, 0xc0, 0x01 };
    bmp(2, 2, 32, 0, NULL, r32, 8);
    printf("bmp32\n");
    GpBitmap *b = load(); if (b) { info(b); all_locks(b); GdipDisposeImage(b); }
    /* the same with every alpha byte 0 */
    for (int i = 3; i < 16; i += 4) r32[i] = 0;
    bmp(2, 2, 32, 0, NULL, r32, 8);
    printf("bmp32 zero alpha\n");
    b = load(); if (b) { info(b); GdipDisposeImage(b); }

    BYTE r24[2 * 8] = { 1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0 };
    bmp(2, 2, 24, 0, NULL, r24, 8);
    printf("bmp24 bottom-up\n");
    b = load(); if (b) { info(b); all_locks(b); GdipDisposeImage(b); }
    bmp(2, -2, 24, 0, NULL, r24, 8);
    printf("bmp24 top-down\n");
    b = load(); if (b) { info(b); GdipDisposeImage(b); }

    DWORD pal[4] = { 0x000000, 0xff0000, 0x00ff00, 0x0000ff };
    BYTE r8[2 * 4] = { 0, 1, 2, 3, 3, 2, 1, 0 };
    bmp(4, 2, 8, 4, pal, r8, 4);
    printf("bmp8\n");
    b = load(); if (b) { info(b); all_locks(b); lock(b, PF_8, 1, NULL); GdipDisposeImage(b); }
    DWORD pal1[2] = { 0x112233, 0xffeedd };
    BYTE r1[2 * 4] = { 0xa0, 0, 0, 0, 0x50, 0, 0, 0 };
    bmp(4, 2, 1, 2, pal1, r1, 4);
    printf("bmp1\n");
    b = load(); if (b) { info(b); lock(b, PF_ARGB, 1, NULL); GdipDisposeImage(b); }

    /* PNG RGBA: premultiplication cases (alpha 0, 1, 127, 128, 254, 255 over odd colours) */
    BYTE raw[1 + 6 * 4];
    BYTE px[6][4] = { { 255, 128, 1, 0 }, { 255, 128, 1, 1 }, { 255, 128, 1, 127 }, { 255, 128, 1, 128 }, { 200, 100, 3, 254 }, { 9, 99, 199, 255 } };
    raw[0] = 0;
    memcpy(raw + 1, px, sizeof px);
    png(6, 1, 8, 6, raw, sizeof raw, NULL, 0, NULL, 0);
    printf("png rgba\n");
    b = load();
    if (b) {
        info(b);
        all_locks(b);
        GpRect rc = { 2, 0, 3, 1 };
        lock(b, PF_PARGB, 1, &rc);
        GpRect bad = { 4, 0, 5, 1 };
        lock(b, PF_ARGB, 1, &bad);
        lock(b, PF_PARGB, 3, NULL);
        info(b);
        lock(b, PF_ARGB, 3, NULL);
        info(b);
        /* a second lock while one is held */
        BitmapData d1, d2;
        printf("  double lock %d", GdipBitmapLockBits(b, NULL, 1, PF_ARGB, &d1));
        printf(" %d", GdipBitmapLockBits(b, NULL, 1, PF_ARGB, &d2));
        printf(" unlock %d %d\n", GdipBitmapUnlockBits(b, &d1), GdipBitmapUnlockBits(b, &d1));
        HBITMAP hb = NULL;
        printf("  hbitmap %d", GdipCreateHBITMAPFromBitmap(b, &hb, 0xff336699));
        BITMAP bm;
        GetObjectA(hb, sizeof bm, &bm);
        printf(" %ldx%ld bpp %u bits %d:", bm.bmWidth, bm.bmHeight, bm.bmBitsPixel, bm.bmBits != NULL);
        if (bm.bmBits) for (int i = 0; i < 6; i++) printf(" %08lx", ((DWORD *)bm.bmBits)[i]);
        printf("\n");
        DeleteObject(hb);
        GpImage *cl = NULL;
        printf("  clone %d\n", GdipCloneImage(b, &cl));
        if (cl) { info(cl); GdipDisposeImage(cl); }
        GdipDisposeImage(b);
    }
    /* gray + alpha, palette with tRNS */
    BYTE ga[1 + 4] = { 0, 0x80, 0x40, 0xff, 0xff };
    png(2, 1, 8, 4, ga, sizeof ga, NULL, 0, NULL, 0);
    printf("png gray alpha\n");
    b = load(); if (b) { info(b); GdipDisposeImage(b); }
    BYTE pl[9] = { 255, 0, 0, 0, 255, 0, 0, 0, 255 }, tr[2] = { 0, 128 };
    BYTE pr[1 + 3] = { 0, 0, 1, 2 };
    png(3, 1, 8, 3, pr, sizeof pr, pl, 9, tr, 2);
    printf("png palette trns\n");
    b = load(); if (b) { info(b); lock(b, PF_ARGB, 1, NULL); GdipDisposeImage(b); }
    BYTE g16[1 + 4] = { 0, 0x12, 0x34, 0xfe, 0xdc };
    png(2, 1, 16, 0, g16, sizeof g16, NULL, 0, NULL, 0);
    printf("png gray16\n");
    b = load(); if (b) { info(b); GdipDisposeImage(b); }
    len = 10; memcpy(buf, "not image!", 10);
    printf("garbage\n");
    load();

    /* Scan0 bitmaps and SetPixel */
    printf("scan0\n");
    GpBitmap *s0 = NULL;
    printf("  create %d\n", GdipCreateBitmapFromScan0(3, 2, 0, PF_ARGB, NULL, &s0));
    info(s0);
    printf("  setpixel %d %d\n", GdipBitmapSetPixel(s0, 0, 0, 0x80ff8040), GdipBitmapSetPixel(s0, 5, 0, 0));
    info(s0);
    GdipDisposeImage(s0);
    printf("  create pargb %d\n", GdipCreateBitmapFromScan0(2, 1, 0, PF_PARGB, NULL, &s0));
    printf("  setpixel %d\n", GdipBitmapSetPixel(s0, 0, 0, 0x80ff8040));
    info(s0);
    lock(s0, PF_PARGB, 1, NULL);
    GdipDisposeImage(s0);
    printf("  create 565 %d\n", GdipCreateBitmapFromScan0(2, 1, 0, PF_565, NULL, &s0));
    GdipBitmapSetPixel(s0, 0, 0, 0xff123456);
    info(s0);
    GdipDisposeImage(s0);
    printf("  bad stride %d bad size %d\n", GdipCreateBitmapFromScan0(3, 2, 3, PF_ARGB, buf, &s0), GdipCreateBitmapFromScan0(0, 2, 0, PF_ARGB, NULL, &s0));
    UINT nd = 0, sz = 0;
    GdipGetImageDecodersSize(&nd, &sz);
    printf("decoders nonzero %d\n", nd > 0);
    return 0;
}
