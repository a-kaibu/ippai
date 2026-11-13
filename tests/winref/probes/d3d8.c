/* Direct3D 8: the x87 precision CreateDevice leaves (single unless D3DCREATE_FPU_PRESERVE),
 * caps and formats an old game asks for, and frames after clears, pre-transformed textured quads
 * (point/linear through the stage states D3D8 keeps samplers in), texture formats, blending and
 * alpha test, a vs.1.1/ps.1.1 pair, CopyRects, UpdateTexture and a lockable back buffer written
 * directly. Each picture is the 64x64 back buffer copied to an image surface with CopyRects. On
 * the Windows runner the device is WARP (Microsoft Basic Render Driver), under d3d8's own layer.
 * libs: -ld3d8 -lgdi32
 * arch: 32 (Direct3D 8 games are 32-bit)
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d8.h>
#include <float.h>

#define W 64
#define H 64

static IDirect3DDevice8 *dev;
static IDirect3DSurface8 *back, *sys;

static void shot(const char *name, int tol) {
    D3DLOCKED_RECT lr;
    HRESULT hr = IDirect3DDevice8_CopyRects(dev, back, NULL, 0, sys, NULL);
    if (FAILED(hr)) { printf("%s: CopyRects %#lx\n", name, hr); return; }
    if (FAILED(IDirect3DSurface8_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) { printf("%s: LockRect failed\n", name); return; }
    probe_image(name, lr.pBits, W, H, lr.Pitch, tol);
    IDirect3DSurface8_UnlockRect(sys);
}

struct tlv { float x, y, z, rhw; DWORD c; float u, v; };
#define FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1)

static void quad(float x0, float y0, float x1, float y1, DWORD c, float u0, float v0, float u1, float v1) {
    struct tlv q[4] = {
        { x0, y0, 0.5f, 1, c, u0, v0 }, { x1, y0, 0.5f, 1, c, u1, v0 },
        { x0, y1, 0.5f, 1, c, u0, v1 }, { x1, y1, 0.5f, 1, c, u1, v1 },
    };
    IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
}

static void begin(DWORD clear) {
    IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, clear, 1.0f, 0);
    IDirect3DDevice8_BeginScene(dev);
}
static void end(void) { IDirect3DDevice8_EndScene(dev); }

static void reset(void) {
    IDirect3DDevice8_SetVertexShader(dev, FVF);
    IDirect3DDevice8_SetPixelShader(dev, 0);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHATESTENABLE, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_BLENDOP, D3DBLENDOP_ADD);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_ONE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_ZERO);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_COLORWRITEENABLE, 0xf);
    for (int s = 0; s < 2; s++) {
        IDirect3DDevice8_SetTexture(dev, s, NULL);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_COLOROP, s ? D3DTOP_DISABLE : D3DTOP_MODULATE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_COLORARG2, s ? D3DTA_CURRENT : D3DTA_DIFFUSE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_ALPHAOP, s ? D3DTOP_DISABLE : D3DTOP_SELECTARG1);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_TEXCOORDINDEX, 0);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_MINFILTER, D3DTEXF_POINT);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_MAGFILTER, D3DTEXF_POINT);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_MIPFILTER, D3DTEXF_NONE);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_ADDRESSU, D3DTADDRESS_WRAP);
        IDirect3DDevice8_SetTextureStageState(dev, s, D3DTSS_ADDRESSV, D3DTADDRESS_WRAP);
    }
    D3DVIEWPORT8 vp = { 0, 0, W, H, 0, 1 };
    IDirect3DDevice8_SetViewport(dev, &vp);
}

/* a texture of any format from raw rows (row = bytes per row of blocks for DXT) */
static IDirect3DTexture8 *texfmt(D3DFORMAT f, int w, int h, const void *data, int row, int rows, D3DPOOL pool) {
    IDirect3DTexture8 *t = NULL;
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice8_CreateTexture(dev, w, h, 1, 0, f, pool, &t))) return NULL;
    IDirect3DTexture8_LockRect(t, 0, &lr, NULL, 0);
    for (int y = 0; y < rows; y++) memcpy((char *)lr.pBits + y * lr.Pitch, (const char *)data + y * row, row);
    IDirect3DTexture8_UnlockRect(t, 0);
    return t;
}

static const char *pc(unsigned cw) {
    switch (cw & _MCW_PC) {
    case _PC_24: return "24";
    case _PC_53: return "53";
    case _PC_64: return "64";
    }
    return "?";
}

