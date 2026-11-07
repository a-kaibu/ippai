/* Direct3D 9 device state at creation and what the Set* calls take: every render state,
 * texture stage state (stages 0, 1, 7) and sampler state (0, 15, displacement map, vertex
 * texture 0) a new device reports, transforms, viewport, material, lights, clip planes, stream
 * frequencies; Set*/Get* with indices out of range (their results, and what reads back).
 * libs: -ld3d9
 * run: browser (sake's Direct3D 9 runs in the page: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

static IDirect3DDevice9 *dev;

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9state", WS_POPUP, 0, 0, 64, 64, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = 64; pp.BackBufferHeight = 48; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = TRUE; pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }

    for (int i = 0; i < 256; i++) {
        DWORD v = 0xdeadbeef;
        HRESULT h = IDirect3DDevice9_GetRenderState(dev, i, &v);
        if (h == D3D_OK) printf("rs %d = %#lx\n", i, v);
        else printf("rs %d: Get %#lx\n", i, h);
    }
    static const int stages[] = { 0, 1, 7, 8 };
    for (int k = 0; k < 4; k++)
        for (int i = 0; i <= 33; i++) {
            DWORD v = 0xdeadbeef;
            HRESULT h = IDirect3DDevice9_GetTextureStageState(dev, stages[k], i, &v);
            printf("tss %d %d: %#lx = %#lx\n", stages[k], i, h, v);
        }
    static const int samplers[] = { 0, 15, 16, D3DDMAPSAMPLER, D3DVERTEXTEXTURESAMPLER0, D3DVERTEXTEXTURESAMPLER3, D3DVERTEXTEXTURESAMPLER3 + 1 };
    for (int k = 0; k < 7; k++)
        for (int i = 0; i <= 14; i++) {
            DWORD v = 0xdeadbeef;
            HRESULT h = IDirect3DDevice9_GetSamplerState(dev, samplers[k], i, &v);
            printf("ss %d %d: %#lx = %#lx\n", samplers[k], i, h, v);
        }
    {
        static const int ts[] = { D3DTS_VIEW, D3DTS_PROJECTION, D3DTS_TEXTURE0, D3DTS_TEXTURE7, D3DTS_WORLD, D3DTS_WORLDMATRIX(255), 0, 1, 24, 512 };
        for (int k = 0; k < 10; k++) {
            D3DMATRIX m; memset(&m, 0x55, sizeof m);
            HRESULT h = IDirect3DDevice9_GetTransform(dev, ts[k], &m);
            printf("transform %d: %#lx %g %g %g %g\n", ts[k], h, m._11, m._12, m._22, m._44);
        }
        D3DVIEWPORT9 vp; memset(&vp, 0x55, sizeof vp);
        IDirect3DDevice9_GetViewport(dev, &vp);
        printf("viewport %lu %lu %lu %lu %g %g\n", vp.X, vp.Y, vp.Width, vp.Height, vp.MinZ, vp.MaxZ);
        RECT r; memset(&r, 0x55, sizeof r);
        printf("scissor %#lx %ld %ld %ld %ld\n", IDirect3DDevice9_GetScissorRect(dev, &r), r.left, r.top, r.right, r.bottom);
        D3DMATERIAL9 mt; memset(&mt, 0x55, sizeof mt);
        printf("material %#lx %g %g %g\n", IDirect3DDevice9_GetMaterial(dev, &mt), mt.Diffuse.r, mt.Ambient.a, mt.Power);
        D3DLIGHT9 l; BOOL on = 7;
        printf("light 0 %#lx enable %#lx %d\n", IDirect3DDevice9_GetLight(dev, 0, &l), IDirect3DDevice9_GetLightEnable(dev, 0, &on), on);
        float p[4] = { 9, 9, 9, 9 };
        printf("clipplane 0 %#lx %g %g; 6 %#lx\n", IDirect3DDevice9_GetClipPlane(dev, 0, p), p[0], p[3], IDirect3DDevice9_GetClipPlane(dev, 6, p));
        UINT f = 7;
        printf("freq 0 %#lx %u; 16 %#lx\n", IDirect3DDevice9_GetStreamSourceFreq(dev, 0, &f), f, IDirect3DDevice9_GetStreamSourceFreq(dev, 16, &f));
        DWORD fvf = 7;
        IDirect3DDevice9_GetFVF(dev, &fvf);
        printf("fvf %#lx npatch %g\n", fvf, IDirect3DDevice9_GetNPatchMode(dev));
        float c[4] = { 9, 9, 9, 9 }; int n[4] = { 9 }; BOOL b = 9;
        printf("vsf 0 %#lx %g; 256 %#lx\n", IDirect3DDevice9_GetVertexShaderConstantF(dev, 0, c, 1), c[0], IDirect3DDevice9_GetVertexShaderConstantF(dev, 256, c, 1));
        printf("psf 0 %#lx %g; 224 %#lx\n", IDirect3DDevice9_GetPixelShaderConstantF(dev, 0, c, 1), c[0], IDirect3DDevice9_GetPixelShaderConstantF(dev, 224, c, 1));
        printf("vsi 0 %#lx %d; 16 %#lx; vsb 0 %#lx %d; 16 %#lx\n", IDirect3DDevice9_GetVertexShaderConstantI(dev, 0, n, 1), n[0], IDirect3DDevice9_GetVertexShaderConstantI(dev, 16, n, 1),
               IDirect3DDevice9_GetVertexShaderConstantB(dev, 0, &b, 1), b, IDirect3DDevice9_GetVertexShaderConstantB(dev, 16, &b, 1));
    }
    /* out of range sets */
    printf("SetRenderState 0 %#lx 300 %#lx\n", IDirect3DDevice9_SetRenderState(dev, 0, 1), IDirect3DDevice9_SetRenderState(dev, 300, 1));
    printf("SetTextureStageState 8 %#lx type 40 %#lx\n", IDirect3DDevice9_SetTextureStageState(dev, 8, D3DTSS_COLOROP, 1), IDirect3DDevice9_SetTextureStageState(dev, 0, 40, 1));
    printf("SetSamplerState 16 %#lx type 20 %#lx\n", IDirect3DDevice9_SetSamplerState(dev, 16, D3DSAMP_ADDRESSU, 1), IDirect3DDevice9_SetSamplerState(dev, 0, 20, 1));
    printf("SetTexture 16 %#lx 261 %#lx\n", IDirect3DDevice9_SetTexture(dev, 16, NULL), IDirect3DDevice9_SetTexture(dev, 261, NULL));
    printf("SetStreamSource 16 %#lx\n", IDirect3DDevice9_SetStreamSource(dev, 16, NULL, 0, 0));
    printf("SetStreamSourceFreq 0 0 %#lx\n", IDirect3DDevice9_SetStreamSourceFreq(dev, 0, 0));
    {
        float c[4] = { 1, 2, 3, 4 };
        printf("SetVertexShaderConstantF 255+2 %#lx\n", IDirect3DDevice9_SetVertexShaderConstantF(dev, 255, c, 2));
        printf("SetClipPlane 6 %#lx\n", IDirect3DDevice9_SetClipPlane(dev, 6, c));
        printf("LightEnable 1000 %#lx\n", IDirect3DDevice9_LightEnable(dev, 1000, TRUE));
        D3DLIGHT9 l; memset(&l, 0x55, sizeof l);
        printf("GetLight 1000 after LightEnable %#lx type %d range %g\n", IDirect3DDevice9_GetLight(dev, 1000, &l), l.Type, l.Range);
        printf("SetLight NULL %#lx SetMaterial NULL %#lx SetViewport NULL %#lx\n", IDirect3DDevice9_SetLight(dev, 1, NULL), IDirect3DDevice9_SetMaterial(dev, NULL), IDirect3DDevice9_SetViewport(dev, NULL));
        D3DVIEWPORT9 vp = { 60, 40, 32, 32, 0, 1 };
        printf("SetViewport past the target %#lx\n", IDirect3DDevice9_SetViewport(dev, &vp));
        D3DVIEWPORT9 g; IDirect3DDevice9_GetViewport(dev, &g);
        printf("viewport now %lu %lu %lu %lu\n", g.X, g.Y, g.Width, g.Height);
        printf("SetFVF 0 %#lx\n", IDirect3DDevice9_SetFVF(dev, 0));
    }
    printf("Release device %lu\n", IDirect3DDevice9_Release(dev));
    IDirect3D9_Release(d3d);
    return 0;
}
