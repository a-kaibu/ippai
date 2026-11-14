/* wingfx executor: interprets a case file (wingfx/SPEC.md) against the Windows graphics APIs of
 * whatever system runs it (Windows: the reference; Wine, sake, ...: under test) and prints the
 * results, one fact per line. The cases are compiled in (build.sh: WINGFX_CASES, a header made
 * from cases/<suite>.txt), so the exe takes no arguments and needs no files: it runs the same
 * under a native console, Wine, sake's node build or sake in a browser page. An argument names a
 * case file to run instead (ad-hoc runs of freshly generated cases).
 *
 * Output goes through WriteFile on the standard output only (a C runtime's stdio is not what this
 * tests) and is flushed after every case, so a crash keeps every finished case. */
#define COBJMACROS
#define __USE_MINGW_ANSI_STDIO 0
#include <windows.h>
#include <d3d9.h>
#include <d3d11.h>
#include <wincodec.h>
#include <ddraw.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include WINGFX_CASES /* static const char wingfx_cases[] */

/* ------------------------------------------------------------------------------------------ */
/* output */

static char obuf[1 << 16];
static int olen;
static void oflush(void) {
    DWORD n;
    if (olen) WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), obuf, olen, &n, NULL);
    olen = 0;
}
static void out(const char *fmt, ...) {
    va_list va;
    if (olen > (int)sizeof(obuf) - 8192) oflush();
    va_start(va, fmt);
    olen += vsprintf(obuf + olen, fmt, va);
    va_end(va);
}

/* ------------------------------------------------------------------------------------------ */
/* tokens of one line */

#define MAXTOK 80
static char *tok[MAXTOK];
static int ntok, lineno;
static int bad; /* the current line could not be run as written */

static long num(int i) {
    if (i >= ntok) { bad = 1; return 0; }
    char *e;
    long v = (long)strtoul(tok[i], &e, 0);
    if (tok[i][0] == '-') v = strtol(tok[i], &e, 0);
    if (*e) bad = 1;
    return v;
}
/* "#RRGGBB" -> COLORREF */
static COLORREF color(int i) {
    if (i >= ntok || tok[i][0] != '#') { bad = 1; return 0; }
    unsigned v = (unsigned)strtoul(tok[i] + 1, NULL, 16);
    return RGB(v >> 16 & 255, v >> 8 & 255, v & 255);
}
static int is(int i, const char *s) { return i < ntok && !strcmp(tok[i], s); }

/* ------------------------------------------------------------------------------------------ */
/* GDI: surfaces in slots, each a DIB section selected into its own memory DC */

#define SLOTS 8
static struct slot {
    HDC dc;
    HBITMAP bmp, oldbmp;
    unsigned char *bits;
    int w, h, bpp, stride, topdown;
    RGBQUAD pal[256];
    HBRUSH brush;
    HGDIOBJ oldbrush;
} sl[SLOTS];

static struct slot *slot(int i) {
    long n = num(i);
    if (n < 0 || n >= SLOTS || !sl[n].dc) { bad = 1; return NULL; }
    return &sl[n];
}

/* the default color tables (SPEC.md "dib") */
static void default_palette(int bpp, RGBQUAD *p) {
    static const unsigned vga[16] = { 0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0,
                                      0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff };
    int n = 1 << bpp;
    for (int i = 0; i < n; i++) {
        unsigned c;
        if (bpp == 1) c = i ? 0xffffff : 0;
        else if (bpp == 4) c = vga[i];
        else c = ((i >> 5 & 7) * 255 / 7) << 16 | ((i >> 2 & 7) * 255 / 7) << 8 | (i & 3) * 255 / 3;
        p[i].rgbRed = c >> 16; p[i].rgbGreen = c >> 8 & 255; p[i].rgbBlue = c & 255; p[i].rgbReserved = 0;
    }
}

static void free_slot(struct slot *s) {
    if (!s->dc) return;
    if (s->brush) { SelectObject(s->dc, s->oldbrush); DeleteObject(s->brush); }
    SelectObject(s->dc, s->oldbmp);
    DeleteObject(s->bmp);
    DeleteDC(s->dc);
    memset(s, 0, sizeof(*s));
}

/* dib <slot> <w> <h> <bpp> [td|bu] [565] [#c0 #c1 ...] */
static void op_dib(void) {
    long n = num(1);
    if (n < 0 || n >= SLOTS) { bad = 1; return; }
    struct slot *s = &sl[n];
    free_slot(s);
    int w = num(2), h = num(3), bpp = num(4), i = 5, f565 = 0;
    s->topdown = 1;
    if (is(i, "td")) i++;
    else if (is(i, "bu")) { s->topdown = 0; i++; }
    if (is(i, "565")) { f565 = 1; i++; }
    if (bad) return;
    struct { BITMAPINFOHEADER h; union { RGBQUAD pal[256]; DWORD masks[3]; } u; } bi;
    memset(&bi, 0, sizeof(bi));
    bi.h.biSize = sizeof(bi.h);
    bi.h.biWidth = w;
    bi.h.biHeight = s->topdown ? -h : h;
    bi.h.biPlanes = 1;
    bi.h.biBitCount = bpp;
    bi.h.biCompression = BI_RGB;
    if (bpp <= 8) {
        default_palette(bpp, s->pal);
        for (int k = 0; i < ntok && k < (1 << bpp); i++, k++) {
            COLORREF c = color(i);
            s->pal[k].rgbRed = GetRValue(c); s->pal[k].rgbGreen = GetGValue(c); s->pal[k].rgbBlue = GetBValue(c);
        }
        memcpy(bi.u.pal, s->pal, sizeof(RGBQUAD) << bpp);
        bi.h.biClrUsed = 1 << bpp;
    } else if (f565) {
        bi.h.biCompression = BI_BITFIELDS;
        bi.u.masks[0] = 0xf800; bi.u.masks[1] = 0x07e0; bi.u.masks[2] = 0x001f;
    }
    s->dc = CreateCompatibleDC(NULL);
    s->bmp = CreateDIBSection(s->dc, (BITMAPINFO *)&bi, DIB_RGB_COLORS, (void **)&s->bits, NULL, 0);
    if (!s->bmp) { out("dib failed %lu\n", GetLastError()); DeleteDC(s->dc); s->dc = NULL; return; }
    s->oldbmp = SelectObject(s->dc, s->bmp);
    s->w = w; s->h = h; s->bpp = bpp; s->stride = ((w * bpp + 31) / 32) * 4;
    memset(s->bits, 0, s->stride * h);
}