static HRESULT create(IDirect3D8 *d3d, HWND hwnd, DWORD flags, DWORD ppflags, IDirect3DDevice8 **out) {
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = TRUE; pp.AutoDepthStencilFormat = D3DFMT_D24S8; pp.Flags = ppflags;
    return IDirect3D8_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, flags, &pp, out);
}

static DWORD checker[16];

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d8", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D8 *d3d = Direct3DCreate8(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 8"); return 0; }

    /* the x87 precision: FPU_PRESERVE keeps it, without it CreateDevice sets single precision
     * (and it stays so after the device is gone) */
    {
        IDirect3DDevice8 *d = NULL;
        printf("fpu before %s\n", pc(_controlfp(0, 0)));
        HRESULT hr = create(d3d, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE, 0, &d);
        printf("fpu preserve %#lx %s\n", hr, pc(_controlfp(0, 0)));
        if (d) IDirect3DDevice8_Release(d);
        d = NULL;
        hr = create(d3d, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, 0, &d);
        volatile double one = 1.0, three = 3.0, third = one / three;
        printf("fpu default %#lx %s\n", hr, pc(_controlfp(0, 0)));
        printf("fpu default: 1/3 %.17g\n", third);
        if (d) printf("Release %lu\n", IDirect3DDevice8_Release(d));
        printf("fpu after release %s\n", pc(_controlfp(0, 0)));
        _controlfp(_PC_53, _MCW_PC);
    }

    HRESULT hr = create(d3d, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE, D3DPRESENTFLAG_LOCKABLE_BACKBUFFER, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    IDirect3DDevice8_GetBackBuffer(dev, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    hr = IDirect3DDevice8_CreateImageSurface(dev, W, H, D3DFMT_X8R8G8B8, &sys);
    if (FAILED(hr)) { printf("skip CreateImageSurface %#lx\n", hr); return 0; }

    /* what games ask before they draw */
    {
        D3DCAPS8 c;
        IDirect3DDevice8_GetDeviceCaps(dev, &c);
        printf("caps vs %lx ps %lx stages %lu textures %lu\n", c.VertexShaderVersion, c.PixelShaderVersion, c.MaxTextureBlendStages, c.MaxSimultaneousTextures);
        printf("caps texture pow2 %d nonpow2cond %d square %d alpha %d\n", !!(c.TextureCaps & D3DPTEXTURECAPS_POW2),
               !!(c.TextureCaps & D3DPTEXTURECAPS_NONPOW2CONDITIONAL), !!(c.TextureCaps & D3DPTEXTURECAPS_SQUAREONLY),
               !!(c.TextureCaps & D3DPTEXTURECAPS_ALPHA));
        /* Caps2 bits of IDirect3D8::GetDeviceCaps (D3DApp of the DirectX 8 SDK keeps a device
           only when it renders windowed: 凌●学園 体験版) */
        static const struct { D3DDEVTYPE t; const char *n; } dt[] = { { D3DDEVTYPE_HAL, "HAL" }, { D3DDEVTYPE_REF, "REF" } };
        for (int i = 0; i < 2; i++) {
            D3DCAPS8 a;
            hr = IDirect3D8_GetDeviceCaps(d3d, 0, dt[i].t, &a);
            if (FAILED(hr)) printf("caps2 %s %#lx\n", dt[i].n, hr);
            else printf("caps2 %s windowed %d gamma %d dynamic %d\n", dt[i].n, !!(a.Caps2 & D3DCAPS2_CANRENDERWINDOWED),
                        !!(a.Caps2 & D3DCAPS2_FULLSCREENGAMMA), !!(a.Caps2 & D3DCAPS2_DYNAMICTEXTURES));
        }
        D3DDISPLAYMODE m;
        hr = IDirect3D8_GetAdapterDisplayMode(d3d, 0, &m);
        printf("GetAdapterDisplayMode %#lx format %d\n", hr, hr ? 0 : m.Format);
        static const struct { D3DFORMAT f; const char *n; } fm[] = {
            { D3DFMT_A8R8G8B8, "A8R8G8B8" }, { D3DFMT_X8R8G8B8, "X8R8G8B8" }, { D3DFMT_R5G6B5, "R5G6B5" },
            { D3DFMT_A1R5G5B5, "A1R5G5B5" }, { D3DFMT_A4R4G4B4, "A4R4G4B4" }, { D3DFMT_X1R5G5B5, "X1R5G5B5" },
            { D3DFMT_DXT1, "DXT1" }, { D3DFMT_DXT3, "DXT3" }, { D3DFMT_DXT5, "DXT5" }, { D3DFMT_D16, "D16" }, { D3DFMT_D24S8, "D24S8" },
        };
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            int depth = fm[i].f == D3DFMT_D16 || fm[i].f == D3DFMT_D24S8;
            HRESULT t = IDirect3D8_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, 0, D3DRTYPE_TEXTURE, fm[i].f);
            HRESULT d = IDirect3D8_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_DEPTHSTENCIL, D3DRTYPE_SURFACE, fm[i].f);
            printf("format %s texture %d depth %d\n", fm[i].n, t == D3D_OK, depth ? d == D3D_OK : -1);
        }
    }

    reset();
    /* clears: whole target, two rects, a smaller viewport */
    {
        D3DRECT r[2] = { { 4, 4, 20, 30 }, { 15, 25, 40, 60 } };
        IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x203040, 1, 0);
        IDirect3DDevice8_Clear(dev, 2, r, D3DCLEAR_TARGET, 0xff8000, 1, 0);
        D3DVIEWPORT8 vp = { 44, 8, 16, 50, 0, 1 };
        IDirect3DDevice8_SetViewport(dev, &vp);
        IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x00ff80, 1, 0);
        reset();
        shot("clear", 0);
    }
    /* sampling through the stage states: point 1:1 and enlarged, linear, clamp, mirror */
    for (int i = 0; i < 16; i++) checker[i] = 0xff000000 | (i & 3) * 0x55 << 16 | (i >> 2) * 0x55 << 8 | ((i ^ (i >> 2)) & 1) * 0xff;
    {
        IDirect3DTexture8 *t = texfmt(D3DFMT_A8R8G8B8, 4, 4, checker, 16, 4, D3DPOOL_MANAGED);
        begin(0x101010);
        IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)t);
        quad(-0.5f + 2, -0.5f + 2, 4 - 0.5f + 2, 4 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        quad(-0.5f + 8, -0.5f + 2, 24 - 0.5f + 8, 24 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTEXF_LINEAR);
        quad(-0.5f + 36, -0.5f + 2, 24 - 0.5f + 36, 24 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ADDRESSU, D3DTADDRESS_CLAMP);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ADDRESSV, D3DTADDRESS_CLAMP);
        quad(-0.5f + 8, -0.5f + 32, 24 - 0.5f + 8, 24 - 0.5f + 32, 0xff80ff40, 0, 0, 1, 1);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTEXF_POINT);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTEXF_POINT);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ADDRESSU, D3DTADDRESS_MIRROR);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ADDRESSV, D3DTADDRESS_MIRROR);
        quad(36 - 0.5f, 32 - 0.5f, 60 - 0.5f, 56 - 0.5f, 0xffffffff, -1, -1, 2, 2);
        end();
        reset();
        shot("sampling", 10); /* (WARP weighs texels in 16ths) */
        IDirect3DTexture8_Release(t);
    }
    /* texture formats, each 8x8 drawn 1:1 in a 9-pixel cell, alpha blended over gray */
    {
        begin(0x808080);
        unsigned short w16[64];
        DWORD w32[64];
        for (int i = 0; i < 64; i++) { w16[i] = (unsigned short)(i * 1031 + 0x1234); w32[i] = 0x80000000u | (unsigned)i * 0x030507; }
        static const unsigned char dxt1[32] = { 0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
                                                0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char dxt3[64] = {
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0xf0, 0xf0, 0xf0, 0xf0, 0x0f, 0x0f, 0x0f, 0x0f,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char dxt5[64] = {
            0xff, 0x00, 0x88, 0xc6, 0x3e, 0x88, 0xc6, 0x3e,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0x40, 0xc0, 0x24, 0x49, 0x92, 0x24, 0x49, 0x92,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0x10, 0x20, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x80, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        struct { D3DFORMAT f; const char *n; const void *d; int row, rows; } fm[] = {
            { D3DFMT_A8R8G8B8, "A8R8G8B8", w32, 32, 8 }, { D3DFMT_X8R8G8B8, "X8R8G8B8", w32, 32, 8 },
            { D3DFMT_R5G6B5, "R5G6B5", w16, 16, 8 }, { D3DFMT_X1R5G5B5, "X1R5G5B5", w16, 16, 8 },
            { D3DFMT_A1R5G5B5, "A1R5G5B5", w16, 16, 8 }, { D3DFMT_A4R4G4B4, "A4R4G4B4", w16, 16, 8 },
            { D3DFMT_DXT1, "DXT1", dxt1, 16, 2 }, { D3DFMT_DXT3, "DXT3", dxt3, 32, 2 }, { D3DFMT_DXT5, "DXT5", dxt5, 32, 2 },
        };
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            IDirect3DTexture8 *t = texfmt(fm[i].f, 8, 8, fm[i].d, fm[i].row, fm[i].rows, D3DPOOL_MANAGED);
            printf("texture %s %s\n", fm[i].n, t ? "ok" : "failed");
            if (!t) continue;
            IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)t);
            float x = (i % 7) * 9 + 1, y = (i / 7) * 9 + 1;
            quad(x - 0.5f, y - 0.5f, x + 8 - 0.5f, y + 8 - 0.5f, 0xffffffff, 0, 0, 1, 1);
            IDirect3DTexture8_Release(t);
        }
        /* UpdateTexture: a system-memory texture into a default-pool one */
        IDirect3DTexture8 *s = texfmt(D3DFMT_A8R8G8B8, 4, 4, checker, 16, 4, D3DPOOL_SYSTEMMEM), *d = NULL;
        hr = IDirect3DDevice8_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &d);
        if (s && d) {
            printf("UpdateTexture %#lx\n", IDirect3DDevice8_UpdateTexture(dev, (IDirect3DBaseTexture8 *)s, (IDirect3DBaseTexture8 *)d));
            IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)d);
            quad(2 - 0.5f, 40 - 0.5f, 18 - 0.5f, 56 - 0.5f, 0xffffffff, 0, 0, 1, 1);
        }
        if (s) IDirect3DTexture8_Release(s);
        if (d) IDirect3DTexture8_Release(d);
        end();
        reset();
        shot("formats", 1);
    }
    /* blending over a gradient, and alpha test on an alpha ramp */
    {
        begin(0);
        struct tlv g[4] = { { 0, 0, 0.5f, 1, 0xff0000ff }, { 64, 0, 0.5f, 1, 0xffff0000 }, { 0, 64, 0.5f, 1, 0xff00ff00 }, { 64, 64, 0.5f, 1, 0xffffffff } };
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
        IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG2);
        IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, g, sizeof g[0]);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        static const DWORD bl[8][3] = {
            { D3DBLENDOP_ADD, D3DBLEND_SRCALPHA, D3DBLEND_INVSRCALPHA }, { D3DBLENDOP_ADD, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_ADD, D3DBLEND_DESTCOLOR, D3DBLEND_ZERO }, { D3DBLENDOP_REVSUBTRACT, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_SUBTRACT, D3DBLEND_SRCALPHA, D3DBLEND_ONE }, { D3DBLENDOP_MIN, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_MAX, D3DBLEND_ONE, D3DBLEND_ONE }, { D3DBLENDOP_ADD, D3DBLEND_INVDESTCOLOR, D3DBLEND_SRCCOLOR },
        };
        for (int i = 0; i < 8; i++) {
            IDirect3DDevice8_SetRenderState(dev, D3DRS_BLENDOP, bl[i][0]);
            IDirect3DDevice8_SetRenderState(dev, D3DRS_SRCBLEND, bl[i][1]);
            IDirect3DDevice8_SetRenderState(dev, D3DRS_DESTBLEND, bl[i][2]);
            float x = (i & 3) * 16, y = (i >> 2) * 16;
            quad(x + 2, y + 2, x + 14, y + 14, 0x80c06020, 0, 0, 0, 0);
        }
        IDirect3DDevice8_SetRenderState(dev, D3DRS_BLENDOP, D3DBLENDOP_ADD);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        struct tlv a[4] = { { 2, 34, 0.5f, 1, 0x00ffffff }, { 62, 34, 0.5f, 1, 0xffffffff }, { 2, 46, 0.5f, 1, 0x00ffffff }, { 62, 46, 0.5f, 1, 0xffffffff } };
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHATESTENABLE, TRUE);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHAREF, 0x80);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHAFUNC, D3DCMP_GREATER);
        IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, a, sizeof a[0]);
        IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHAFUNC, D3DCMP_LESS);
        for (int i = 0; i < 4; i++) a[i].y += 14;
        IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, a, sizeof a[0]);
        end();
        reset();
        shot("blend", 2);
    }
    /* shaders: vs.1.1 (mov oPos, v0; mov oD0, v5; mov oT0, v7) with a declaration, then
     * ps.1.1 (tex t0; mul r0, t0, v0) */
    {
        static const DWORD decl[] = { D3DVSD_STREAM(0), D3DVSD_REG(0, D3DVSDT_FLOAT4), D3DVSD_REG(5, D3DVSDT_D3DCOLOR),
                                      D3DVSD_REG(7, D3DVSDT_FLOAT2), D3DVSD_END() };
        static const DWORD vs[] = { 0xfffe0101, 0x00000001, 0xc00f0000, 0x90e40000, 0x00000001, 0xd00f0000, 0x90e40005,
                                    0x00000001, 0xe00f0000, 0x90e40007, 0x0000ffff };
        static const DWORD ps[] = { 0xffff0101, 0x00000042, 0xb00f0000, 0x00000005, 0x800f0000, 0xb0e40000, 0x90e40000, 0x0000ffff };
        DWORD hv = 0, hp = 0;
        HRESULT a = IDirect3DDevice8_CreateVertexShader(dev, decl, vs, &hv, 0);
        HRESULT b = IDirect3DDevice8_CreatePixelShader(dev, ps, &hp);
        printf("CreateVertexShader %#lx CreatePixelShader %#lx\n", a, b);
        struct { float x, y, z, w; DWORD c; float u, v; } v[4] = {
            { -0.9f, 0.9f, 0.5f, 1, 0xffff0000, 0, 0 }, { 0.9f, 0.9f, 0.5f, 1, 0xff00ff00, 1, 0 },
            { -0.9f, -0.9f, 0.5f, 1, 0xff0000ff, 0, 1 }, { 0.9f, -0.9f, 0.5f, 1, 0xffffffff, 1, 1 },
        };
        IDirect3DTexture8 *t = texfmt(D3DFMT_A8R8G8B8, 4, 4, checker, 16, 4, D3DPOOL_MANAGED);
        begin(0x202020);
        if (SUCCEEDED(a)) {
            IDirect3DDevice8_SetVertexShader(dev, hv);
            IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)t);
            if (SUCCEEDED(b)) IDirect3DDevice8_SetPixelShader(dev, hp);
            IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
        }
        end();
        reset();
        shot("shaders", 2);
        if (SUCCEEDED(a)) IDirect3DDevice8_DeleteVertexShader(dev, hv);
        if (SUCCEEDED(b)) IDirect3DDevice8_DeletePixelShader(dev, hp);
        IDirect3DTexture8_Release(t);
    }
    /* CopyRects: an image surface into the back buffer, whole, with rects and points; then the
     * lockable back buffer written directly */
    {
        IDirect3DSurface8 *s = NULL;
        IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x000000, 1, 0);
        if (SUCCEEDED(IDirect3DDevice8_CreateImageSurface(dev, 16, 16, D3DFMT_X8R8G8B8, &s))) {
            D3DLOCKED_RECT lr;
            IDirect3DSurface8_LockRect(s, &lr, NULL, 0);
            for (int y = 0; y < 16; y++)
                for (int x = 0; x < 16; x++) ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] = (DWORD)(x * 16) << 16 | (DWORD)(y * 16) << 8 | 0x40;
            IDirect3DSurface8_UnlockRect(s);
            printf("CopyRects whole %#lx\n", IDirect3DDevice8_CopyRects(dev, s, NULL, 0, back, NULL));
            RECT r[2] = { { 0, 0, 8, 8 }, { 4, 6, 16, 10 } };
            POINT p[2] = { { 30, 2 }, { 40, 40 } };
            printf("CopyRects rects %#lx\n", IDirect3DDevice8_CopyRects(dev, s, r, 2, back, p));
            printf("CopyRects rects no points %#lx\n", IDirect3DDevice8_CopyRects(dev, s, r + 1, 1, back, NULL));
            IDirect3DSurface8_Release(s);
        }
        D3DLOCKED_RECT lr;
        RECT lk = { 20, 20, 28, 60 };
        hr = IDirect3DSurface8_LockRect(back, &lr, &lk, 0);
        printf("LockRect back buffer %#lx\n", hr);
        if (SUCCEEDED(hr)) {
            for (int y = 0; y < 40; y++)
                for (int x = 0; x < 8; x++) ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] = 0xff000000u | (DWORD)(y * 6) << 16 | (DWORD)(x * 32);
            IDirect3DSurface8_UnlockRect(back);
        }
        shot("copyrects", 0);
    }
    printf("TestCooperativeLevel %#lx\n", IDirect3DDevice8_TestCooperativeLevel(dev));
    printf("Present %#lx\n", IDirect3DDevice8_Present(dev, NULL, NULL, NULL, NULL));
    IDirect3DSurface8_Release(sys);
    IDirect3DSurface8_Release(back);
    printf("Release device %lu\n", IDirect3DDevice8_Release(dev));
    printf("Release d3d %lu\n", IDirect3D8_Release(d3d));
    return 0;
}
