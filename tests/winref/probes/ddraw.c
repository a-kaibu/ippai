/* DirectDraw 7 (and the IDirectDraw of DirectDrawCreate) in a window, as old games use it:
 * system-memory offscreen surfaces of 32, 16 (565 and 555) and 8 bpp (with a palette); Blt
 * color fill, copy, stretch up and down, source color keys (Blt and BltFast), mirroring, rect
 * clipping and its errors, overlapping blits within a surface; Lock pitches; GetDC with GDI
 * drawing on a surface; GetSurfaceDesc, GetPixelFormat and display mode queries.
 * libs: -lddraw -ldxguid -lgdi32 */
#define COBJMACROS
#include "../probe.h"
#include <ddraw.h>

#define SW 24
#define SH 16

static IDirectDraw7 *dd;

static IDirectDrawSurface7 *surface(int w, int h, int bpp, DWORD r, DWORD g, DWORD b) {
    DDSURFACEDESC2 d = { sizeof d };
    d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    d.dwWidth = w;
    d.dwHeight = h;
    d.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    d.ddpfPixelFormat.dwSize = sizeof d.ddpfPixelFormat;
    d.ddpfPixelFormat.dwFlags = DDPF_RGB | (bpp == 8 ? DDPF_PALETTEINDEXED8 : 0);
    d.ddpfPixelFormat.dwRGBBitCount = bpp;
    d.ddpfPixelFormat.dwRBitMask = r;
    d.ddpfPixelFormat.dwGBitMask = g;
    d.ddpfPixelFormat.dwBBitMask = b;
    IDirectDrawSurface7 *s = NULL;
    HRESULT hr = IDirectDraw7_CreateSurface(dd, &d, &s, NULL);
    if (FAILED(hr)) { printf("CreateSurface %dx%d %d bpp %#lx\n", w, h, bpp, hr); return NULL; }
    return s;
}

/* a surface's pixels as a picture (its format's channels to 8 bits; 8 bpp as the index in
 * all three) */
static void show(const char *name, IDirectDrawSurface7 *s) {
    DDSURFACEDESC2 d = { sizeof d };
    HRESULT hr = IDirectDrawSurface7_Lock(s, NULL, &d, DDLOCK_WAIT | DDLOCK_READONLY, NULL);
    if (FAILED(hr)) { printf("%s: Lock %#lx\n", name, hr); return; }
    int w = d.dwWidth, h = d.dwHeight, bpp = d.ddpfPixelFormat.dwRGBBitCount;
    printf("%s: lock pitch>=row %d bpp %d\n", name, d.lPitch >= w * bpp / 8, bpp);
    unsigned char *px = malloc(w * h * 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            unsigned char *p = (unsigned char *)d.lpSurface + y * d.lPitch, *o = px + (y * w + x) * 4;
            unsigned v = bpp == 32 ? ((DWORD *)p)[x] : bpp == 16 ? ((WORD *)p)[x] : p[x];
            if (bpp == 8) { o[0] = o[1] = o[2] = v; continue; }
            DWORD m[3] = { d.ddpfPixelFormat.dwBBitMask, d.ddpfPixelFormat.dwGBitMask, d.ddpfPixelFormat.dwRBitMask };
            for (int k = 0; k < 3; k++) {
                DWORD mk = m[k];
                int sh = 0, bits = 0;
                while (mk && !(mk & 1)) { mk >>= 1; sh++; }
                while (mk & 1) { mk >>= 1; bits++; }
                unsigned c = (v >> sh) & ((1u << bits) - 1);
                o[k] = bits ? c * 255 / ((1u << bits) - 1) : 0;
            }
        }
    IDirectDrawSurface7_Unlock(s, NULL);
    probe_image(name, px, w, h, w * 4, 0);
    free(px);
}

/* fills a surface with a pattern through Lock (bpp-sized texels) */
static void pattern(IDirectDrawSurface7 *s, int seed) {
    DDSURFACEDESC2 d = { sizeof d };
    if (FAILED(IDirectDrawSurface7_Lock(s, NULL, &d, DDLOCK_WAIT | DDLOCK_WRITEONLY, NULL))) { puts("pattern: Lock failed"); return; }
    int bpp = d.ddpfPixelFormat.dwRGBBitCount;
    for (DWORD y = 0; y < d.dwHeight; y++)
        for (DWORD x = 0; x < d.dwWidth; x++) {
            unsigned char *p = (unsigned char *)d.lpSurface + y * d.lPitch;
            unsigned v = (x * 2654435761u + y * 40503u + seed * 977u) >> 7;
            if ((x + y + seed) % 5 == 0) v = 0; /* the key color shows up */
            if (bpp == 32) ((DWORD *)p)[x] = v & 0xffffff;
            else if (bpp == 16) ((WORD *)p)[x] = v;
            else p[x] = v;
        }
    IDirectDrawSurface7_Unlock(s, NULL);
}