/* fill <slot> <seed>: every byte of the pixel buffer from xorshift32 (SPEC.md) */
static void op_fill(void) {
    struct slot *s = slot(1);
    unsigned x = (unsigned)num(2);
    if (bad) return;
    if (!x) x = 0x9e3779b9;
    for (int i = 0; i < s->stride * s->h; i++) {
        x ^= x << 13; x ^= x >> 17; x ^= x << 5;
        s->bits[i] = (unsigned char)x;
    }
}

/* the row of the picture's top-to-bottom y in memory */
static unsigned char *row(struct slot *s, int y) { return s->bits + (s->topdown ? y : s->h - 1 - y) * s->stride; }

/* dump <slot>: "px <slot> <w> <h> <bpp>" then each row's bytes (only those of the w pixels; a
 * partial last byte as it is in memory), top to bottom */
static void op_dump(void) {
    struct slot *s = slot(1);
    if (bad) return;
    GdiFlush();
    out("px %ld %d %d %d\n", num(1), s->w, s->h, s->bpp);
    int bytes = (s->w * s->bpp + 7) / 8;
    for (int y = 0; y < s->h; y++) {
        unsigned char *p = row(s, y);
        char *o = obuf + olen;
        for (int i = 0; i < bytes; i++) o += sprintf(o, "%02x", p[i]);
        *o++ = '\n';
        olen = o - obuf;
        if (olen > (int)sizeof(obuf) - 8192) oflush();
    }
}

/* poke <slot> <x> <y> <hex bytes>: raw bytes at a pixel (bpp >= 8) */
static void op_poke(void) {
    struct slot *s = slot(1);
    int x = num(2), y = num(3);
    if (bad || s->bpp < 8 || x < 0 || y < 0 || x >= s->w || y >= s->h || ntok < 5) { bad = 1; return; }
    unsigned char *p = row(s, y) + x * (s->bpp / 8);
    for (int i = 0; tok[4][2 * i] && tok[4][2 * i + 1] && i < s->bpp / 8; i++) {
        char b[3] = { tok[4][2 * i], tok[4][2 * i + 1], 0 };
        p[i] = (unsigned char)strtoul(b, NULL, 16);
    }
}

static void set_brush(struct slot *s, HBRUSH b) {
    if (!b) { out("brush failed\n"); return; }
    HGDIOBJ o = SelectObject(s->dc, b);
    if (s->brush) DeleteObject(s->brush);
    else s->oldbrush = o;
    s->brush = b;
}

/* brush <slot> solid #c | hatch <style> #c | dib <src slot> | null */
static void op_brush(void) {
    struct slot *s = slot(1);
    if (bad) return;
    if (is(2, "solid")) {
        COLORREF c = color(3);
        if (!bad) set_brush(s, CreateSolidBrush(c));
    } else if (is(2, "hatch")) {
        int st = num(3);
        COLORREF c = color(4);
        if (!bad) set_brush(s, CreateHatchBrush(st, c));
    } else if (is(2, "dib")) {
        struct slot *p = slot(3);
        if (bad) return;
        /* a packed DIB of the slot's picture: header, color table, bits */
        int ncol = p->bpp <= 8 ? 1 << p->bpp : 0;
        int size = sizeof(BITMAPINFOHEADER) + ncol * 4 + p->stride * p->h;
        BITMAPINFOHEADER *bh = calloc(1, size);
        bh->biSize = sizeof(*bh);
        bh->biWidth = p->w;
        bh->biHeight = p->topdown ? -p->h : p->h;
        bh->biPlanes = 1;
        bh->biBitCount = p->bpp;
        bh->biCompression = BI_RGB;
        bh->biClrUsed = ncol;
        memcpy((char *)(bh + 1), p->pal, ncol * 4);
        memcpy((char *)(bh + 1) + ncol * 4, p->bits, p->stride * p->h);
        set_brush(s, CreateDIBPatternBrushPt(bh, DIB_RGB_COLORS));
        free(bh);
    } else if (is(2, "null")) {
        set_brush(s, (HBRUSH)GetStockObject(NULL_BRUSH));
        s->brush = NULL; /* (stock: never deleted) */
    } else bad = 1;
}

static void op_patblt(void) {
    struct slot *s = slot(1);
    int x = num(2), y = num(3), w = num(4), h = num(5);
    DWORD rop = (DWORD)num(6);
    if (bad) return;
    BOOL r = PatBlt(s->dc, x, y, w, h, rop);
    out("ret %d\n", r ? 1 : 0);
}

static void op_bitblt(void) {
    struct slot *d = slot(1);
    int x = num(2), y = num(3), w = num(4), h = num(5);
    struct slot *s = slot(6);
    int sx = num(7), sy = num(8);
    DWORD rop = (DWORD)num(9);
    if (bad) return;
    BOOL r = BitBlt(d->dc, x, y, w, h, s->dc, sx, sy, rop);
    out("ret %d\n", r ? 1 : 0);
}

/* stretchblt <d> <x> <y> <w> <h> <s> <sx> <sy> <sw> <sh> <rop> */
static void op_stretchblt(void) {
    struct slot *d = slot(1);
    int x = num(2), y = num(3), w = num(4), h = num(5);
    struct slot *s = slot(6);
    int sx = num(7), sy = num(8), sw = num(9), sh = num(10);
    DWORD rop = (DWORD)num(11);
    if (bad) return;
    BOOL r = StretchBlt(d->dc, x, y, w, h, s->dc, sx, sy, sw, sh, rop);
    out("ret %d\n", r ? 1 : 0);
}

static void print_color(const char *what, COLORREF c) {
    if (c == CLR_INVALID) out("%s invalid\n", what);
    else out("%s %02x%02x%02x\n", what, GetRValue(c), GetGValue(c), GetBValue(c));
}

/* a top-down BITMAPINFO of w x h at <bpp> [565] (color table: the default one, SPEC.md) */
typedef struct { BITMAPINFOHEADER h; union { RGBQUAD pal[256]; DWORD masks[3]; } u; } BI;
static void make_bi(BI *bi, int w, int h, int bpp, int f565) {
    memset(bi, 0, sizeof(*bi));
    bi->h.biSize = sizeof(bi->h);
    bi->h.biWidth = w;
    bi->h.biHeight = -h;
    bi->h.biPlanes = 1;
    bi->h.biBitCount = bpp;
    bi->h.biCompression = f565 ? BI_BITFIELDS : BI_RGB;
    if (f565) { bi->u.masks[0] = 0xf800; bi->u.masks[1] = 0x07e0; bi->u.masks[2] = 0x001f; }
    if (bpp <= 8) { default_palette(bpp, bi->u.pal); bi->h.biClrUsed = 1 << bpp; }
}

