/* replay.c's DirectDraw pictures (any interface version: each surface is looked at through
 * IDirectDrawSurface7):
 *   f<n>     the back buffer before a Flip of the primary surface, or the part of the primary
 *            a Blt / BltFast drew to (in a window the primary is the screen); a frame each
 *   l<call>  a surface the recording locked to read (DDLOCK_READONLY), the locked rect
 *   d<call>  (-draws) the surface a Blt / BltFast drew to, after it
 * Pixels go to 8 bits a channel as tests/winref/probes/ddraw.c shows them (c * 255 / max of the
 * channel's mask), so that a replay of that probe draws its pictures pixel for pixel; an 8-bit
 * surface's l<call> is its indices (as gray, as the probe has them), f<n> and d<call> are the
 * colors of the palette it (or the primary) has. */
#define COBJMACROS
#include <stdio.h>
#include <ddraw.h>
#include "replay.h"

void ddraw_lock_prep(int v2, void *desc) {
    *(DWORD *)desc = v2 ? sizeof(DDSURFACEDESC2) : sizeof(DDSURFACEDESC);
}

void ddraw_lock_got(int v2, const void *desc, void **bits, INT *pitch) {
    if (v2) { const DDSURFACEDESC2 *d = desc; *bits = d->lpSurface; *pitch = d->lPitch; }
    else { const DDSURFACEDESC *d = desc; *bits = d->lpSurface; *pitch = d->lPitch; }
}

static IDirectDrawSurface7 *v7(W p) {
    IDirectDrawSurface7 *s = NULL;
    if (p && FAILED(IUnknown_QueryInterface((IUnknown *)p, &IID_IDirectDrawSurface7, (void **)&s))) s = NULL;
    return s;
}

/* the 256 colors of a surface's palette (0: none) */
static int palette_of(IDirectDrawSurface7 *s, PALETTEENTRY *pe) {
    IDirectDrawPalette *p = NULL;
    if (!s || FAILED(IDirectDrawSurface7_GetPalette(s, &p)) || !p) return 0;
    HRESULT hr = IDirectDrawPalette_GetEntries(p, 0, 0, 256, pe);
    IDirectDrawPalette_Release(p);
    return SUCCEEDED(hr);
}

/* w x h pixels at bits (pitch, format pf) as a picture; pe: an 8-bit surface's colors (NULL:
 * its indices as gray) */
static void picture(const char *name, const BYTE *bits, int w, int h, INT pitch, const DDPIXELFORMAT *pf, const PALETTEENTRY *pe) {
    int bpp = pf->dwRGBBitCount;
    if (opt_noimages) return;
    if (!(pf->dwFlags & (DDPF_RGB | DDPF_PALETTEINDEXED8)) || (bpp != 8 && bpp != 16 && bpp != 24 && bpp != 32)) {
        printf("image %s: format flags %#lx bpp %d\n", name, pf->dwFlags, bpp);
        return;
    }
    if (w <= 0 || h <= 0) return;
    BYTE *px = malloc((size_t)w * h * 4);
    DWORD m[3] = { pf->dwBBitMask, pf->dwGBitMask, pf->dwRBitMask };
    int sh[3], nb[3];
    for (int k = 0; k < 3; k++) {
        DWORD mk = m[k];
        sh[k] = nb[k] = 0;
        while (mk && !(mk & 1)) { mk >>= 1; sh[k]++; }
        while (mk & 1) { mk >>= 1; nb[k]++; }
    }
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            const BYTE *p = bits + (size_t)y * pitch;
            BYTE *o = px + ((size_t)y * w + x) * 4;
            unsigned v = bpp == 32 ? ((const DWORD *)p)[x] : bpp == 24 ? (unsigned)(p[x * 3] | p[x * 3 + 1] << 8 | p[x * 3 + 2] << 16) : bpp == 16 ? ((const WORD *)p)[x] : p[x];
            o[3] = 0;
            if (bpp == 8) {
                if (pe) { o[0] = pe[v].peBlue; o[1] = pe[v].peGreen; o[2] = pe[v].peRed; }
                else o[0] = o[1] = o[2] = v;
                continue;
            }
            for (int k = 0; k < 3; k++) {
                unsigned c = (v >> sh[k]) & ((1u << nb[k]) - 1);
                o[k] = nb[k] ? c * 255 / ((1u << nb[k]) - 1) : 0;
            }
        }
    image_out(name, px, w, h, w * 4, IMG_BGRX);
    free(px);
}

/* a rect of a surface (NULL: all of it), read through a lock; pal: whose palette an 8-bit one
 * shows with */
