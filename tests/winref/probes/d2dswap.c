/* KiriKiri's DrawDeviceD2D (AMBITIOUS MISSION after episode 1) end to end: a window, a D3D11
 * device and its DXGI factory (adapter's parent), CreateSwapChainForHwnd, an ID2D1Factory1's
 * device context, the target a bitmap of the swap chain's buffer 0 (GetBuffer as
 * IDXGISurface), the picture a DYNAMIC texture written through Map(WRITE_DISCARD) every frame,
 * DrawBitmap under a transform, EndDraw, Present. Each frame's buffer 0 is read back through a
 * staging copy before Present (what Present shows), for both the bit-blt and the flip models,
 * the bitmap of buffer 0 made once and kept across Presents as the game keeps it. As the game,
 * each frame goes through a bitmap made with D2D1_BITMAP_OPTIONS_TARGET (CreateBitmap with
 * D2D1_BITMAP_PROPERTIES1, slot 57) first: the picture drawn into it, then it onto buffer 0.
 * libs: -ld3d11 -ld2d1 -ldxgi -luuid
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <dxgi1_2.h>
#include <d2d1.h>
#include <d2d1_1.h>

#define W 64
#define H 48
#define SW 8
#define SH 6
#ifndef HOLD
#define HOLD 0
#endif

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static void shot(IDXGISwapChain1 *sc, const char *name) {
    ID3D11Texture2D *buf = NULL, *stage = NULL;
    D3D11_TEXTURE2D_DESC td;
    D3D11_MAPPED_SUBRESOURCE m;
    if (FAILED(IDXGISwapChain1_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&buf))) { printf("%s: GetBuffer failed\n", name); return; }
    ID3D11Texture2D_GetDesc(buf, &td);
    td.Usage = D3D11_USAGE_STAGING; td.BindFlags = 0; td.CPUAccessFlags = D3D11_CPU_ACCESS_READ; td.MiscFlags = 0;
    ID3D11Device_CreateTexture2D(dev, &td, NULL, &stage);
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)stage, (ID3D11Resource *)buf);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage, 0, D3D11_MAP_READ, 0, &m))) {
        probe_image(name, m.pData, W, H, m.RowPitch, 2);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage, 0);
    } else printf("%s: Map failed\n", name);
    ID3D11Texture2D_Release(stage);
    ID3D11Texture2D_Release(buf);
}

static void run(HWND hwnd, IDXGIFactory2 *fac, ID2D1Device *d2dev, DXGI_SWAP_EFFECT effect, UINT count, const char *tag) {
    DXGI_SWAP_CHAIN_DESC1 sd = { W, H, DXGI_FORMAT_B8G8R8A8_UNORM, FALSE, { 1, 0 }, DXGI_USAGE_RENDER_TARGET_OUTPUT, count, DXGI_SCALING_STRETCH, effect, DXGI_ALPHA_MODE_UNSPECIFIED, 0 };
    IDXGISwapChain1 *sc = NULL;
    HRESULT hr = IDXGIFactory2_CreateSwapChainForHwnd(fac, (IUnknown *)dev, hwnd, &sd, NULL, NULL, &sc);
    printf("%s: CreateSwapChainForHwnd %#lx\n", tag, hr);
    if (!sc) return;
    struct { ID2D1DeviceContext *dc; UINT_PTR more[6]; } holder = { NULL, { 0x0a0b0c0d, 0x0a0b0c0d, 0x40001000, 0x40001000, 0x1234, 0x5678 } };
    /* (by the vtable: ID2D1Device::CreateDeviceContext 4, ID2D1DeviceContext::SetTarget 74) */
    hr = ((HRESULT(STDMETHODCALLTYPE *)(void *, D2D1_DEVICE_CONTEXT_OPTIONS, ID2D1DeviceContext **))(*(void ***)d2dev)[4])(d2dev, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, &holder.dc);
    ID2D1DeviceContext *dc = holder.dc;
    IDXGISurface *bs = NULL, *ps = NULL;
    ID3D11Texture2D *dyn = NULL;
    ID2D1Bitmap1 *target = NULL, *pic = NULL, *mid = NULL;
    IDXGISwapChain1_GetBuffer(sc, 0, &IID_IDXGISurface, (void **)&bs);
    D3D11_TEXTURE2D_DESC dd = { SW, SH, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, D3D11_USAGE_DYNAMIC, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE, 0 };
    ID3D11Device_CreateTexture2D(dev, &dd, NULL, &dyn);
    if (dyn) ID3D11Texture2D_QueryInterface(dyn, &IID_IDXGISurface, (void **)&ps);
    if (dc && bs && ps) {
        D2D1_BITMAP_PROPERTIES1 tp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE }, 96, 96, D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, NULL };
        hr = ID2D1DeviceContext_CreateBitmapFromDxgiSurface(dc, bs, &tp, &target);
        printf("%s: target bitmap %#lx\n", tag, hr);
        D2D1_BITMAP_PROPERTIES1 pp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_IGNORE }, 96, 96, D2D1_BITMAP_OPTIONS_NONE, NULL };
        hr = ID2D1DeviceContext_CreateBitmapFromDxgiSurface(dc, ps, &pp, &pic);
        printf("%s: picture bitmap %#lx\n", tag, hr);
        /* (ID2D1DeviceContext::CreateBitmap(size, data, pitch, properties1, out): slot 57) */
        D2D1_BITMAP_PROPERTIES1 mp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED }, 96, 96, D2D1_BITMAP_OPTIONS_TARGET, NULL };
        D2D1_SIZE_U ms = { W, H };
        hr = ((HRESULT(STDMETHODCALLTYPE *)(void *, D2D1_SIZE_U, const void *, UINT32, const D2D1_BITMAP_PROPERTIES1 *, ID2D1Bitmap1 **))(*(void ***)dc)[57])(dc, ms, NULL, 0, &mp, &mid);
        printf("%s: target bitmap of its own %#lx\n", tag, hr);
    }
    static const UINT colors[3][4] = {
        { 0x00ff0000, 0x0000ff00, 0x000000ff, 0x00808080 },
        { 0x00ffff00, 0x00ffff00, 0x0000ffff, 0x0000ffff },
        { 0x00ffffff, 0x00000000, 0x00ff00ff, 0x00400000 },
    };
    for (int frame = 0; frame < 3 && target && pic && mid; frame++) {
        D3D11_MAPPED_SUBRESOURCE m;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)dyn, 0, D3D11_MAP_WRITE_DISCARD, 0, &m);
        if (FAILED(hr)) { printf("%s: Map %#lx\n", tag, hr); break; }
        /* (alpha 0: ignored) quadrants */
        for (int y = 0; y < SH; y++)
            for (int x = 0; x < SW; x++)
                ((UINT *)((BYTE *)m.pData + y * m.RowPitch))[x] = colors[frame][(y >= SH / 2) * 2 + (x >= SW / 2)];
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)dyn, 0);
        ID2D1RenderTarget *r = (ID2D1RenderTarget *)dc;
        D2D1_COLOR_F black = { 0, 0, 0, 1 };
        D2D1_MATRIX_3X2_F zoom = { { { 1, 0, 0, 1, 0, 4 } } }, ident = { { { 1, 0, 0, 1, 0, 0 } } };
        D2D1_RECT_F d = { 0, 0, W, H - 8 }, s = { 0, 0, SW, SH }, all = { 0, 0, W, H };
        /* the frame into the bitmap of its own: the picture zoomed below a 4-pixel strip */
        ((void(STDMETHODCALLTYPE *)(void *, void *))(*(void ***)dc)[74])(dc, mid);
        ID2D1RenderTarget_BeginDraw(r);
        ID2D1RenderTarget_Clear(r, &black);
        ID2D1RenderTarget_SetTransform(r, &zoom);
        ID2D1RenderTarget_DrawBitmap(r, (ID2D1Bitmap *)pic, &d, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, &s);
        ID2D1RenderTarget_SetTransform(r, &ident);
        hr = ID2D1RenderTarget_EndDraw(r, NULL, NULL);
        if (hr) printf("%s: EndDraw (bitmap) %#lx\n", tag, hr);
        /* then that onto buffer 0 */
        ((void(STDMETHODCALLTYPE *)(void *, void *))(*(void ***)dc)[74])(dc, target);
        ID2D1RenderTarget_BeginDraw(r);
        D2D1_COLOR_F blue = { 0, 0, 1, 1 };
        ID2D1RenderTarget_Clear(r, &blue);
        ID2D1RenderTarget_DrawBitmap(r, (ID2D1Bitmap *)mid, &all, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
        hr = ID2D1RenderTarget_EndDraw(r, NULL, NULL);
        if (hr) printf("%s: EndDraw %#lx\n", tag, hr);
        /* (the context lets go of its target before Present, as DrawDeviceD2D does) */
        ((void(STDMETHODCALLTYPE *)(void *, void *))(*(void ***)dc)[74])(dc, NULL);
        char name[64];
        snprintf(name, sizeof name, "%s_frame%d", tag, frame);
        shot(sc, name);
        hr = IDXGISwapChain1_Present(sc, 0, 0);
        if (hr) printf("%s: Present %#lx\n", tag, hr);
        /* (built with -DHOLD=1: time for a look at the page) */
        if (HOLD) { printf("hold %s\n", name); Sleep(3000); }
    }
    if (mid) ID2D1Bitmap_Release((ID2D1Bitmap *)mid);
    if (pic) ID2D1Bitmap_Release((ID2D1Bitmap *)pic);
    if (target) ID2D1Bitmap_Release((ID2D1Bitmap *)target);
    if (dc) ID2D1RenderTarget_Release((ID2D1RenderTarget *)dc);
    if (ps) IDXGISurface_Release(ps);
    if (bs) IDXGISurface_Release(bs);
    if (dyn) ID3D11Texture2D_Release(dyn);
    IDXGISwapChain1_Release(sc);
}