/* getdibits <s> <bpp> [565]: GetDIBits of the slot's bitmap (deselected meanwhile) as a
 * top-down DIB of that format: "dibits <lines>", for bpp <= 8 "pal" and the table GetDIBits
 * wrote, then the rows as dump prints them */
static void op_getdibits(void) {
    struct slot *s = slot(1);
    int bpp = num(2), f565 = is(3, "565");
    if (bad) return;
    BI bi;
    make_bi(&bi, s->w, s->h, bpp, f565);
    int stride = ((s->w * bpp + 31) / 32) * 4;
    unsigned char *buf = calloc(1, stride * s->h);
    SelectObject(s->dc, s->oldbmp);
    int n = GetDIBits(s->dc, s->bmp, 0, s->h, buf, (BITMAPINFO *)&bi, DIB_RGB_COLORS);
    SelectObject(s->dc, s->bmp);
    out("dibits %d\n", n);
    if (bpp <= 8) {
        out("pal");
        for (int i = 0; i < (1 << bpp); i++) out(" %02x%02x%02x", bi.u.pal[i].rgbRed, bi.u.pal[i].rgbGreen, bi.u.pal[i].rgbBlue);
        out("\n");
    }
    int bytes = (s->w * bpp + 7) / 8;
    for (int y = 0; y < s->h && n > 0; y++) {
        for (int i = 0; i < bytes; i++) out("%02x", buf[y * stride + i]);
        out("\n");
    }
    free(buf);
}

/* setdibits <s> <bpp> [565] <seed>: SetDIBits of a top-down DIB of that format (default color
 * table), its bytes from xorshift32 like fill, onto the slot's bitmap (deselected meanwhile) */
static void op_setdibits(void) {
    struct slot *s = slot(1);
    int bpp = num(2), f565 = is(3, "565");
    unsigned x = (unsigned)num(f565 ? 4 : 3);
    if (bad) return;
    BI bi;
    make_bi(&bi, s->w, s->h, bpp, f565);
    int stride = ((s->w * bpp + 31) / 32) * 4;
    unsigned char *buf = calloc(1, stride * s->h);
    if (!x) x = 0x9e3779b9;
    for (int i = 0; i < stride * s->h; i++) { x ^= x << 13; x ^= x >> 17; x ^= x << 5; buf[i] = (unsigned char)x; }
    SelectObject(s->dc, s->oldbmp);
    int n = SetDIBits(s->dc, s->bmp, 0, s->h, buf, (BITMAPINFO *)&bi, DIB_RGB_COLORS);
    SelectObject(s->dc, s->bmp);
    out("setdibits %d\n", n);
    free(buf);
}

static void op_pixel(void) {
    struct slot *s = slot(1);
    int x = num(2), y = num(3);
    if (bad) return;
    if (is(0, "getpixel")) print_color("pixel", GetPixel(s->dc, x, y));
    else { COLORREF c = color(4); if (!bad) print_color("pixel", SetPixel(s->dc, x, y, c)); }
}

static void op_dcstate(void) {
    struct slot *s = slot(1);
    if (bad) return;
    if (is(0, "text")) { COLORREF c = color(2); if (!bad) SetTextColor(s->dc, c); }
    else if (is(0, "bk")) { COLORREF c = color(2); if (!bad) SetBkColor(s->dc, c); }
    else if (is(0, "bkmode")) { int m = num(2); if (!bad) SetBkMode(s->dc, m); }
    else if (is(0, "org")) { int x = num(2), y = num(3); if (!bad) SetBrushOrgEx(s->dc, x, y, NULL); }
    else if (is(0, "clip")) {
        int a = num(2), b = num(3), c = num(4), d = num(5);
        if (!bad) out("clip %d\n", IntersectClipRect(s->dc, a, b, c, d));
    } else if (is(0, "noclip")) SelectClipRgn(s->dc, NULL);
    else if (is(0, "mode")) { int m = num(2); if (!bad) SetStretchBltMode(s->dc, m); }
}

static void gdi_reset(void) {
    for (int i = 0; i < SLOTS; i++) free_slot(&sl[i]);
}

/* ------------------------------------------------------------------------------------------ */
/* Direct3D 9 (d3d9.dll loaded only by a case that asks for a device: the GDI suite runs where
 * there is none) */

static IDirect3D9 *d3d;
static IDirect3DDevice9 *dev;
static HWND hwnd;
#define BLOCKS 8
static IDirect3DStateBlock9 *blocks[BLOCKS];

static void d3d9_reset(void) {
    for (int i = 0; i < BLOCKS; i++)
        if (blocks[i]) { IDirect3DStateBlock9_Release(blocks[i]); blocks[i] = NULL; }
    if (dev) { IDirect3DDevice9_Release(dev); dev = NULL; }
    if (d3d) { IDirect3D9_Release(d3d); d3d = NULL; }
}

/* device [hwvp|swvp|mixed]: a windowed HAL device, 64x64 X8R8G8B8 back buffer, D24S8 */
static void d3d8_reset(void);
static void op_device(void) {
    d3d9_reset();
    d3d8_reset();
    static IDirect3D9 *(WINAPI *create)(UINT);
    if (!create) {
        HMODULE m = LoadLibraryA("d3d9.dll");
        if (m) create = (void *)GetProcAddress(m, "Direct3DCreate9");
    }
    if (!create || !(d3d = create(D3D_SDK_VERSION))) { out("device none\n"); return; }
    if (!hwnd) {
        WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "wingfx" };
        RegisterClassA(&wc);
        hwnd = CreateWindowA("wingfx", "wingfx", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    }
    DWORD flags = D3DCREATE_HARDWARE_VERTEXPROCESSING;
    if (is(1, "swvp")) flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING;
    else if (is(1, "mixed")) flags = D3DCREATE_MIXED_VERTEXPROCESSING;
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = 64;
    pp.BackBufferHeight = 64;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.hDeviceWindow = hwnd;
    pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = TRUE;
    pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, flags, &pp, &dev);
    if (FAILED(hr)) {
        /* (a software vertex processing device where hardware is refused: the state is the same) */
        hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    }
    if (FAILED(hr)) { out("device failed %08lx\n", hr); dev = NULL; return; }
    out("device ok\n");
}

