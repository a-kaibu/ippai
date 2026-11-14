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

#define MAXTOK 64
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
static void op_device(void) {
    d3d9_reset();
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
static void op_state(void) {
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

static void reset_all(void) {
    gdi_reset();
    d3d9_reset();
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
        else if (is(0, "text") || is(0, "bk") || is(0, "bkmode") || is(0, "org") || is(0, "clip") || is(0, "noclip")) op_dcstate();
        else if (is(0, "device")) op_device();
        else if (is(0, "rs") || is(0, "tss") || is(0, "ss")) op_state();
        else if (is(0, "sb")) op_sb();
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
