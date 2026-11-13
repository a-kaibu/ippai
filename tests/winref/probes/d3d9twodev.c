/* Two Direct3D 9 devices of one IDirect3D9 on two windows, the second one released while the
 * first keeps drawing (KiriKiri Z's D3D draw device makes one per window: 姫騎士ド下品敗北人生's
 * small 238x90 window got a device that was released, and the main window's device drew
 * nothing after that, its title black). Per frame as KiriKiri: a managed texture locked and
 * filled, BeginScene, SetTexture, a textured quad, EndScene, Present. The results of each call
 * and the main back buffer's pixels after the second device is gone.
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 64
#define H 32

struct v { float x, y, z, rhw, u, t; };

static IDirect3DDevice9 *mk(IDirect3D9 *d3d, HWND h, int w, int hh) {
    D3DPRESENT_PARAMETERS pp = { 0 };
    /* (KiriKiri: a back buffer the size of the desktop, presented from a source rectangle) */
    pp.BackBufferWidth = w; pp.BackBufferHeight = hh; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_COPY; pp.hDeviceWindow = h; pp.Windowed = TRUE;
    IDirect3DDevice9 *dev = NULL;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, h, D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_MULTITHREADED, &pp, &dev);
    printf("CreateDevice %#lx\n", hr);
    return dev;
}

/* one KiriKiri frame: fill the texture with c, draw it over w x h; the results of each call */
static void frame(const char *what, IDirect3DDevice9 *dev, IDirect3DTexture9 *tex, int w, int h, DWORD c) {
    D3DLOCKED_RECT lr;
    HRESULT lk = IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0);
    if (SUCCEEDED(lk)) {
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++) ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] = c;
        IDirect3DTexture9_UnlockRect(tex, 0);
    }
    D3DVIEWPORT9 vp = { 0, 0, w, h, 0, 1 };
    HRESULT v = IDirect3DDevice9_SetViewport(dev, &vp);
    HRESULT b = IDirect3DDevice9_BeginScene(dev);
    struct v q[4] = { { -0.5f, -0.5f, 0, 1, 0, 0 }, { w - 0.5f, -0.5f, 0, 1, 1, 0 }, { -0.5f, h - 0.5f, 0, 1, 0, 1 }, { w - 0.5f, h - 0.5f, 0, 1, 1, 1 } };
    HRESULT t = IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1);
    HRESULT d = IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    HRESULT e = IDirect3DDevice9_EndScene(dev);
    RECT src = { 0, 0, w, h };
    HRESULT p = IDirect3DDevice9_Present(dev, &src, NULL, NULL, NULL);
    printf("%s: lock %#lx viewport %#lx begin %#lx texture %#lx draw %#lx end %#lx present %#lx\n", what, lk, v, b, t, d, e, p);
}

static DWORD pixel(IDirect3DDevice9 *dev, int x, int y) {
    IDirect3DSurface9 *back = NULL, *sys = NULL;
    D3DSURFACE_DESC desc;
    DWORD r = 0xbad0bad0;
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DSurface9_GetDesc(back, &desc);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, desc.Width, desc.Height, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    D3DLOCKED_RECT lr;
    if (SUCCEEDED(IDirect3DDevice9_GetRenderTargetData(dev, back, sys)) && SUCCEEDED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) {
        r = ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff;
        IDirect3DSurface9_UnlockRect(sys);
    }
    IDirect3DSurface9_Release(sys);
    IDirect3DSurface9_Release(back);
    return r;
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND a = CreateWindowA("probe", "main", WS_POPUP | WS_VISIBLE, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    HWND b = CreateWindowA("probe", "small", WS_POPUP | WS_VISIBLE, 100, 100, 24, 10, a, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    /* both back buffers larger than their windows, the same size (as KiriKiri's desktop size) */
    IDirect3DDevice9 *da = mk(d3d, a, 2 * W, 2 * H);
    if (!da) { puts("skip CreateDevice"); return 0; }
    IDirect3DTexture9 *ta = NULL, *tb = NULL;
    printf("texture a %#lx\n", IDirect3DDevice9_CreateTexture(da, W, H, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &ta, NULL));
    frame("a 1", da, ta, W, H, 0xff102030);
    printf("a pixel %06lx\n", pixel(da, 5, 5));
    IDirect3DDevice9 *db = mk(d3d, b, 2 * W, 2 * H);
    printf("texture b %#lx\n", IDirect3DDevice9_CreateTexture(db, 24, 10, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &tb, NULL));
    frame("b 1", db, tb, 24, 10, 0xff405060);
    frame("a 2", da, ta, W, H, 0xff708090);
    printf("a pixel %06lx b pixel %06lx\n", pixel(da, 5, 5), pixel(db, 5, 5));
    printf("release texture b %lu\n", IDirect3DTexture9_Release(tb));
    printf("release device b %lu\n", IDirect3DDevice9_Release(db));
    DestroyWindow(b);
    for (int i = 3; i <= 5; i++) {
        char n[16];
        snprintf(n, sizeof n, "a %d", i);
        frame(n, da, ta, W, H, 0xff000000 | (0x30 * i) << 16 | 0x40 << 8 | 0x50);
    }
    printf("a pixel after b %06lx\n", pixel(da, 5, 5));
    printf("release texture a %lu\n", IDirect3DTexture9_Release(ta));
    printf("release device a %lu\n", IDirect3DDevice9_Release(da));
    printf("release d3d %lu\n", IDirect3D9_Release(d3d));
    return 0;
}
