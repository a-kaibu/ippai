/* replay.c's D3D9 pictures: the back buffer before each Present (f<n>), what a
 * GetRenderTargetData read back (r<call>), the render target after each draw (-draws: d<call>) */
#define COBJMACROS
#include <stdio.h>
#include <d3d9.h>
#include "replay.h"

static void dump_surface(const char *name, IDirect3DSurface9 *s) {
    D3DSURFACE_DESC d;
    D3DLOCKED_RECT lr;
    if (opt_noimages) return;
    if (FAILED(IDirect3DSurface9_GetDesc(s, &d))) { printf("image %s: GetDesc failed\n", name); return; }
    if (FAILED(IDirect3DSurface9_LockRect(s, &lr, NULL, D3DLOCK_READONLY))) { printf("image %s: LockRect failed\n", name); return; }
    if (d.Format == D3DFMT_X8R8G8B8 || d.Format == D3DFMT_A8R8G8B8) image_out(name, lr.pBits, d.Width, d.Height, lr.Pitch, IMG_BGRX);
    else if (d.Format == D3DFMT_R5G6B5) image_out(name, lr.pBits, d.Width, d.Height, lr.Pitch, IMG_565);
    else if (d.Format == D3DFMT_X1R5G5B5 || d.Format == D3DFMT_A1R5G5B5) image_out(name, lr.pBits, d.Width, d.Height, lr.Pitch, IMG_X555);
    else printf("image %s: format %u\n", name, (unsigned)d.Format);
    IDirect3DSurface9_UnlockRect(s);
}

/* a render target's pixels, through a system-memory copy (and a plain one first if it is
 * multisampled) */
static void dump_target(IDirect3DDevice9 *dev, const char *name, IDirect3DSurface9 *rt) {
    D3DSURFACE_DESC d;
    IDirect3DSurface9 *plain = NULL, *sys = NULL;
    HRESULT hr;
    if (opt_noimages) return;
    if (FAILED(IDirect3DSurface9_GetDesc(rt, &d))) { printf("image %s: GetDesc failed\n", name); return; }
    if (d.MultiSampleType != D3DMULTISAMPLE_NONE) {
        hr = IDirect3DDevice9_CreateRenderTarget(dev, d.Width, d.Height, d.Format, D3DMULTISAMPLE_NONE, 0, FALSE, &plain, NULL);
        if (FAILED(hr)) { printf("image %s: CreateRenderTarget %#lx\n", name, hr); return; }
        IDirect3DDevice9_StretchRect(dev, rt, NULL, plain, NULL, D3DTEXF_NONE);
        rt = plain;
    }
    hr = IDirect3DDevice9_CreateOffscreenPlainSurface(dev, d.Width, d.Height, d.Format, D3DPOOL_SYSTEMMEM, &sys, NULL);
    if (SUCCEEDED(hr)) hr = IDirect3DDevice9_GetRenderTargetData(dev, rt, sys);
    if (FAILED(hr)) printf("image %s: GetRenderTargetData %#lx\n", name, hr);
    else dump_surface(name, sys);
    if (sys) IDirect3DSurface9_Release(sys);
    if (plain) IDirect3DSurface9_Release(plain);
}

void d3d9_before(const char *iface, const char *method, W *a, int n, UINT no) {
    char name[32];
    IDirect3DSurface9 *bb = NULL;
    IDirect3DDevice9 *dev = NULL;
    HRESULT hr;
    if (strcmp(method, "Present") && strcmp(method, "PresentEx")) return;
    if (frame_wanted()) {
        sprintf(name, "f%d", frame);
        if (strstr(iface, "SwapChain9")) {
            hr = IDirect3DSwapChain9_GetBackBuffer((IDirect3DSwapChain9 *)a[0], 0, D3DBACKBUFFER_TYPE_MONO, &bb);
            if (SUCCEEDED(hr)) hr = IDirect3DSwapChain9_GetDevice((IDirect3DSwapChain9 *)a[0], &dev);
        } else {
            dev = (IDirect3DDevice9 *)a[0];
            IDirect3DDevice9_AddRef(dev);
            hr = IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &bb);
        }
        if (FAILED(hr)) printf("image %s: GetBackBuffer %#lx\n", name, hr);
        else dump_target(dev, name, bb);
        if (bb) IDirect3DSurface9_Release(bb);
        if (dev) IDirect3DDevice9_Release(dev);
    }
    frame++;
}

void d3d9_after(const char *iface, const char *method, W *a, int n, UINT no, HRESULT hr) {
    char img[32];
    if (FAILED(hr)) return;
    if (!strcmp(method, "GetRenderTargetData") && n >= 3 && a[2]) {
        sprintf(img, "r%u", no);
        dump_surface(img, (IDirect3DSurface9 *)a[2]);
    } else if (draws_wanted() && !strncmp(method, "Draw", 4) && strstr(iface, "Device9")) {
        IDirect3DSurface9 *rt = NULL;
        if (SUCCEEDED(IDirect3DDevice9_GetRenderTarget((IDirect3DDevice9 *)a[0], 0, &rt))) {
            sprintf(img, "d%u", no);
            dump_target((IDirect3DDevice9 *)a[0], img, rt);
            IDirect3DSurface9_Release(rt);
        }
    }
}