/* rs|tss|ss set <args> <value> | get <args>: "<what> <args> = <hex>" or "... hr <hex>" */
static void op_state8(void);
static void op_sb8(void);
static void *dev8;
static void op_state(void) {
    if (dev8) { op_state8(); return; }
    if (!dev) { out("nodev\n"); return; }
    int set = is(1, "set");
    if (!set && !is(1, "get")) { bad = 1; return; }
    int two = !is(0, "rs");
    long a = num(2), b = two ? num(3) : 0;
    DWORD v = set ? (DWORD)num(two ? 4 : 3) : 0xdeadbeef;
    if (bad) return;
    HRESULT hr;
    if (is(0, "rs")) hr = set ? IDirect3DDevice9_SetRenderState(dev, a, v) : IDirect3DDevice9_GetRenderState(dev, a, &v);
    else if (is(0, "tss")) hr = set ? IDirect3DDevice9_SetTextureStageState(dev, a, b, v) : IDirect3DDevice9_GetTextureStageState(dev, a, b, &v);
    else hr = set ? IDirect3DDevice9_SetSamplerState(dev, a, b, v) : IDirect3DDevice9_GetSamplerState(dev, a, b, &v);
    if (set) {
        if (hr) out("%s %ld%s%s set hr %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", hr);
    } else if (hr) out("%s %ld%s%s hr %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", hr);
    else out("%s %ld%s%s = %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", v);
}

/* sb begin | end <id> | create <id> <type 1 all, 2 pixel, 3 vertex> | capture <id> | apply <id> | release <id> */
static void op_sb(void) {
    if (dev8) { op_sb8(); return; }
    if (!dev) { out("nodev\n"); return; }
    HRESULT hr;
    if (is(1, "begin")) hr = IDirect3DDevice9_BeginStateBlock(dev);
    else {
        long i = num(2);
        if (bad || i < 0 || i >= BLOCKS) { bad = 1; return; }
        if (is(1, "end") || is(1, "create")) {
            if (blocks[i]) { IDirect3DStateBlock9_Release(blocks[i]); blocks[i] = NULL; }
            if (is(1, "end")) hr = IDirect3DDevice9_EndStateBlock(dev, &blocks[i]);
            else {
                long t = num(3);
                if (bad) return;
                hr = IDirect3DDevice9_CreateStateBlock(dev, t, &blocks[i]);
            }
            if (FAILED(hr)) blocks[i] = NULL;
        } else if (!blocks[i]) { out("sb %ld none\n", i); return; }
        else if (is(1, "capture")) hr = IDirect3DStateBlock9_Capture(blocks[i]);
        else if (is(1, "apply")) hr = IDirect3DStateBlock9_Apply(blocks[i]);
        else if (is(1, "release")) { IDirect3DStateBlock9_Release(blocks[i]); blocks[i] = NULL; hr = 0; }
        else { bad = 1; return; }
    }
    if (hr) out("sb %s hr %08lx\n", tok[1], hr);
}


/* ------------------------------------------------------------------------------------------ */
/* Direct3D 8 (d3d8.dll loaded when a case asks for it; its header clashes with d3d9's, so the
 * calls go through the vtables by index, d3d8.h's order) */

static void *d3d8;
#define VT(o, i) ((*(void ***)(o))[i])
typedef ULONG (WINAPI *Rel8)(void *);
static DWORD blocks8[BLOCKS];

static void d3d8_reset(void) {
    if (dev8) {
        for (int i = 0; i < BLOCKS; i++)
            if (blocks8[i]) ((HRESULT (WINAPI *)(void *, DWORD))VT(dev8, 56))(dev8, blocks8[i]);
        ((Rel8)VT(dev8, 2))(dev8);
    }
    memset(blocks8, 0, sizeof blocks8);
    dev8 = NULL;
    if (d3d8) { ((Rel8)VT(d3d8, 2))(d3d8); d3d8 = NULL; }
}

/* device8 [hwvp|swvp|mixed]: as device, for Direct3D 8 */
static void op_device8(void) {
    d3d9_reset();
    d3d8_reset();
    static void *(WINAPI *create)(UINT);
    if (!create) {
        HMODULE m = LoadLibraryA("d3d8.dll");
        if (m) create = (void *)GetProcAddress(m, "Direct3DCreate8");
    }
    if (!create || !(d3d8 = create(220))) { out("device8 none\n"); return; }
    if (!hwnd) {
        WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "wingfx" };
        RegisterClassA(&wc);
        hwnd = CreateWindowA("wingfx", "wingfx", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    }
    DWORD flags = D3DCREATE_HARDWARE_VERTEXPROCESSING;
    if (is(1, "swvp")) flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING;
    else if (is(1, "mixed")) flags = D3DCREATE_MIXED_VERTEXPROCESSING;
    /* D3DPRESENT_PARAMETERS of Direct3D 8: no MultiSampleQuality */
    struct { UINT w, h; D3DFORMAT fmt; UINT count; DWORD ms, swap; HWND win; BOOL windowed, autods; D3DFORMAT dsfmt; DWORD flags; UINT rate, interval; } pp = { 0 };
    pp.w = 64; pp.h = 64; pp.fmt = D3DFMT_X8R8G8B8; pp.count = 1; pp.swap = D3DSWAPEFFECT_DISCARD;
    pp.win = hwnd; pp.windowed = TRUE; pp.autods = TRUE; pp.dsfmt = D3DFMT_D24S8;
    typedef HRESULT (WINAPI *Create8)(void *, UINT, D3DDEVTYPE, HWND, DWORD, void *, void **);
    HRESULT hr = ((Create8)VT(d3d8, 15))(d3d8, 0, D3DDEVTYPE_HAL, hwnd, flags, &pp, &dev8);
    if (FAILED(hr)) hr = ((Create8)VT(d3d8, 15))(d3d8, 0, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev8);
    if (FAILED(hr)) { out("device8 failed %08lx\n", hr); dev8 = NULL; return; }
    out("device8 ok\n");
}

static void op_state8(void) {
    int set = is(1, "set");
    if ((!set && !is(1, "get")) || is(0, "ss")) { bad = 1; return; }
    int two = is(0, "tss");
    long a = num(2), b = two ? num(3) : 0;
    DWORD v = set ? (DWORD)num(two ? 4 : 3) : 0xdeadbeef;
    if (bad) return;
    HRESULT hr;
    if (!two) hr = set ? ((HRESULT (WINAPI *)(void *, DWORD, DWORD))VT(dev8, 50))(dev8, a, v) : ((HRESULT (WINAPI *)(void *, DWORD, DWORD *))VT(dev8, 51))(dev8, a, &v);
    else hr = set ? ((HRESULT (WINAPI *)(void *, DWORD, DWORD, DWORD))VT(dev8, 63))(dev8, a, b, v) : ((HRESULT (WINAPI *)(void *, DWORD, DWORD, DWORD *))VT(dev8, 62))(dev8, a, b, &v);
    if (set) {
        if (hr) out("%s %ld%s%s set hr %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", hr);
    } else if (hr) out("%s %ld%s%s hr %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", hr);
    else out("%s %ld%s%s = %08lx\n", tok[0], a, two ? " " : "", two ? tok[3] : "", v);
}

