/* Direct3D 9 calls games make that replays showed differ (tests/replay): QueryInterface for
 * the Ex interfaces on objects of Direct3DCreate9 and of Direct3DCreate9Ex, BeginScene /
 * EndScene out of order, and GetFunction with the sizes a caller passes (NULL data, a size of
 * 0, too small, exact). Each line: the call, its HRESULT and what it wrote.
 * libs: -ld3d9 -ldxguid
 * run: browser (sake's Direct3D 9 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

/* vs_2_0: a comment of 2 tokens, dcl_position v0, mov oPos, v0, end (11 tokens) */
static const DWORD vs[] = { 0xfffe0200, 0x0002fffe, 0x64636261, 0x68676665, 0x0200001f, 0x80000000, 0x900f0000,
                            0x02000001, 0xc00f0000, 0x90e40000, 0x0000ffff };

static IDirect3DDevice9 *device(IDirect3D9 *d3d, HWND hwnd) {
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = 64; pp.BackBufferHeight = 64; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    IDirect3DDevice9 *dev = NULL;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    return SUCCEEDED(hr) ? dev : NULL;
}

static void qi(const char *what, void *o, REFIID iid) {
    void *out = (void *)1;
    HRESULT hr = IUnknown_QueryInterface((IUnknown *)o, iid, &out);
    printf("%s: %#lx %s\n", what, hr, out == NULL ? "NULL" : out == o ? "same" : out == (void *)1 ? "untouched" : "other");
    if (SUCCEEDED(hr) && out) IUnknown_Release((IUnknown *)out);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9calls", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    IDirect3DDevice9 *dev = device(d3d, hwnd);
    if (!dev) { puts("skip no device"); return 0; }
    qi("Direct3DCreate9 QI IDirect3D9", d3d, &IID_IDirect3D9);
    qi("Direct3DCreate9 QI IDirect3D9Ex", d3d, &IID_IDirect3D9Ex);
    qi("its device QI IDirect3DDevice9", dev, &IID_IDirect3DDevice9);
    qi("its device QI IDirect3DDevice9Ex", dev, &IID_IDirect3DDevice9Ex);

    HRESULT (WINAPI *create_ex)(UINT, IDirect3D9Ex **) = (void *)GetProcAddress(GetModuleHandleA("d3d9.dll"), "Direct3DCreate9Ex");
    IDirect3D9Ex *d3dex = NULL;
    if (create_ex && SUCCEEDED(create_ex(D3D_SDK_VERSION, &d3dex)) && d3dex) {
        qi("Direct3DCreate9Ex QI IDirect3D9Ex", d3dex, &IID_IDirect3D9Ex);
        IDirect3DDevice9 *devex = device((IDirect3D9 *)d3dex, hwnd);
        if (devex) {
            qi("its CreateDevice device QI IDirect3DDevice9Ex", devex, &IID_IDirect3DDevice9Ex);
            IDirect3DDevice9_Release(devex);
        } else {
            puts("its CreateDevice device: none");
        }
        IDirect3D9Ex_Release(d3dex);
    } else {
        puts("Direct3DCreate9Ex: none");
    }

    printf("EndScene outside a scene: %#lx\n", IDirect3DDevice9_EndScene(dev));
    printf("BeginScene: %#lx\n", IDirect3DDevice9_BeginScene(dev));
    printf("BeginScene in a scene: %#lx\n", IDirect3DDevice9_BeginScene(dev));
    printf("EndScene: %#lx\n", IDirect3DDevice9_EndScene(dev));
    printf("EndScene again: %#lx\n", IDirect3DDevice9_EndScene(dev));
    printf("BeginScene after: %#lx\n", IDirect3DDevice9_BeginScene(dev));
    printf("EndScene after: %#lx\n", IDirect3DDevice9_EndScene(dev));

    IDirect3DVertexShader9 *shader = NULL;
    HRESULT hr = IDirect3DDevice9_CreateVertexShader(dev, vs, &shader);
    printf("CreateVertexShader: %#lx\n", hr);
    if (SUCCEEDED(hr)) {
        static const UINT sizes[] = { 0, 8, 43, 44, 100 };
        UINT size = 12345;
        hr = IDirect3DVertexShader9_GetFunction(shader, NULL, &size);
        printf("GetFunction NULL data: %#lx size %u\n", hr, size);
        for (int i = 0; i < 5; i++) {
            DWORD buf[32];
            memset(buf, 0xcc, sizeof buf);
            size = sizes[i];
            hr = IDirect3DVertexShader9_GetFunction(shader, buf, &size);
            int copied = 0;
            while (copied < 11 && buf[copied] == vs[copied]) copied++;
            printf("GetFunction size %u: %#lx size %u tokens copied %d after %s\n", sizes[i], hr, size, copied, buf[11] == 0xcccccccc ? "untouched" : "written");
        }
        IDirect3DVertexShader9_Release(shader);
    }
    IDirect3DDevice9_Release(dev);
    IDirect3D9_Release(d3d);
    return 0;
}
