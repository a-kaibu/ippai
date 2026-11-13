/* Direct3D 9 volume textures (#107): CheckDeviceFormat for D3DRTYPE_VOLUMETEXTURE, the caps,
 * CreateVolumeTexture's levels and their descriptions (texture and volume), LockBox's pitches
 * and boxes (pBits at the box's corner, boxes outside the volume), GetContainer, then sampling
 * from a ps_2_0 shader with dcl_volume (point and linear filtering between slices, every W
 * address mode, a box written over the first texels), a system memory texture through
 * UpdateTexture, and a 2D texture set where the shader samples a volume.
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <d3d9.h>

#define W 8
#define H 8
#define S 4 /* the volume: S x S x S */

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *bsys;

/* ps_2_0: dcl t0.xyz; dcl_volume s0; texld r0, t0, s0; mov oC0, r0 */
static const DWORD ps_code[] = {
    0xffff0200, 0x0200001f, 0x80000000, 0xb0070000, 0x0200001f, 0xa0000000, 0xa00f0800, 0x03000042,
    0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff,
};

struct tv { float x, y, z, rhw; float u, v, w; };

/* texel (x, y, z): z in red, y in green, x in blue (steps of 0x40: linear filtering between two
 * slices is exact) */
static DWORD texel(int x, int y, int z) { return 0xff000000 | (z * 0x40 + 0x20) << 16 | (y * 0x40 + 0x10) << 8 | (x * 0x40 + 0x08); }

/* the back buffer's pixel (x, y), without alpha */
static DWORD bpx(int x, int y) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, bsys))) return 0xbad0bad0;
    if (FAILED(IDirect3DSurface9_LockRect(bsys, &lr, NULL, D3DLOCK_READONLY))) return 0xbad1bad1;
    DWORD v = ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff;
    IDirect3DSurface9_UnlockRect(bsys);
    return v;
}

/* a quad over the back buffer sampling the volume at (u, v, w) everywhere: what it reads */
static DWORD at(float u, float v, float w) {
    struct tv q[4] = { { -0.5f, -0.5f, 0, 1, u, v, w }, { W - 0.5f, -0.5f, 0, 1, u, v, w }, { -0.5f, H - 0.5f, 0, 1, u, v, w }, { W - 0.5f, H - 0.5f, 0, 1, u, v, w } };
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x123456, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_TEX1 | D3DFVF_TEXCOORDSIZE3(0));
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
    IDirect3DDevice9_EndScene(dev);
    return bpx(3, 3);
}

static void fill(IDirect3DVolumeTexture9 *t, UINT level, int n) {
    D3DLOCKED_BOX lb;
    HRESULT hr = IDirect3DVolumeTexture9_LockBox(t, level, &lb, NULL, 0);
    printf("LockBox level %u %#lx RowPitch %d SlicePitch %d\n", level, hr, lb.RowPitch, lb.SlicePitch);
    if (FAILED(hr)) return;
    for (int z = 0; z < n; z++)
        for (int y = 0; y < n; y++)
            for (int x = 0; x < n; x++) ((DWORD *)((char *)lb.pBits + z * lb.SlicePitch + y * lb.RowPitch))[x] = texel(x, y, z);
    IDirect3DVolumeTexture9_UnlockBox(t, level);
}