static void fill(IDirectDrawSurface7 *s, RECT *r, DWORD color) {
    DDBLTFX fx = { sizeof fx };
    fx.dwFillColor = color;
    HRESULT hr = IDirectDrawSurface7_Blt(s, r, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
    if (FAILED(hr)) printf("fill %#lx\n", hr);
}

static void blits(const char *tag, int bpp, DWORD r, DWORD g, DWORD b, DWORD key, DWORD fillc) {
    IDirectDrawSurface7 *dst = surface(SW, SH, bpp, r, g, b), *src = surface(8, 6, bpp, r, g, b);
    if (!dst || !src) return;
    char n[64];
    DDSURFACEDESC2 d = { sizeof d };
    IDirectDrawSurface7_GetSurfaceDesc(dst, &d);
    printf("%s: desc flags %#lx caps %#lx %lux%lu bpp %lu masks %lx %lx %lx\n", tag, d.dwFlags & ~DDSD_PITCH, d.ddsCaps.dwCaps, d.dwWidth, d.dwHeight,
           d.ddpfPixelFormat.dwRGBBitCount, d.ddpfPixelFormat.dwRBitMask, d.ddpfPixelFormat.dwGBitMask, d.ddpfPixelFormat.dwBBitMask);
    pattern(src, 1);
    fill(dst, NULL, fillc);
    RECT fr = { 20, 1, 23, 15 };
    fill(dst, &fr, fillc ^ 0x5555);
    /* copy 1:1 at a corner, stretched up 2x and down, a sub-rect */
    RECT d1 = { 0, 0, 8, 6 }, d2 = { 8, 0, 20, 9 }, d3 = { 0, 7, 5, 11 }, s3 = { 2, 1, 7, 5 }, d4 = { 6, 7, 9, 9 };
    printf("%s: Blt copy %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d1, src, NULL, DDBLT_WAIT, NULL));
    printf("%s: Blt stretch %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d2, src, NULL, DDBLT_WAIT, NULL));
    printf("%s: Blt sub %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d3, src, &s3, DDBLT_WAIT, NULL));
    printf("%s: Blt shrink %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d4, src, NULL, DDBLT_WAIT, NULL));
    /* source color key: Blt and BltFast */
    DDCOLORKEY ck = { key, key };
    printf("%s: SetColorKey %#lx\n", tag, IDirectDrawSurface7_SetColorKey(src, DDCKEY_SRCBLT, &ck));
    RECT d5 = { 10, 10, 18, 16 };
    printf("%s: Blt keysrc %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d5, src, NULL, DDBLT_WAIT | DDBLT_KEYSRC, NULL));
    printf("%s: BltFast srccolorkey %#lx\n", tag, IDirectDrawSurface7_BltFast(dst, 16, 9, src, NULL, DDBLTFAST_WAIT | DDBLTFAST_SRCCOLORKEY));
    /* mirroring through DDBLTFX */
    DDBLTFX fx = { sizeof fx };
    fx.dwDDFX = DDBLTFX_MIRRORLEFTRIGHT;
    RECT d6 = { 0, 11, 8, 16 };
    RECT s6 = { 0, 0, 8, 5 };
    printf("%s: Blt mirror %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &d6, src, &s6, DDBLT_WAIT | DDBLT_DDFX, &fx));
    /* errors: a rect past the surface, BltFast past the edge, an empty rect */
    RECT bad = { 20, 10, 30, 20 }, empty = { 3, 3, 3, 5 };
    printf("%s: Blt outside %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &bad, src, NULL, DDBLT_WAIT, NULL));
    printf("%s: BltFast edge %#lx\n", tag, IDirectDrawSurface7_BltFast(dst, 20, 12, src, NULL, DDBLTFAST_WAIT));
    printf("%s: Blt empty %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &empty, src, NULL, DDBLT_WAIT, NULL));
    snprintf(n, sizeof n, "%s_blits", tag);
    show(n, dst);
    /* within one surface: overlapping moves right/down and left/up */
    RECT a = { 0, 0, 10, 8 }, b2 = { 2, 1, 12, 9 };
    printf("%s: Blt overlap %#lx\n", tag, IDirectDrawSurface7_Blt(dst, &b2, dst, &a, DDBLT_WAIT, NULL));
    RECT c = { 14, 7, 22, 14 };
    printf("%s: BltFast overlap %#lx\n", tag, IDirectDrawSurface7_BltFast(dst, 12, 6, dst, &c, DDBLTFAST_WAIT));
    snprintf(n, sizeof n, "%s_overlap", tag);
    show(n, dst);
    /* GDI on the surface */
    HDC dc = NULL;
    HRESULT hr = IDirectDrawSurface7_GetDC(dst, &dc);
    printf("%s: GetDC %#lx\n", tag, hr);
    if (SUCCEEDED(hr)) {
        HBRUSH br = CreateSolidBrush(RGB(255, 128, 0));
        RECT gr = { 3, 3, 15, 10 };
        FillRect(dc, &gr, br);
        DeleteObject(br);
        SetPixel(dc, 1, 14, RGB(0, 0, 255));
        MoveToEx(dc, 0, 15, NULL);
        LineTo(dc, 23, 0);
        printf("%s: GetPixel %06lx\n", tag, GetPixel(dc, 5, 5));
        IDirectDrawSurface7_ReleaseDC(dst, dc);
        snprintf(n, sizeof n, "%s_gdi", tag);
        show(n, dst);
    }
    IDirectDrawSurface7_Release(src);
    IDirectDrawSurface7_Release(dst);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "ddraw", WS_OVERLAPPEDWINDOW, 0, 0, 320, 240, 0, 0, wc.hInstance, 0);
    HRESULT hr = DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL);
    if (FAILED(hr)) { printf("skip DirectDrawCreateEx %#lx\n", hr); return 0; }
    printf("SetCooperativeLevel %#lx\n", IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL));
    DDCAPS hal = { sizeof hal }, hel = { sizeof hel };
    hr = IDirectDraw7_GetCaps(dd, &hal, &hel);
    printf("GetCaps %#lx hel blt %d colorkey %d stretch %d\n", hr, !!(hel.dwCaps & DDCAPS_BLT), !!(hel.dwCaps & DDCAPS_COLORKEY), !!(hel.dwCaps & DDCAPS_BLTSTRETCH));
    DDSURFACEDESC2 mode = { sizeof mode };
    hr = IDirectDraw7_GetDisplayMode(dd, &mode);
    printf("GetDisplayMode %#lx bpp %lu\n", hr, mode.ddpfPixelFormat.dwRGBBitCount);

    blits("rgb32", 32, 0xff0000, 0xff00, 0xff, 0, 0x203040);
    blits("rgb565", 16, 0xf800, 0x7e0, 0x1f, 0, 0x2945);
    blits("rgb555", 16, 0x7c00, 0x3e0, 0x1f, 0, 0x14a5);

    /* 8 bpp with a palette: indices kept by blits; GetDC through the palette */
    {
        IDirectDrawSurface7 *s = surface(SW, SH, 8, 0, 0, 0);
        if (s) {
            PALETTEENTRY pe[256];
            for (int i = 0; i < 256; i++) pe[i] = (PALETTEENTRY){ (BYTE)i, (BYTE)(255 - i), (BYTE)(i * 3), 0 };
            IDirectDrawPalette *pal = NULL;
            hr = IDirectDraw7_CreatePalette(dd, DDPCAPS_8BIT | DDPCAPS_ALLOW256, pe, &pal, NULL);
            printf("CreatePalette %#lx\n", hr);
            if (pal) printf("SetPalette %#lx\n", IDirectDrawSurface7_SetPalette(s, pal));
            pattern(s, 3);
            RECT r = { 4, 4, 12, 9 };
            fill(s, &r, 77);
            HDC dc = NULL;
            if (SUCCEEDED(IDirectDrawSurface7_GetDC(s, &dc))) {
                printf("8bpp GetPixel %06lx %06lx\n", GetPixel(dc, 5, 5), GetPixel(dc, 0, 1));
                HBRUSH br = CreateSolidBrush(RGB(10, 245, 30));
                RECT gr = { 14, 2, 20, 6 };
                FillRect(dc, &gr, br);
                DeleteObject(br);
                IDirectDrawSurface7_ReleaseDC(s, dc);
            } else puts("8bpp GetDC failed");
            show("pal8", s);
            if (pal) IDirectDrawPalette_Release(pal);
            IDirectDrawSurface7_Release(s);
        }
    }
    /* a surface without a pixel format takes the display's */
    {
        DDSURFACEDESC2 d = { sizeof d };
        d.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
        d.dwWidth = 5;
        d.dwHeight = 3;
        d.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
        IDirectDrawSurface7 *s = NULL;
        hr = IDirectDraw7_CreateSurface(dd, &d, &s, NULL);
        printf("CreateSurface default format %#lx\n", hr);
        if (s) {
            DDPIXELFORMAT pf = { sizeof pf };
            IDirectDrawSurface7_GetPixelFormat(s, &pf);
            printf("default format flags %#lx bpp %lu masks %lx %lx %lx\n", pf.dwFlags, pf.dwRGBBitCount, pf.dwRBitMask, pf.dwGBitMask, pf.dwBBitMask);
            IDirectDrawSurface7_Release(s);
        }
    }
    printf("Release %lu\n", IDirectDraw7_Release(dd));
    return 0;
}