int main(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "probe";
    RegisterClassA(&wc);
    RECT wr = { 0, 0, W, H };
    AdjustWindowRect(&wr, WS_OVERLAPPEDWINDOW, FALSE);
    HWND hwnd = CreateWindowA("probe", "d2dswap", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 40, 40, wr.right - wr.left, wr.bottom - wr.top, 0, 0, wc.hInstance, 0);
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    IDXGIDevice *xd = NULL;
    IDXGIAdapter *ad = NULL;
    IDXGIFactory2 *fac = NULL;
    ID3D11Device_QueryInterface(dev, &IID_IDXGIDevice, (void **)&xd);
    if (xd) IDXGIDevice_GetAdapter(xd, &ad);
    if (ad) IDXGIAdapter_GetParent(ad, &IID_IDXGIFactory2, (void **)&fac);
    printf("factory %d\n", fac != NULL);
    ID2D1Factory1 *f1 = NULL;
    ID2D1Device *d2dev = NULL;
    D2D1_FACTORY_OPTIONS fo = { D2D1_DEBUG_LEVEL_NONE };
    hr = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &IID_ID2D1Factory1, &fo, (void **)&f1);
    /* (ID2D1Factory1::CreateDevice: slot 17) */
    if (f1 && xd) hr = ((HRESULT(STDMETHODCALLTYPE *)(void *, IDXGIDevice *, ID2D1Device **))(*(void ***)f1)[17])(f1, xd, &d2dev);
    printf("CreateDevice %#lx\n", hr);
    if (fac && d2dev) {
        run(hwnd, fac, d2dev, DXGI_SWAP_EFFECT_DISCARD, 1, "discard");
        run(hwnd, fac, d2dev, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, 2, "flip");
    }
    if (d2dev) ((ULONG(STDMETHODCALLTYPE *)(void *))(*(void ***)d2dev)[2])(d2dev);
    if (f1) ((ULONG(STDMETHODCALLTYPE *)(void *))(*(void ***)f1)[2])(f1);
    if (fac) IDXGIFactory2_Release(fac);
    if (ad) IDXGIAdapter_Release(ad);
    if (xd) IDXGIDevice_Release(xd);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
    DestroyWindow(hwnd);
    return 0;
}