static void dump(const char *name, IDirectDrawSurface7 *s, RECT *r, IDirectDrawSurface7 *pal) {
    DDSURFACEDESC2 d = { sizeof d };
    PALETTEENTRY pe[256];
    if (opt_noimages || !s) return;
    HRESULT hr = IDirectDrawSurface7_Lock(s, r, &d, DDLOCK_WAIT | DDLOCK_READONLY, NULL);
    if (FAILED(hr)) { printf("image %s: Lock %#lx\n", name, hr); return; }
    int has = d.ddpfPixelFormat.dwRGBBitCount == 8 && (palette_of(s, pe) || palette_of(pal, pe));
    picture(name, d.lpSurface, r ? r->right - r->left : (int)d.dwWidth, r ? r->bottom - r->top : (int)d.dwHeight, d.lPitch, &d.ddpfPixelFormat, has ? pe : NULL);
    IDirectDrawSurface7_Unlock(s, r);
}

static int is_primary(IDirectDrawSurface7 *s) {
    DDSCAPS2 caps = { 0 };
    return s && SUCCEEDED(IDirectDrawSurface7_GetCaps(s, &caps)) && (caps.dwCaps & DDSCAPS_PRIMARYSURFACE);
}

static int is_surface(const char *iface) { return !strncmp(iface, "IDirectDrawSurface", 18); }

void ddraw_before(const char *iface, const char *method, W *a, int n, UINT no) {
    char name[32];
    if (!is_surface(iface) || strcmp(method, "Flip")) return;
    IDirectDrawSurface7 *front = v7(a[0]);
    if (!is_primary(front)) { if (front) IDirectDrawSurface7_Release(front); return; }
    if (frame_wanted()) {
        IDirectDrawSurface7 *back = n > 1 && a[1] ? v7(a[1]) : NULL;
        DDSCAPS2 caps = { DDSCAPS_BACKBUFFER };
        HRESULT hr = back ? S_OK : IDirectDrawSurface7_GetAttachedSurface(front, &caps, &back);
        sprintf(name, "f%d", frame);
        if (FAILED(hr) || !back) printf("image %s: GetAttachedSurface %#lx\n", name, hr);
        else dump(name, back, NULL, front);
        if (back) IDirectDrawSurface7_Release(back);
    }
    frame++;
    IDirectDrawSurface7_Release(front);
}

void ddraw_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr) {
    char name[32];
    if (FAILED(hr) || !is_surface(iface)) return;
    int v2 = iface[strlen(iface) - 1] == '7' || iface[strlen(iface) - 1] == '4';
    if (!strcmp(method, "Lock") && n >= 4 && (a[3] & DDLOCK_READONLY)) {
        /* (the lock the recording made, as it is: its rect, the pixels this system handed back) */
        sprintf(name, "l%u", no);
        const RECT *r = (const RECT *)a[1];
        DDPIXELFORMAT pf;
        void *bits;
        INT pitch;
        int w, h;
        if (v2) { DDSURFACEDESC2 *d = (DDSURFACEDESC2 *)a[2]; pf = d->ddpfPixelFormat; w = d->dwWidth; h = d->dwHeight; }
        else { DDSURFACEDESC *d = (DDSURFACEDESC *)a[2]; pf = d->ddpfPixelFormat; w = d->dwWidth; h = d->dwHeight; }
        ddraw_lock_got(v2, (void *)a[2], &bits, &pitch);
        if (r) { w = r->right - r->left; h = r->bottom - r->top; }
        picture(name, bits, w, h, pitch, &pf, NULL);
        return;
    }
    if (strcmp(method, "Blt") && strcmp(method, "BltFast")) return;
    IDirectDrawSurface7 *dst = v7(a[0]);
    if (!dst) return;
    DDSURFACEDESC2 d = { sizeof d };
    IDirectDrawSurface7_GetSurfaceDesc(dst, &d);
    RECT r = { 0, 0, d.dwWidth, d.dwHeight };
    if (!strcmp(method, "Blt") && a[1]) r = *(const RECT *)a[1];
    else if (!strcmp(method, "BltFast")) {
        RECT s = { 0, 0, 0, 0 };
        if (a[4]) s = *(const RECT *)a[4];
        else {
            IDirectDrawSurface7 *src = v7(a[3]);
            DDSURFACEDESC2 sd = { sizeof sd };
            if (src && SUCCEEDED(IDirectDrawSurface7_GetSurfaceDesc(src, &sd))) { s.right = sd.dwWidth; s.bottom = sd.dwHeight; }
            if (src) IDirectDrawSurface7_Release(src);
        }
        r = (RECT){ (LONG)a[1], (LONG)a[2], (LONG)a[1] + s.right - s.left, (LONG)a[2] + s.bottom - s.top };
    }
    /* (in the surface) */
    if (r.left < 0) r.left = 0;
    if (r.top < 0) r.top = 0;
    if (r.right > (LONG)d.dwWidth) r.right = d.dwWidth;
    if (r.bottom > (LONG)d.dwHeight) r.bottom = d.dwHeight;
    if (is_primary(dst)) {
        if (frame_wanted() && r.right > r.left && r.bottom > r.top) {
            sprintf(name, "f%d", frame);
            dump(name, dst, &r, dst);
        }
        frame++;
    } else if (draws_wanted()) {
        sprintf(name, "d%u", no);
        dump(name, dst, NULL, NULL);
    }
    IDirectDrawSurface7_Release(dst);
}