static void op_sb8(void) {
    typedef HRESULT (WINAPI *Tok)(void *, DWORD);
    HRESULT hr;
    if (is(1, "begin")) hr = ((HRESULT (WINAPI *)(void *))VT(dev8, 52))(dev8);
    else {
        long i = num(2);
        if (bad || i < 0 || i >= BLOCKS) { bad = 1; return; }
        if (is(1, "end") || is(1, "create")) {
            if (blocks8[i]) { ((Tok)VT(dev8, 56))(dev8, blocks8[i]); blocks8[i] = 0; }
            if (is(1, "end")) hr = ((HRESULT (WINAPI *)(void *, DWORD *))VT(dev8, 53))(dev8, &blocks8[i]);
            else {
                long t = num(3);
                if (bad) return;
                hr = ((HRESULT (WINAPI *)(void *, DWORD, DWORD *))VT(dev8, 57))(dev8, t, &blocks8[i]);
            }
            if (FAILED(hr)) blocks8[i] = 0;
        } else if (!blocks8[i]) { out("sb %ld none\n", i); return; }
        else if (is(1, "capture")) hr = ((Tok)VT(dev8, 55))(dev8, blocks8[i]);
        else if (is(1, "apply")) hr = ((Tok)VT(dev8, 54))(dev8, blocks8[i]);
        else if (is(1, "release")) { hr = ((Tok)VT(dev8, 56))(dev8, blocks8[i]); blocks8[i] = 0; }
        else { bad = 1; return; }
    }
    if (hr) out("sb %s hr %08lx\n", tok[1], hr);
}

/* ------------------------------------------------------------------------------------------ */
/* Direct3D 11 state objects (d3d11.dll loaded only when a case asks for a device) */

static ID3D11Device *dev11;
#define OBJS 64
static IUnknown *objs[OBJS];
static int nobjs;

static void d3d11_reset(void) {
    for (int i = 0; i < nobjs; i++)
        if (objs[i]) IUnknown_Release(objs[i]);
    nobjs = 0;
    if (dev11) { ID3D11Device_Release(dev11); dev11 = NULL; }
}

/* device11: D3D11CreateDevice, hardware (WARP if refused), feature level 11_0 asked */
static void op_device11(void) {
    d3d11_reset();
    static HRESULT (WINAPI *create)(void *, D3D_DRIVER_TYPE, HMODULE, UINT, const D3D_FEATURE_LEVEL *, UINT, UINT, ID3D11Device **, D3D_FEATURE_LEVEL *, void **);
    if (!create) {
        HMODULE m = LoadLibraryA("d3d11.dll");
        if (m) create = (void *)GetProcAddress(m, "D3D11CreateDevice");
    }
    if (!create) { out("device11 none\n"); return; }
    D3D_FEATURE_LEVEL fl = D3D_FEATURE_LEVEL_11_0;
    HRESULT hr = create(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev11, NULL, NULL);
    if (FAILED(hr)) hr = create(NULL, D3D_DRIVER_TYPE_WARP, NULL, 0, &fl, 1, D3D11_SDK_VERSION, &dev11, NULL, NULL);
    if (FAILED(hr)) { out("device11 failed %08lx\n", hr); dev11 = NULL; return; }
    out("device11 ok\n");
}

/* blend|depth|raster|sampler [check|nodesc] <the description's 32-bit words, missing ones 0>: Create...State
 * (ppState NULL for check, pDesc NULL for nodesc), then GetDesc. Prints "<kind> hr <hr>" on
 * failure (and for check / nodesc), else "<kind> = <GetDesc's words> same <n|->", n the
 * case's earlier state line (counted from 0) that gave the same object. */
static void op_state11(void) {
    static const char *kinds[] = { "blend", "depth", "raster", "sampler" };
    static const int words[] = { 66, 13, 10, 13 };
    int k = 0;
    while (!is(0, kinds[k])) k++;
    if (!dev11) { out("nodev\n"); return; }
    int check = is(1, "check"), nodesc = is(1, "nodesc"), at = 1 + check + nodesc;
    DWORD d[66] = { 0 }, got[66];
    if (!nodesc) {
        if (ntok - at > words[k]) { bad = 1; return; }
        for (int i = 0; i < ntok - at; i++) d[i] = (DWORD)num(at + i);
    } else if (ntok != 2) { bad = 1; return; }
    if (bad || nobjs >= OBJS) { bad = 1; return; }
    void *o = NULL, **po = check ? NULL : &o, *pd = nodesc ? NULL : d;
    HRESULT hr;
    memset(got, 0xcc, sizeof got);
    switch (k) {
    case 0:
        hr = ID3D11Device_CreateBlendState(dev11, pd, (ID3D11BlendState **)po);
        if (o) ID3D11BlendState_GetDesc((ID3D11BlendState *)o, (D3D11_BLEND_DESC *)got);
        break;
    case 1:
        hr = ID3D11Device_CreateDepthStencilState(dev11, pd, (ID3D11DepthStencilState **)po);
        if (o) ID3D11DepthStencilState_GetDesc((ID3D11DepthStencilState *)o, (D3D11_DEPTH_STENCIL_DESC *)got);
        break;
    case 2:
        hr = ID3D11Device_CreateRasterizerState(dev11, pd, (ID3D11RasterizerState **)po);
        if (o) ID3D11RasterizerState_GetDesc((ID3D11RasterizerState *)o, (D3D11_RASTERIZER_DESC *)got);
        break;
    default:
        hr = ID3D11Device_CreateSamplerState(dev11, pd, (ID3D11SamplerState **)po);
        if (o) ID3D11SamplerState_GetDesc((ID3D11SamplerState *)o, (D3D11_SAMPLER_DESC *)got);
        break;
    }
    objs[nobjs++] = o;
    if (!o) { out("%s hr %08lx\n", kinds[k], hr); return; }
    out("%s =", kinds[k]);
    for (int i = 0; i < words[k]; i++) {
        DWORD w = got[i];
        /* (UINT8 fields: the write mask of each target, the stencil masks; padding not printed) */
        if (k == 0 && i >= 2 && (i - 2) % 8 == 7) w &= 0xff;
        if (k == 1 && i == 4) w &= 0xffff;
        out(" %lx", w);
    }
    int same = -1;
    for (int j = 0; j < nobjs - 1; j++)
        if (objs[j] == o) { same = j; break; }
    if (same < 0) out(" same -\n");
    else out(" same %d\n", same);
}

