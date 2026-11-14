/* Additional swap chains the way ANGLE (Ren'Py 6/7's libGLESv2, RJ112723 #146) makes them: a
 * Direct3D9Ex device 1x1 on a hidden window, then for the EGL window surface
 * CreateAdditionalSwapChain on the real window with a size of 0 and D3DFMT_UNKNOWN. What is
 * written back to the parameters, GetPresentParameters, the chain's back buffer (its size and
 * format, not the device's), GetNumberOfSwapChains / GetSwapChain, the parameters refused, and
 * a frame drawn into the chain's back buffer and presented by the chain: its pixels.
 * libs: -ld3d9 -ldxguid
 * run: browser (sake's Direct3D 9 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

typedef HRESULT(WINAPI *create9ex)(UINT, IDirect3D9Ex **);

static void params(const char *what, const D3DPRESENT_PARAMETERS *p) {
    printf("%s: %ux%u format %u count %u effect %u windowed %d autodepth %d\n", what, p->BackBufferWidth, p->BackBufferHeight,
           p->BackBufferFormat, p->BackBufferCount, p->SwapEffect, p->Windowed, p->EnableAutoDepthStencil);
}

static DWORD pixel(IDirect3DDevice9 *dev, IDirect3DSurface9 *rt, int x, int y) {
    D3DSURFACE_DESC d;
    IDirect3DSurface9_GetDesc(rt, &d);
    IDirect3DSurface9 *sys = NULL;
    DWORD r = 0xbad0bad0;
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, d.Width, d.Height, d.Format, D3DPOOL_SYSTEMMEM, &sys, NULL);
    D3DLOCKED_RECT lr;
    if (sys && SUCCEEDED(IDirect3DDevice9_GetRenderTargetData(dev, rt, sys)) && SUCCEEDED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) {
        r = ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff;
        IDirect3DSurface9_UnlockRect(sys);
    }
    if (sys) IDirect3DSurface9_Release(sys);
    return r;
}

static void refuse(IDirect3DDevice9 *dev, HWND h, const char *what, UINT count, D3DSWAPEFFECT effect, D3DFORMAT f, BOOL windowed) {
    D3DPRESENT_PARAMETERS p = { 0 };
    p.BackBufferWidth = 32; p.BackBufferHeight = 16; p.BackBufferFormat = f; p.BackBufferCount = count;
    p.SwapEffect = effect; p.hDeviceWindow = h; p.Windowed = windowed;
    IDirect3DSwapChain9 *sc = (IDirect3DSwapChain9 *)1;
    HRESULT hr = IDirect3DDevice9_CreateAdditionalSwapChain(dev, &p, &sc);
    printf("%s: %#lx %s\n", what, hr, sc == NULL ? "NULL" : sc == (void *)1 ? "untouched" : "made");
    if (SUCCEEDED(hr) && sc && sc != (void *)1) IDirect3DSwapChain9_Release(sc);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    /* (ANGLE's hidden window, and the program's: a 96x48 client area) */
    HWND hidden = CreateWindowA("probe", "AngleHiddenWindow", WS_DISABLED | WS_POPUP, 0, 0, 1, 1, 0, 0, wc.hInstance, 0);
    HWND win = CreateWindowA("probe", "d3d9swapchain", WS_POPUP | WS_VISIBLE, 0, 0, 96, 48, 0, 0, wc.hInstance, 0);
    create9ex f = (create9ex)GetProcAddress(GetModuleHandleA("d3d9.dll"), "Direct3DCreate9Ex");
    IDirect3D9Ex *ex = NULL;
    IDirect3D9 *d3d = NULL;
    if (f && SUCCEEDED(f(D3D_SDK_VERSION, &ex))) IDirect3D9Ex_QueryInterface(ex, &IID_IDirect3D9, (void **)&d3d);
    else d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = 1; pp.BackBufferHeight = 1; pp.BackBufferFormat = D3DFMT_UNKNOWN; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hidden; pp.Windowed = TRUE;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    IDirect3DDevice9 *dev = NULL;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hidden,
                                         D3DCREATE_FPU_PRESERVE | D3DCREATE_NOWINDOWCHANGES | D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    params("device", &pp);

    D3DPRESENT_PARAMETERS sp = { 0 };
    sp.BackBufferFormat = D3DFMT_UNKNOWN; sp.BackBufferCount = 1; sp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    sp.hDeviceWindow = win; sp.Windowed = TRUE; sp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    IDirect3DSwapChain9 *sc = NULL;
    hr = IDirect3DDevice9_CreateAdditionalSwapChain(dev, &sp, &sc);
    printf("CreateAdditionalSwapChain %#lx\n", hr);
    if (FAILED(hr)) return 0;
    params("written back", &sp);
    D3DPRESENT_PARAMETERS got;
    memset(&got, 0xcc, sizeof got);
    printf("GetPresentParameters %#lx\n", IDirect3DSwapChain9_GetPresentParameters(sc, &got));
    params("chain", &got);
    printf("GetNumberOfSwapChains %u\n", IDirect3DDevice9_GetNumberOfSwapChains(dev));
    IDirect3DSwapChain9 *other = (IDirect3DSwapChain9 *)1;
    hr = IDirect3DDevice9_GetSwapChain(dev, 1, &other);
    printf("GetSwapChain 1 %#lx %s\n", hr, other == NULL ? "NULL" : other == (void *)1 ? "untouched" : other == sc ? "the chain" : "other");
    if (SUCCEEDED(hr) && other && other != (void *)1) IDirect3DSwapChain9_Release(other);

    IDirect3DSurface9 *bb = NULL, *devbb = NULL, *none = (IDirect3DSurface9 *)1;
    printf("chain GetBackBuffer %#lx\n", IDirect3DSwapChain9_GetBackBuffer(sc, 0, D3DBACKBUFFER_TYPE_MONO, &bb));
    printf("chain GetBackBuffer 1 %#lx\n", IDirect3DSwapChain9_GetBackBuffer(sc, 1, D3DBACKBUFFER_TYPE_MONO, &none));
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &devbb);
    D3DSURFACE_DESC d;
    IDirect3DSurface9_GetDesc(bb, &d);
    printf("chain back buffer %ux%u format %u usage %#lx pool %u, the device's: %s\n", d.Width, d.Height, d.Format, d.Usage, d.Pool, bb == devbb ? "yes" : "no");
    IDirect3DSurface9_GetDesc(devbb, &d);
    printf("device back buffer %ux%u\n", d.Width, d.Height);

    /* refused */
    refuse(dev, win, "full screen", 1, D3DSWAPEFFECT_DISCARD, D3DFMT_X8R8G8B8, FALSE);
    refuse(dev, win, "4 back buffers", 4, D3DSWAPEFFECT_DISCARD, D3DFMT_X8R8G8B8, TRUE);
    refuse(dev, win, "swap effect 0", 1, 0, D3DFMT_X8R8G8B8, TRUE);
    refuse(dev, win, "swap effect 6", 1, 6, D3DFMT_X8R8G8B8, TRUE);
    refuse(dev, win, "A8", 1, D3DSWAPEFFECT_DISCARD, D3DFMT_A8, TRUE);
    refuse(dev, win, "R5G6B5 copy", 1, D3DSWAPEFFECT_COPY, D3DFMT_R5G6B5, TRUE);
    refuse(dev, win, "A8R8G8B8 flip, 0 back buffers", 0, D3DSWAPEFFECT_FLIP, D3DFMT_A8R8G8B8, TRUE);
    refuse(dev, NULL, "no window (the device's)", 1, D3DSWAPEFFECT_DISCARD, D3DFMT_X8R8G8B8, TRUE);
    printf("NULL out %#lx\n", IDirect3DDevice9_CreateAdditionalSwapChain(dev, &sp, NULL));

    /* a frame into the chain's back buffer, presented by the chain */
    printf("SetRenderTarget %#lx\n", IDirect3DDevice9_SetRenderTarget(dev, 0, bb));
    D3DVIEWPORT9 vp;
    IDirect3DDevice9_GetViewport(dev, &vp);
    printf("viewport %lu,%lu %lux%lu\n", vp.X, vp.Y, vp.Width, vp.Height);
    printf("Clear %#lx\n", IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff204060, 1.0f, 0));
    D3DRECT r = { 48, 0, 96, 24 };
    printf("Clear rect %#lx\n", IDirect3DDevice9_Clear(dev, 1, &r, D3DCLEAR_TARGET, 0xffa0c0e0, 1.0f, 0));
    printf("chain Present %#lx\n", IDirect3DSwapChain9_Present(sc, NULL, NULL, NULL, NULL, 0));
    printf("pixels %06lx %06lx %06lx\n", pixel(dev, bb, 5, 5), pixel(dev, bb, 60, 5), pixel(dev, bb, 60, 40));
    printf("device back buffer %06lx\n", pixel(dev, devbb, 0, 0));
    IDirect3DDevice9_SetRenderTarget(dev, 0, devbb);

    /* (no reference counts printed: Windows forwards a chain's back buffer's to the chain) */
    IDirect3DSurface9_Release(devbb);
    IDirect3DSwapChain9_Release(sc);
    IDirect3DSurface9_GetDesc(bb, &d);
    printf("chain back buffer after the chain %ux%u\n", d.Width, d.Height);
    IDirect3DSurface9_Release(bb);
    IDirect3DDevice9_Release(dev);
    IDirect3D9_Release(d3d);
    if (ex) IDirect3D9Ex_Release(ex);
    DestroyWindow(win);
    DestroyWindow(hidden);
    return 0;
}
