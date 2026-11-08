/* replay.c's D3D8 pictures: the back buffer before each Present (f<n>), a surface the recording
 * locked to read (l<call>: a D3DLOCK_READONLY LockRect, as after CopyRects into an image
 * surface), the render target after each draw (-draws: d<call>) */
#define COBJMACROS
#include <stdio.h>
#include <d3d8.h>
#include "replay.h"

static void dump_bits(const char *name, D3DFORMAT f, const void *bits, int w, int h, int pitch) {
    if (f == D3DFMT_X8R8G8B8 || f == D3DFMT_A8R8G8B8) image_out(name, bits, w, h, pitch, IMG_BGRX);
    else if (f == D3DFMT_R5G6B5) image_out(name, bits, w, h, pitch, IMG_565);
    else if (f == D3DFMT_X1R5G5B5 || f == D3DFMT_A1R5G5B5) image_out(name, bits, w, h, pitch, IMG_X555);
    else printf("image %s: format %u\n", name, (unsigned)f);
}

/* a surface's pixels, through an image surface (CopyRects: D3D8 copies any surface to one) */
static void dump_target(IDirect3DDevice8 *dev, const char *name, IDirect3DSurface8 *s) {
    D3DSURFACE_DESC d;
    D3DLOCKED_RECT lr;
    IDirect3DSurface8 *sys = NULL;
    HRESULT hr;
    if (opt_noimages) return;
    if (FAILED(IDirect3DSurface8_GetDesc(s, &d))) { printf("image %s: GetDesc failed\n", name); return; }
    hr = IDirect3DDevice8_CreateImageSurface(dev, d.Width, d.Height, d.Format, &sys);
    if (SUCCEEDED(hr)) hr = IDirect3DDevice8_CopyRects(dev, s, NULL, 0, sys, NULL);
    if (SUCCEEDED(hr)) hr = IDirect3DSurface8_LockRect(sys, &lr, NULL, D3DLOCK_READONLY);
    if (FAILED(hr)) printf("image %s: CopyRects %#lx\n", name, hr);
    else {
        dump_bits(name, d.Format, lr.pBits, d.Width, d.Height, lr.Pitch);
        IDirect3DSurface8_UnlockRect(sys);
    }
    if (sys) IDirect3DSurface8_Release(sys);
}

void d3d8_before(const char *iface, const char *method, W *a, int n, UINT no) {
    char name[32];
    IDirect3DSurface8 *bb = NULL;
    if (strcmp(method, "Present") || strcmp(iface, "IDirect3DDevice8")) return;
    if (frame_wanted()) {
        sprintf(name, "f%d", frame);
        HRESULT hr = IDirect3DDevice8_GetBackBuffer((IDirect3DDevice8 *)a[0], 0, D3DBACKBUFFER_TYPE_MONO, &bb);
        if (FAILED(hr)) printf("image %s: GetBackBuffer %#lx\n", name, hr);
        else dump_target((IDirect3DDevice8 *)a[0], name, bb);
        if (bb) IDirect3DSurface8_Release(bb);
    }
    frame++;
}

void d3d8_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr) {
    char img[32];
    if (FAILED(hr)) return;
    if (!strcmp(iface, "IDirect3DSurface8") && !strcmp(method, "LockRect") && n >= 4 && (a[3] & D3DLOCK_READONLY)) {
        /* (the lock is the recording's own: read it before its UnlockRect) */
        D3DSURFACE_DESC d;
        D3DLOCKED_RECT *lr = (D3DLOCKED_RECT *)a[1];
        if (opt_noimages || !lr || FAILED(IDirect3DSurface8_GetDesc((IDirect3DSurface8 *)a[0], &d))) return;
        sprintf(img, "l%u", no);
        dump_bits(img, d.Format, lr->pBits, d.Width, d.Height, lr->Pitch);
    } else if (draws_wanted() && !strncmp(method, "Draw", 4) && !strcmp(iface, "IDirect3DDevice8")) {
        IDirect3DSurface8 *rt = NULL;
        if (SUCCEEDED(IDirect3DDevice8_GetRenderTarget((IDirect3DDevice8 *)a[0], &rt))) {
            sprintf(img, "d%u", no);
            dump_target((IDirect3DDevice8 *)a[0], img, rt);
            IDirect3DSurface8_Release(rt);
        }
    }
}