/* ------------------------------------------------------------------------------------------ */
/* WIC pixel format conversion (windowscodecs.dll through COM, initialized when first needed) */

static const struct { const char *n; const GUID *g; int bits; } wicfmts[] = {
    { "bw", &GUID_WICPixelFormatBlackWhite, 1 }, { "gray2", &GUID_WICPixelFormat2bppGray, 2 },
    { "gray4", &GUID_WICPixelFormat4bppGray, 4 }, { "gray8", &GUID_WICPixelFormat8bppGray, 8 },
    { "gray16", &GUID_WICPixelFormat16bppGray, 16 }, { "bgr555", &GUID_WICPixelFormat16bppBGR555, 16 },
    { "bgr565", &GUID_WICPixelFormat16bppBGR565, 16 }, { "bgr24", &GUID_WICPixelFormat24bppBGR, 24 },
    { "rgb24", &GUID_WICPixelFormat24bppRGB, 24 }, { "bgr32", &GUID_WICPixelFormat32bppBGR, 32 },
    { "bgra", &GUID_WICPixelFormat32bppBGRA, 32 }, { "pbgra", &GUID_WICPixelFormat32bppPBGRA, 32 },
    { "rgb32", &GUID_WICPixelFormat32bppRGB, 32 }, { "rgba", &GUID_WICPixelFormat32bppRGBA, 32 },
    { "prgba", &GUID_WICPixelFormat32bppPRGBA, 32 }, { "rgb48", &GUID_WICPixelFormat48bppRGB, 48 },
    { "rgba64", &GUID_WICPixelFormat64bppRGBA, 64 }, { "cmyk", &GUID_WICPixelFormat32bppCMYK, 32 },
    { "alpha8", &GUID_WICPixelFormat8bppAlpha, 8 }, { "idx1", &GUID_WICPixelFormat1bppIndexed, 1 },
    { "idx2", &GUID_WICPixelFormat2bppIndexed, 2 }, { "idx4", &GUID_WICPixelFormat4bppIndexed, 4 },
    { "idx8", &GUID_WICPixelFormat8bppIndexed, 8 },
};
static IWICImagingFactory *wic;

static int wicfmt(int i) {
    for (int f = 0; f < (int)(sizeof wicfmts / sizeof *wicfmts); f++)
        if (is(i, wicfmts[f].n)) return f;
    bad = 1;
    return 0;
}

/* the palette of indexed sources and targets: 16 colors, entry 5 not opaque (alpha 0x40) */
static IWICPalette *wic_palette(void) {
    IWICPalette *p = NULL;
    if (FAILED(IWICImagingFactory_CreatePalette(wic, &p)) || !p) return NULL;
    WICColor c[16];
    for (int i = 0; i < 16; i++) c[i] = (i == 5 ? 0x40000000u : 0xff000000u) | (unsigned)(i * 17) << 16 | (unsigned)(255 - i * 13) << 8 | (unsigned)((i * 71) & 255);
    IWICPalette_InitializeCustom(p, c, 16);
    return p;
}

/* convert <from> <w> <h> <seed|ramp> <to>: a bitmap of `from` (rows (w * bits + 7) / 8 bytes,
 * from xorshift32 or byte i = i mod 256), a format converter to `to` (no dither, the palette
 * for indexed formats), CopyPixels of all of it. Prints "conv <w> <h>" and each row's bytes (the
 * bits past the row's last pixel 0), or "conv can <0|1> init|copy hr <hr>". */
static void op_convert(void) {
    int from = wicfmt(1);
    long w = num(2), h = num(3);
    int ramp = is(4, "ramp");
    unsigned seed = ramp ? 0 : (unsigned)num(4);
    int to = wicfmt(5);
    if (bad || w < 1 || h < 1 || w > 256 || h > 64) { bad = 1; return; }
    if (!wic) {
        CoInitializeEx(NULL, COINIT_MULTITHREADED);
        if (FAILED(CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&wic))) wic = NULL;
    }
    if (!wic) { out("conv none\n"); return; }
    int ss = (w * wicfmts[from].bits + 7) / 8, ds = (w * wicfmts[to].bits + 7) / 8;
    unsigned char *px = malloc(ss * h), *o = malloc(ds * h);
    unsigned x = seed ? seed : 0x9e3779b9;
    for (int i = 0; i < ss * h; i++) {
        x ^= x << 13; x ^= x >> 17; x ^= x << 5;
        px[i] = ramp ? (unsigned char)i : (unsigned char)x;
    }
    IWICBitmap *b = NULL;
    IWICFormatConverter *c = NULL;
    IWICPalette *pal = wic_palette();
    HRESULT hr = IWICImagingFactory_CreateBitmapFromMemory(wic, w, h, wicfmts[from].g, ss, ss * h, px, &b);
    if (FAILED(hr)) { out("conv bitmap hr %08lx\n", hr); goto done; }
    if (from >= 19 && pal) IWICBitmap_SetPalette(b, pal);
    IWICImagingFactory_CreateFormatConverter(wic, &c);
    BOOL can = FALSE;
    IWICFormatConverter_CanConvert(c, wicfmts[from].g, wicfmts[to].g, &can);
    hr = IWICFormatConverter_Initialize(c, (IWICBitmapSource *)b, wicfmts[to].g, WICBitmapDitherTypeNone, to >= 19 ? pal : NULL, 0, to >= 19 ? WICBitmapPaletteTypeCustom : WICBitmapPaletteTypeMedianCut);
    if (FAILED(hr)) { out("conv can %d init hr %08lx\n", can, hr); goto done; }
    memset(o, 0, ds * h);
    hr = IWICFormatConverter_CopyPixels(c, NULL, ds, ds * h, o);
    if (FAILED(hr)) { out("conv can %d copy hr %08lx\n", can, hr); goto done; }
    out("conv %ld %ld can %d\n", w, h, can);
    for (int y = 0; y < h; y++) {
        unsigned char *r = o + y * ds;
        int tail = (w * wicfmts[to].bits) % 8;
        if (tail) r[ds - 1] &= (unsigned char)(0xff00 >> tail);
        for (int i = 0; i < ds; i++) out("%02x", r[i]);
        out("\n");
    }
