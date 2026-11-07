/* Direct3D 8 device state at creation: every render state (GetRenderState's result and value:
 * D3D8 has states D3D9 dropped, LINEPATTERN, ZVISIBLE, EDGEANTIALIAS, ZBIAS,
 * SOFTWAREVERTEXPROCESSING, PATCHSEGMENTS), the texture stage states of stages 0 and 7 (the
 * sampler states are stage states in D3D8), and what SetRenderState of the D3D8-only states
 * reads back; on a software and a hardware vertex processing device.
 * libs: -ld3d8
 * arch: 32 (Direct3D 8 games are 32-bit)
 * run: browser (sake's Direct3D 8 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d8.h>

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d8state", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    IDirect3D8 *d3d = Direct3DCreate8(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 8"); return 0; }
    static const DWORD flags[2] = { D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DCREATE_HARDWARE_VERTEXPROCESSING };
    for (int f = 0; f < 2; f++) {
        D3DPRESENT_PARAMETERS pp = { 0 };
        pp.BackBufferWidth = 64; pp.BackBufferHeight = 64; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
        pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
        pp.EnableAutoDepthStencil = TRUE; pp.AutoDepthStencilFormat = D3DFMT_D24S8;
        IDirect3DDevice8 *dev = NULL;
        HRESULT hr = IDirect3D8_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, flags[f], &pp, &dev);
        printf("device %d: CreateDevice %#lx\n", f, hr);
        if (FAILED(hr)) continue;
        for (int i = 0; i < 256; i++) {
            DWORD v = 0xdeadbeef;
            HRESULT h = IDirect3DDevice8_GetRenderState(dev, i, &v);
            if (h == D3D_OK) printf("%d rs %d = %#lx\n", f, i, v);
            else printf("%d rs %d: Get %#lx\n", f, i, h);
        }
        static const int stages[] = { 0, 7 };
        for (int k = 0; k < 2; k++)
            for (int i = 0; i <= 29; i++) {
                DWORD v = 0xdeadbeef;
                HRESULT h = IDirect3DDevice8_GetTextureStageState(dev, stages[k], i, &v);
                printf("%d tss %d %d: %#lx = %#lx\n", f, stages[k], i, h, v);
            }
        static const int d8only[] = { D3DRS_LINEPATTERN, D3DRS_ZVISIBLE, D3DRS_EDGEANTIALIAS, D3DRS_ZBIAS, D3DRS_SOFTWAREVERTEXPROCESSING, D3DRS_PATCHSEGMENTS };
        for (int k = 0; k < 6; k++) {
            DWORD v = 0xdeadbeef;
            HRESULT s = IDirect3DDevice8_SetRenderState(dev, d8only[k], 1);
            HRESULT g = IDirect3DDevice8_GetRenderState(dev, d8only[k], &v);
            printf("%d set rs %d to 1: %#lx, reads %#lx %#lx\n", f, d8only[k], s, g, v);
        }
        printf("%d Release %lu\n", f, IDirect3DDevice8_Release(dev));
    }
    IDirect3D8_Release(d3d);
    return 0;
}
