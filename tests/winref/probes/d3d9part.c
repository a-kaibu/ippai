/* Direct3D 9 windowed Present of a part of the back buffer (pSourceRect, pDestRect): the window
 * shows the back buffer's box in the destination box and keeps the rest of what it showed.
 * 果つることなき未来ヨリ (CatSystem2, #235) draws whole frames and presents only the box that
 * changed, Present(&box, &box); a Present that stretches the box over the whole window shows the
 * top-left part of the picture. Here: a 64x64 window, the whole frame red, then a green box of
 * the back buffer into the same box, a blue frame's 4x4 corner stretched into 16x16 elsewhere
 * (all of the back buffer blue: the filter makes no difference), a box past the client area,
 * then a whole gray frame and a box of a dark one after it. Each step's screen is read with
 * GetFrontBufferData (the window at the screen's top left, on top).
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 64
#define H 64

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *front;

static void clear(const RECT *r, D3DCOLOR c) {
    D3DRECT d;
    if (r) { d.x1 = r->left; d.y1 = r->top; d.x2 = r->right; d.y2 = r->bottom; }
    IDirect3DDevice9_Clear(dev, r ? 1 : 0, r ? &d : NULL, D3DCLEAR_TARGET, c, 1, 0);
}

static void present(const char *name, const RECT *src, const RECT *dst) {
    HRESULT hr = IDirect3DDevice9_Present(dev, src, dst, NULL, NULL);
    printf("%s: Present %#lx\n", name, hr);
    /* (the compositor's next frame) */
    Sleep(200);
    D3DLOCKED_RECT lr;
    hr = IDirect3DDevice9_GetFrontBufferData(dev, 0, front);
    if (FAILED(hr) || FAILED(IDirect3DSurface9_LockRect(front, &lr, NULL, D3DLOCK_READONLY))) {
        printf("%s: GetFrontBufferData %#lx\n", name, hr);
        return;
    }
    probe_image(name, lr.pBits, W, H, lr.Pitch, 0);
    IDirect3DSurface9_UnlockRect(front);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowExA(WS_EX_TOPMOST, "probe", "d3d9part", WS_POPUP | WS_VISIBLE, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    UpdateWindow(hwnd);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_COPY; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    /* (GetFrontBufferData's surface: the display mode's size, A8R8G8B8 in system memory) */
    D3DDISPLAYMODE mode;
    IDirect3DDevice9_GetDisplayMode(dev, 0, &mode);
    hr = IDirect3DDevice9_CreateOffscreenPlainSurface(dev, mode.Width, mode.Height, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, &front, NULL);
    if (FAILED(hr)) { printf("skip CreateOffscreenPlainSurface %#lx\n", hr); return 0; }
    /* (another format or pool: refused) */
    IDirect3DSurface9 *bad;
    if (SUCCEEDED(IDirect3DDevice9_CreateOffscreenPlainSurface(dev, mode.Width, mode.Height, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &bad, NULL))) {
        printf("X8R8G8B8: GetFrontBufferData %#lx\n", IDirect3DDevice9_GetFrontBufferData(dev, 0, bad));
        IDirect3DSurface9_Release(bad);
    }
    printf("swap chain 1: GetFrontBufferData %#lx\n", IDirect3DDevice9_GetFrontBufferData(dev, 1, front));

    clear(NULL, 0xffff0000);
    present("whole", NULL, NULL);
    RECT box = { 8, 4, 40, 36 };
    clear(NULL, 0xff00ff00);
    present("box", &box, &box);
    RECT corner = { 0, 0, 4, 4 }, there = { 0, 40, 16, 56 };
    clear(NULL, 0xff0000ff);
    present("stretched", &corner, &there);
    RECT half = { 0, 0, 32, 32 }, past = { 48, 48, 80, 80 };
    clear(NULL, 0xff00ffff);
    present("past", &half, &past);
    clear(NULL, 0xff808080);
    present("whole2", NULL, NULL);
    RECT top = { 16, 0, 32, 8 };
    clear(NULL, 0xff102030);
    present("box2", &top, &top);
    IDirect3DSurface9_Release(front);
    IDirect3DDevice9_Release(dev);
    IDirect3D9_Release(d3d);
    DestroyWindow(hwnd);
    return 0;
}
