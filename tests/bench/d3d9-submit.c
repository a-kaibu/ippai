/* D3D9 submission benchmark: 12,004 draws per round into 8x8 A8R8G8B8/X8R8G8B8
 * targets, with opaque/alpha/color-mask state changes and synchronous pixel readback.
 * Six rounds; discard round zero. The non-timing output must match between builds.
 * Build as a 32-bit Windows console program with -O2 -ld3d9 -lgdi32, then use
 * tests/winref/run-browser.mjs (browser.txt: d3d9-submit-32).
 * This isolates submission work from large-screen rasterization; it is not game FPS. */
#define COBJMACROS
#include "../winref/probe.h"
#include <d3d9.h>

#define W 8
#define H 8

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *bsys;

struct cv { float x, y, z, rhw; DWORD c; };
struct tv { float x, y, z, rhw; float u, v; };

static void rect(float x0, float y0, float x1, float y1, DWORD c) {
    struct cv v[4] = { { x0 - 0.5f, y0 - 0.5f, 0, 1, c }, { x1 - 0.5f, y0 - 0.5f, 0, 1, c }, { x0 - 0.5f, y1 - 0.5f, 0, 1, c }, { x1 - 0.5f, y1 - 0.5f, 0, 1, c } };
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    for (int rep = 0; rep < 2000; ++rep) IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
}

/* the back buffer's pixel (x, y), without alpha */
static DWORD bpx(int x, int y) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, bsys))) return 0xbad0bad0;
    if (FAILED(IDirect3DSurface9_LockRect(bsys, &lr, NULL, D3DLOCK_READONLY))) return 0xbad1bad1;
    DWORD v = ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff;
    IDirect3DSurface9_UnlockRect(bsys);
    return v;
}

static const struct { D3DFORMAT f; const char *n; int bpp; } fm[] = {
    { D3DFMT_A8R8G8B8, "A8R8G8B8", 4 }, { D3DFMT_X8R8G8B8, "X8R8G8B8", 4 },
};

/* where the probe looks: drawn opaque, cleared only, blended, drawn without green */
static const int at[4][2] = { { 1, 1 }, { 5, 1 }, { 5, 6 }, { 7, 2 } };

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9rtfmt", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &bsys, NULL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);

    LARGE_INTEGER freq, start, end; QueryPerformanceFrequency(&freq);
    for (int round = 0; round < 6; ++round) { QueryPerformanceCounter(&start);
    for (int i = 0; i < 2; i++) {
        HRESULT rt = IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_RENDERTARGET, D3DRTYPE_TEXTURE, fm[i].f);
        HRESULT bl = IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_RENDERTARGET | D3DUSAGE_QUERY_POSTPIXELSHADER_BLENDING,
                                                  D3DRTYPE_TEXTURE, fm[i].f);
        IDirect3DTexture9 *t = NULL;
        hr = IDirect3DDevice9_CreateTexture(dev, W, H, 1, D3DUSAGE_RENDERTARGET, fm[i].f, D3DPOOL_DEFAULT, &t, NULL);
        printf("%s rendertarget %d blending %d create %#lx\n", fm[i].n, rt == D3D_OK, bl == D3D_OK, hr);
        if (FAILED(hr)) continue;
        IDirect3DSurface9 *s = NULL;
        IDirect3DTexture9_GetSurfaceLevel(t, 0, &s);
        IDirect3DDevice9_SetRenderTarget(dev, 0, s);
        hr = IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x80406020, 1, 0);
        printf("  clear %#lx\n", hr);
        IDirect3DDevice9_BeginScene(dev);
        IDirect3DDevice9_SetTexture(dev, 0, NULL);
        /* (the diffuse color and alpha as they are: no texture's) */
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
        rect(0, 0, 4, 8, 0xc0ff8000);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        rect(4, 4, 8, 8, 0x8000ffff);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, D3DCOLORWRITEENABLE_RED | D3DCOLORWRITEENABLE_BLUE | D3DCOLORWRITEENABLE_ALPHA);
        rect(6, 0, 8, 4, 0x40c0c0c0);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, 0xf);
        IDirect3DDevice9_EndScene(dev);

        /* the texels */
        IDirect3DSurface9 *sys = NULL;
        hr = IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, fm[i].f, D3DPOOL_SYSTEMMEM, &sys, NULL);
        HRESULT g = sys ? IDirect3DDevice9_GetRenderTargetData(dev, s, sys) : hr;
        printf("  GetRenderTargetData %#lx", g);
        D3DLOCKED_RECT lr;
        if (SUCCEEDED(g) && SUCCEEDED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) {
            for (int k = 0; k < 4; k++) {
                const unsigned char *p = (const unsigned char *)lr.pBits + at[k][1] * lr.Pitch + at[k][0] * fm[i].bpp;
                printf(" ");
                for (int b = 0; b < fm[i].bpp; b++) printf("%02x", p[b]);
            }
            IDirect3DSurface9_UnlockRect(sys);
        }
        printf("\n");
        if (sys) IDirect3DSurface9_Release(sys);

        /* sampled: the color, then the alpha */
        IDirect3DDevice9_SetRenderTarget(dev, 0, back);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
        struct tv v[4] = { { -0.5f, -0.5f, 0, 1, 0, 0 }, { W - 0.5f, -0.5f, 0, 1, 1, 0 }, { -0.5f, H - 0.5f, 0, 1, 0, 1 }, { W - 0.5f, H - 0.5f, 0, 1, 1, 1 } };
        for (int a = 0; a < 2; a++) {
            IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x123456, 1, 0);
            IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
            IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE | (a ? D3DTA_ALPHAREPLICATE : 0));
            IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
            IDirect3DDevice9_BeginScene(dev);
            IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1);
            IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
            IDirect3DDevice9_EndScene(dev);
            printf(a ? "  alpha" : "  color");
            for (int k = 0; k < 4; k++) printf(" %06lx", bpx(at[k][0], at[k][1]));
            printf("\n");
        }
        IDirect3DDevice9_SetTexture(dev, 0, NULL);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DSurface9_Release(s);
        IDirect3DTexture9_Release(t);
    }
    QueryPerformanceCounter(&end); printf("bench round %d: %.3f ms\n", round, 1000.0 * (end.QuadPart - start.QuadPart) / freq.QuadPart);
    }
    return 0;
}