static void samples(const char *what) {
    printf("%s:", what);
    for (int z = 0; z < S; z++) printf(" %06lx", at((z + 0.5f) / S, (1.5f) / S, (z + 0.5f) / S));
    printf("\n");
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9vol", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    printf("CheckDeviceFormat VOLUMETEXTURE A8R8G8B8 %#lx\n", IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, 0, D3DRTYPE_VOLUMETEXTURE, D3DFMT_A8R8G8B8));
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    D3DCAPS9 caps;
    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("VOLUMEMAP %d\n", (caps.TextureCaps & D3DPTEXTURECAPS_VOLUMEMAP) != 0);
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &bsys, NULL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);

    /* invalid ones */
    IDirect3DVolumeTexture9 *t = (IDirect3DVolumeTexture9 *)1;
    hr = IDirect3DDevice9_CreateVolumeTexture(dev, S, S, 0, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
    printf("CreateVolumeTexture depth 0 %#lx %d\n", hr, t == NULL);
    hr = IDirect3DDevice9_CreateVolumeTexture(dev, S, S, S, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &t, NULL);
    printf("CreateVolumeTexture RENDERTARGET %#lx\n", hr);

    /* all levels: 4x4x4, 2x2x2, 1x1x1 */
    t = NULL;
    hr = IDirect3DDevice9_CreateVolumeTexture(dev, S, S, S, 0, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
    printf("CreateVolumeTexture %#lx\n", hr);
    if (FAILED(hr)) return 0;
    printf("levels %lu type %d\n", IDirect3DVolumeTexture9_GetLevelCount(t), IDirect3DVolumeTexture9_GetType(t));
    for (UINT l = 0; l < 4; l++) {
        D3DVOLUME_DESC d;
        memset(&d, 0, sizeof d);
        hr = IDirect3DVolumeTexture9_GetLevelDesc(t, l, &d);
        printf("level %u %#lx format %d type %d usage %#lx pool %d %ux%ux%u\n", l, hr, d.Format, d.Type, d.Usage, d.Pool, d.Width, d.Height, d.Depth);
    }
    IDirect3DVolume9 *v = NULL;
    hr = IDirect3DVolumeTexture9_GetVolumeLevel(t, 1, &v);
    printf("GetVolumeLevel %#lx\n", hr);
    if (v) {
        D3DVOLUME_DESC d;
        IDirect3DVolume9_GetDesc(v, &d);
        printf("volume desc format %d type %d %ux%ux%u\n", d.Format, d.Type, d.Width, d.Height, d.Depth);
        void *c = NULL;
        hr = IDirect3DVolume9_GetContainer(v, &IID_IDirect3DVolumeTexture9, &c);
        printf("GetContainer %#lx same %d\n", hr, c == (void *)t);
        if (c) IUnknown_Release((IUnknown *)c);
        D3DLOCKED_BOX lb;
        D3DBOX bad[] = { { 0, 0, 3, 2, 0, 2 }, { 0, 0, 2, 2, 0, 3 }, { 1, 0, 1, 2, 0, 2 } };
        for (int k = 0; k < 3; k++) {
            hr = IDirect3DVolume9_LockBox(v, &lb, &bad[k], 0);
            printf("LockBox box %d %#lx\n", k, hr);
            if (SUCCEEDED(hr)) IDirect3DVolume9_UnlockBox(v);
        }
        IDirect3DVolume9_Release(v);
    }
    fill(t, 0, S);
    fill(t, 1, S / 2);
    {
        /* a box: Left 1, Top 1, Right 3, Bottom 2, Front 2, Back 3; pBits at its corner */
        D3DLOCKED_BOX all, lb;
        D3DBOX b = { 1, 1, 3, 2, 2, 3 };
        IDirect3DVolumeTexture9_LockBox(t, 0, &all, NULL, D3DLOCK_READONLY);
        char *base = all.pBits;
        IDirect3DVolumeTexture9_UnlockBox(t, 0);
        hr = IDirect3DVolumeTexture9_LockBox(t, 0, &lb, &b, 0);
        printf("LockBox box %#lx offset %d\n", hr, (int)((char *)lb.pBits - base));
        if (SUCCEEDED(hr)) {
            ((DWORD *)lb.pBits)[0] = 0xffffffff;
            ((DWORD *)lb.pBits)[1] = 0xff000000;
            IDirect3DVolumeTexture9_UnlockBox(t, 0);
        }
    }

    IDirect3DPixelShader9 *ps = NULL;
    hr = IDirect3DDevice9_CreatePixelShader(dev, ps_code, &ps);
    printf("CreatePixelShader %#lx\n", hr);
    if (FAILED(hr)) return 0;
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
    samples("point");
    printf("box texels: %06lx %06lx\n", at(1.5f / S, 1.5f / S, 2.5f / S), at(2.5f / S, 1.5f / S, 2.5f / S));
    /* W outside 0..1: every address mode */
    static const char *mode[] = { "", "WRAP", "MIRROR", "CLAMP", "BORDER", "MIRRORONCE" };
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_BORDERCOLOR, 0xff808080);
    for (int m = 1; m <= 5; m++) {
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSW, m);
        printf("ADDRESSW %s: %06lx %06lx\n", mode[m], at(0.5f / S, 0.5f / S, 1.0f + 0.5f / S), at(0.5f / S, 0.5f / S, -0.5f / S));
    }
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSW, D3DTADDRESS_WRAP);
    /* linear: halfway between slices 0 and 1, 1 and 2 */
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSW, D3DTADDRESS_CLAMP);
    printf("linear: %06lx %06lx\n", at(0.5f / S, 0.5f / S, 1.0f / S), at(0.5f / S, 0.5f / S, 2.0f / S));
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);

    /* system memory through UpdateTexture into a default pool one */
    IDirect3DVolumeTexture9 *sys = NULL, *def = NULL;
    hr = IDirect3DDevice9_CreateVolumeTexture(dev, S, S, S, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    printf("CreateVolumeTexture SYSTEMMEM %#lx\n", hr);
    hr = IDirect3DDevice9_CreateVolumeTexture(dev, S, S, S, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &def, NULL);
    printf("CreateVolumeTexture DEFAULT %#lx\n", hr);
    if (sys && def) {
        fill(sys, 0, S);
        printf("UpdateTexture %#lx\n", IDirect3DDevice9_UpdateTexture(dev, (IDirect3DBaseTexture9 *)sys, (IDirect3DBaseTexture9 *)def));
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)def);
        samples("updated");
        printf("UpdateTexture back %#lx\n", IDirect3DDevice9_UpdateTexture(dev, (IDirect3DBaseTexture9 *)def, (IDirect3DBaseTexture9 *)sys));
    }

    /* a 2D texture where the shader samples a volume (undefined: only that it draws) */
    IDirect3DTexture9 *t2 = NULL;
    IDirect3DDevice9_CreateTexture(dev, S, S, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t2, NULL);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t2);
    at(0.5f, 0.5f, 0.5f);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    samples("again");
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    if (t2) IDirect3DTexture9_Release(t2);
    if (sys) IDirect3DVolumeTexture9_Release(sys);
    if (def) IDirect3DVolumeTexture9_Release(def);
    IDirect3DPixelShader9_Release(ps);
    printf("Release %lu\n", IDirect3DVolumeTexture9_Release(t));
    return 0;
}