done:
    if (c) IWICFormatConverter_Release(c);
    if (b) IWICBitmap_Release(b);
    if (pal) IWICPalette_Release(pal);
    free(px);
    free(o);
}

/* ------------------------------------------------------------------------------------------ */
/* DirectDraw 7: system-memory offscreen surfaces in slots (ddraw.dll loaded when first needed) */

static IDirectDraw7 *dd7;
static IDirectDrawSurface7 *dds[SLOTS];

static void ddraw_reset(void) {
    for (int i = 0; i < SLOTS; i++)
        if (dds[i]) { IDirectDrawSurface7_Release(dds[i]); dds[i] = NULL; }
    if (dd7) { IDirectDraw7_Release(dd7); dd7 = NULL; }
}

static IDirectDrawSurface7 *dslot(int i) {
    long s = num(i);
    if (bad || s < 0 || s >= SLOTS || !dds[s]) { bad = 1; return NULL; }
    return dds[s];
}

/* dd: DirectDrawCreateEx (IDirectDraw7), SetCooperativeLevel(DDSCL_NORMAL) */
static void op_dd(void) {
    ddraw_reset();
    static HRESULT (WINAPI *create)(GUID *, void **, REFIID, IUnknown *);
    if (!create) {
        HMODULE m = LoadLibraryA("ddraw.dll");
        if (m) create = (void *)GetProcAddress(m, "DirectDrawCreateEx");
    }
    static const GUID iid7 = { 0x15e65ec0, 0x3b9c, 0x11d2, { 0xb9, 0x2f, 0x00, 0x60, 0x97, 0x97, 0xea, 0x5b } };
    if (!create) { out("dd none\n"); return; }
    HRESULT hr = create(NULL, (void **)&dd7, &iid7, NULL);
    if (FAILED(hr)) { out("dd failed %08lx\n", hr); dd7 = NULL; return; }
    if (!hwnd) {
        WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "wingfx" };
        RegisterClassA(&wc);
        hwnd = CreateWindowA("wingfx", "wingfx", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    }
    IDirectDraw7_SetCooperativeLevel(dd7, hwnd, DDSCL_NORMAL);
    out("dd ok\n");
}

/* ddsurf <s> <w> <h> <8|555|565|32> */
static void op_ddsurf(void) {
    long i = num(1), w = num(2), h = num(3);
    if (bad || i < 0 || i >= SLOTS || w < 1 || h < 1 || w > 256 || h > 256) { bad = 1; return; }
    if (!dd7) { out("nodev\n"); return; }
    if (dds[i]) { IDirectDrawSurface7_Release(dds[i]); dds[i] = NULL; }
    DDSURFACEDESC2 d = { sizeof d };
    d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    d.dwWidth = w;
    d.dwHeight = h;
    d.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    d.ddpfPixelFormat.dwSize = sizeof d.ddpfPixelFormat;
    d.ddpfPixelFormat.dwFlags = DDPF_RGB;
    if (is(4, "8")) { d.ddpfPixelFormat.dwFlags |= DDPF_PALETTEINDEXED8; d.ddpfPixelFormat.dwRGBBitCount = 8; }
    else if (is(4, "555")) { d.ddpfPixelFormat.dwRGBBitCount = 16; d.ddpfPixelFormat.dwRBitMask = 0x7c00; d.ddpfPixelFormat.dwGBitMask = 0x3e0; d.ddpfPixelFormat.dwBBitMask = 0x1f; }
    else if (is(4, "565")) { d.ddpfPixelFormat.dwRGBBitCount = 16; d.ddpfPixelFormat.dwRBitMask = 0xf800; d.ddpfPixelFormat.dwGBitMask = 0x7e0; d.ddpfPixelFormat.dwBBitMask = 0x1f; }
    else if (is(4, "32")) { d.ddpfPixelFormat.dwRGBBitCount = 32; d.ddpfPixelFormat.dwRBitMask = 0xff0000; d.ddpfPixelFormat.dwGBitMask = 0xff00; d.ddpfPixelFormat.dwBBitMask = 0xff; }
    else { bad = 1; return; }
    HRESULT hr = IDirectDraw7_CreateSurface(dd7, &d, &dds[i], NULL);
    if (FAILED(hr)) { out("ddsurf failed %08lx\n", hr); dds[i] = NULL; }
}

/* ddfill <s> <seed> | dddump <s>: the bytes of each row's pixels through Lock */
static void op_ddlock(void) {
    IDirectDrawSurface7 *s = dslot(1);
    int fill = is(0, "ddfill");
    unsigned x = fill ? (unsigned)num(2) : 0;
    if (bad) return;
    DDSURFACEDESC2 d = { sizeof d };
    HRESULT hr = IDirectDrawSurface7_Lock(s, NULL, &d, DDLOCK_WAIT, NULL);
    if (FAILED(hr)) { out("lock hr %08lx\n", hr); return; }
    int row = d.dwWidth * d.ddpfPixelFormat.dwRGBBitCount / 8;
    if (!x) x = 0x9e3779b9;
    if (!fill) out("dd %s %lu %lu %lu\n", tok[1], d.dwWidth, d.dwHeight, d.ddpfPixelFormat.dwRGBBitCount);
    for (DWORD y = 0; y < d.dwHeight; y++) {
        unsigned char *p = (unsigned char *)d.lpSurface + y * d.lPitch;
        for (int i = 0; i < row; i++) {
            if (fill) { x ^= x << 13; x ^= x >> 17; x ^= x << 5; p[i] = (unsigned char)x; }
            else out("%02x", p[i]);
        }
        if (!fill) out("\n");
    }
    IDirectDrawSurface7_Unlock(s, NULL);
}

/* ddkey <s> src|dest <value>|none: SetColorKey DDCKEY_SRCBLT / DESTBLT, low = high = value */
static void op_ddkey(void) {
    IDirectDrawSurface7 *s = dslot(1);
    DWORD which = is(2, "src") ? DDCKEY_SRCBLT : is(2, "dest") ? DDCKEY_DESTBLT : 0;
    if (!which) bad = 1;
    DDCOLORKEY k = { 0, 0 };
    int none = is(3, "none");
    if (!none) k.dwColorSpaceLowValue = k.dwColorSpaceHighValue = (DWORD)num(3);
    if (bad) return;
    HRESULT hr = IDirectDrawSurface7_SetColorKey(s, which, none ? NULL : &k);
    if (hr) out("ddkey hr %08lx\n", hr);
}

/* a rect at token i: "all" (NULL) or x0 y0 x1 y1; returns the next token */
static int ddrect(int i, RECT *r, RECT **pr) {
    if (is(i, "all")) { *pr = NULL; return i + 1; }
    r->left = num(i); r->top = num(i + 1); r->right = num(i + 2); r->bottom = num(i + 3);
    *pr = r;
    return i + 4;
}

/* ddblt <d> <rect> <s|-> <rect> [keysrc keydest mirrorlr mirrorud keysrcov <v> keydestov <v>]
 * ddcolor <d> <rect> <value>: Blt DDBLT_COLORFILL
 * ddfast <d> <x> <y> <s> <rect> [srckey destkey]: BltFast */
static void op_ddblt(void) {
    IDirectDrawSurface7 *d = dslot(1), *s = NULL;
    RECT dr, sr, *pdr = NULL, *psr = NULL;
    DDBLTFX fx = { sizeof fx };
    HRESULT hr;
    if (is(0, "ddfast")) {
        long x = num(2), y = num(3);
        s = dslot(4);
        int i = ddrect(5, &sr, &psr);
        DWORD f = DDBLTFAST_WAIT;
        for (; i < ntok; i++)
            if (is(i, "srckey")) f |= DDBLTFAST_SRCCOLORKEY;
            else if (is(i, "destkey")) f |= DDBLTFAST_DESTCOLORKEY;
            else bad = 1;
        if (bad) return;
        hr = IDirectDrawSurface7_BltFast(d, x, y, s, psr, f);
    } else if (is(0, "ddcolor")) {
        int i = ddrect(2, &dr, &pdr);
        fx.dwFillColor = (DWORD)num(i);
        if (bad || ntok != i + 1) { bad = 1; return; }
        hr = IDirectDrawSurface7_Blt(d, pdr, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
    } else {
        int i = ddrect(2, &dr, &pdr);
        if (!is(i, "-")) s = dslot(i);
        i = ddrect(i + 1, &sr, &psr);
        DWORD f = DDBLT_WAIT;
        for (; i < ntok; i++)
            if (is(i, "keysrc")) f |= DDBLT_KEYSRC;
            else if (is(i, "keydest")) f |= DDBLT_KEYDEST;
            else if (is(i, "mirrorlr")) { f |= DDBLT_DDFX; fx.dwDDFX |= DDBLTFX_MIRRORLEFTRIGHT; }
            else if (is(i, "mirrorud")) { f |= DDBLT_DDFX; fx.dwDDFX |= DDBLTFX_MIRRORUPDOWN; }
            else if (is(i, "keysrcov")) { f |= DDBLT_KEYSRCOVERRIDE; fx.ddckSrcColorkey.dwColorSpaceLowValue = fx.ddckSrcColorkey.dwColorSpaceHighValue = (DWORD)num(++i); }
            else if (is(i, "keydestov")) { f |= DDBLT_KEYDESTOVERRIDE; fx.ddckDestColorkey.dwColorSpaceLowValue = fx.ddckDestColorkey.dwColorSpaceHighValue = (DWORD)num(++i); }
            else bad = 1;
        if (bad) return;
        hr = IDirectDrawSurface7_Blt(d, pdr, s, psr, f, &fx);
    }
    out("ret %08lx\n", hr);
}

/* ------------------------------------------------------------------------------------------ */

static void reset_all(void) {
    gdi_reset();
    d3d9_reset();
    d3d8_reset();
    d3d11_reset();
    ddraw_reset();
}

static void run(char *text) {
    int incase = 0;
    for (char *l = text, *next; l && *l; l = next) {
        next = strchr(l, '\n');
        if (next) *next++ = 0;
        lineno++;
        char *hash = strchr(l, '#');
        /* (a comment starts with "# " or is the whole line; "#RRGGBB" is a color) */
        if (hash && (hash[1] == ' ' || hash == l)) *hash = 0;
        ntok = 0;
        for (char *t = strtok(l, " \t\r"); t && ntok < MAXTOK; t = strtok(NULL, " \t\r")) tok[ntok++] = t;
        if (!ntok) continue;
        bad = 0;
        if (is(0, "case")) {
            if (incase) { reset_all(); oflush(); }
            out("case %s\n", ntok > 1 ? tok[1] : "?");
            incase = 1;
        } else if (is(0, "end")) { reset_all(); oflush(); incase = 0; }
        else if (is(0, "dib")) op_dib();
        else if (is(0, "fill")) op_fill();
        else if (is(0, "poke")) op_poke();
        else if (is(0, "dump")) op_dump();
        else if (is(0, "brush")) op_brush();
        else if (is(0, "patblt")) op_patblt();
        else if (is(0, "bitblt")) op_bitblt();
        else if (is(0, "stretchblt")) op_stretchblt();
        else if (is(0, "getdibits")) op_getdibits();
        else if (is(0, "setdibits")) op_setdibits();
        else if (is(0, "getpixel") || is(0, "setpixel")) op_pixel();
        else if (is(0, "text") || is(0, "bk") || is(0, "bkmode") || is(0, "org") || is(0, "clip") || is(0, "noclip") || is(0, "mode")) op_dcstate();
        else if (is(0, "device")) op_device();
        else if (is(0, "rs") || is(0, "tss") || is(0, "ss")) op_state();
        else if (is(0, "sb")) op_sb();
        else if (is(0, "device11")) op_device11();
        else if (is(0, "device8")) op_device8();
        else if (is(0, "convert")) op_convert();
        else if (is(0, "dd")) op_dd();
        else if (is(0, "ddsurf")) op_ddsurf();
        else if (is(0, "ddfill") || is(0, "dddump")) op_ddlock();
        else if (is(0, "ddkey")) op_ddkey();
        else if (is(0, "ddblt") || is(0, "ddcolor") || is(0, "ddfast")) op_ddblt();
        else if (is(0, "blend") || is(0, "depth") || is(0, "raster") || is(0, "sampler")) op_state11();
        else bad = 1;
        if (bad) out("bad line %d\n", lineno);
    }
    reset_all();
    oflush();
}

int main(int argc, char **argv) {
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    char *text;
    if (argc > 1) {
        HANDLE f = CreateFileA(argv[1], GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        if (f == INVALID_HANDLE_VALUE) { out("cannot open %s\n", argv[1]); oflush(); return 2; }
        DWORD n = GetFileSize(f, NULL), got;
        text = malloc(n + 1);
        ReadFile(f, text, n, &got, NULL);
        text[got] = 0;
        CloseHandle(f);
    } else text = strdup(wingfx_cases);
    out("wingfx %s\n", WINGFX_SUITE);
    run(text);
    out("done\n");
    oflush();
    return 0;
}
