/* Direct3D 9: what a frame looks like after clears, pre-transformed triangles (fill rules,
 * Gouraud), texture sampling (point/linear, address modes, formats incl. DXT), texture stage
 * operations, blending and alpha test, depth and scissor tests, StretchRect / ColorFill, plus
 * device caps and format support a game asks for. Each picture is the 64x64 back buffer read
 * back with GetRenderTargetData. On the Windows runner the device is WARP (Microsoft Basic
 * Render Driver).
 * libs: -ld3d9 -lgdi32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 64
#define H 64

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *sys;

static void shot(const char *name, int tol) {
    D3DLOCKED_RECT lr;
    HRESULT hr = IDirect3DDevice9_GetRenderTargetData(dev, back, sys);
    if (FAILED(hr)) { printf("%s: GetRenderTargetData %#lx\n", name, hr); return; }
    if (FAILED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) { printf("%s: LockRect failed\n", name); return; }
    probe_image(name, lr.pBits, W, H, lr.Pitch, tol);
    IDirect3DSurface9_UnlockRect(sys);
}

struct tlv { float x, y, z, rhw; DWORD c; float u, v; };
#define FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1)

static void quad(float x0, float y0, float x1, float y1, DWORD c, float u0, float v0, float u1, float v1) {
    struct tlv q[4] = {
        { x0, y0, 0.5f, 1, c, u0, v0 }, { x1, y0, 0.5f, 1, c, u1, v0 },
        { x0, y1, 0.5f, 1, c, u0, v1 }, { x1, y1, 0.5f, 1, c, u1, v1 },
    };
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof q[0]);
}

static void begin(DWORD clear) {
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER | D3DCLEAR_STENCIL, clear, 1.0f, 0);
    IDirect3DDevice9_BeginScene(dev);
}
static void end(void) { IDirect3DDevice9_EndScene(dev); }

/* default states between pictures */
static void reset(void) {
    IDirect3DDevice9_SetFVF(dev, FVF);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZWRITEENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHATESTENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_BLENDOP, D3DBLENDOP_ADD);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_ONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_ZERO);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, 0xf);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SHADEMODE, D3DSHADE_GOURAUD);
    for (int s = 0; s < 2; s++) {
        IDirect3DDevice9_SetTexture(dev, s, NULL);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_COLOROP, s ? D3DTOP_DISABLE : D3DTOP_MODULATE);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_COLORARG2, s ? D3DTA_CURRENT : D3DTA_DIFFUSE);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_ALPHAOP, s ? D3DTOP_DISABLE : D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
        IDirect3DDevice9_SetTextureStageState(dev, s, D3DTSS_TEXCOORDINDEX, 0);
        IDirect3DDevice9_SetSamplerState(dev, s, D3DSAMP_MINFILTER, D3DTEXF_POINT);
        IDirect3DDevice9_SetSamplerState(dev, s, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
        IDirect3DDevice9_SetSamplerState(dev, s, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        IDirect3DDevice9_SetSamplerState(dev, s, D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP);
        IDirect3DDevice9_SetSamplerState(dev, s, D3DSAMP_ADDRESSV, D3DTADDRESS_WRAP);
    }
    D3DVIEWPORT9 vp = { 0, 0, W, H, 0, 1 };
    IDirect3DDevice9_SetViewport(dev, &vp);
}

/* a w x h A8R8G8B8 texture from pixels */
static IDirect3DTexture9 *tex32(int w, int h, const DWORD *px) {
    IDirect3DTexture9 *t = NULL;
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_CreateTexture(dev, w, h, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL))) return NULL;
    IDirect3DTexture9_LockRect(t, 0, &lr, NULL, 0);
    for (int y = 0; y < h; y++) memcpy((char *)lr.pBits + y * lr.Pitch, px + y * w, w * 4);
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

/* a texture of any format from raw rows (row = bytes per row of blocks for DXT) */
static IDirect3DTexture9 *texfmt(D3DFORMAT f, int w, int h, const void *data, int row, int rows) {
    IDirect3DTexture9 *t = NULL;
    D3DLOCKED_RECT lr;
    HRESULT hr = IDirect3DDevice9_CreateTexture(dev, w, h, 1, 0, f, D3DPOOL_MANAGED, &t, NULL);
    if (FAILED(hr)) return NULL;
    IDirect3DTexture9_LockRect(t, 0, &lr, NULL, 0);
    for (int y = 0; y < rows; y++) memcpy((char *)lr.pBits + y * lr.Pitch, (const char *)data + y * row, row);
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

static DWORD checker[16];

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = TRUE; pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);

    /* what games ask before they draw */
    {
        D3DCAPS9 c;
        IDirect3DDevice9_GetDeviceCaps(dev, &c);
        printf("caps vs %lx ps %lx maxtex %lux%lu stages %lu textures %lu\n", c.VertexShaderVersion, c.PixelShaderVersion,
               c.MaxTextureWidth, c.MaxTextureHeight, c.MaxTextureBlendStages, c.MaxSimultaneousTextures);
        printf("caps texture pow2 %d nonpow2cond %d square %d alpha %d mipmap %d\n", !!(c.TextureCaps & D3DPTEXTURECAPS_POW2),
               !!(c.TextureCaps & D3DPTEXTURECAPS_NONPOW2CONDITIONAL), !!(c.TextureCaps & D3DPTEXTURECAPS_SQUAREONLY),
               !!(c.TextureCaps & D3DPTEXTURECAPS_ALPHA), !!(c.TextureCaps & D3DPTEXTURECAPS_MIPMAP));
        printf("caps blend src %lx dest %lx scissor %d separatealpha %d\n", c.SrcBlendCaps, c.DestBlendCaps,
               !!(c.RasterCaps & D3DPRASTERCAPS_SCISSORTEST), !!(c.PrimitiveMiscCaps & D3DPMISCCAPS_SEPARATEALPHABLEND));
        static const struct { D3DFORMAT f; const char *n; } fm[] = {
            { D3DFMT_A8R8G8B8, "A8R8G8B8" }, { D3DFMT_X8R8G8B8, "X8R8G8B8" }, { D3DFMT_R5G6B5, "R5G6B5" },
            { D3DFMT_A1R5G5B5, "A1R5G5B5" }, { D3DFMT_A4R4G4B4, "A4R4G4B4" }, { D3DFMT_X1R5G5B5, "X1R5G5B5" },
            { D3DFMT_A8, "A8" }, { D3DFMT_L8, "L8" }, { D3DFMT_A8L8, "A8L8" }, { D3DFMT_P8, "P8" },
            { D3DFMT_DXT1, "DXT1" }, { D3DFMT_DXT3, "DXT3" }, { D3DFMT_DXT5, "DXT5" }, { D3DFMT_A8B8G8R8, "A8B8G8R8" },
            { D3DFMT_A2R10G10B10, "A2R10G10B10" }, { D3DFMT_A16B16G16R16F, "A16B16G16R16F" }, { D3DFMT_A32B32G32R32F, "A32B32G32R32F" },
            { D3DFMT_R32F, "R32F" }, { D3DFMT_D16, "D16" }, { D3DFMT_D24S8, "D24S8" }, { D3DFMT_D24X8, "D24X8" },
            { D3DFMT_D32, "D32" }, { D3DFMT_R8G8B8, "R8G8B8" }, { D3DFMT_V8U8, "V8U8" }, { D3DFMT_UYVY, "UYVY" }, { D3DFMT_YUY2, "YUY2" },
        };
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            int depth = fm[i].f == D3DFMT_D16 || fm[i].f == D3DFMT_D24S8 || fm[i].f == D3DFMT_D24X8 || fm[i].f == D3DFMT_D32;
            HRESULT t = IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, 0, D3DRTYPE_TEXTURE, fm[i].f);
            HRESULT r = IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_RENDERTARGET, D3DRTYPE_TEXTURE, fm[i].f);
            HRESULT d = IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_DEPTHSTENCIL, D3DRTYPE_SURFACE, fm[i].f);
            printf("format %s texture %d rendertarget %d depth %d\n", fm[i].n, t == D3D_OK, r == D3D_OK, depth ? d == D3D_OK : -1);
        }
        printf("CheckDeviceType windowed %d\n", IDirect3D9_CheckDeviceType(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DFMT_X8R8G8B8, TRUE) == D3D_OK);
    }

    reset();
    /* clears: whole target, two rects, inside a smaller viewport */
    {
        D3DRECT r[2] = { { 4, 4, 20, 30 }, { 15, 25, 40, 60 } };
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x203040, 1, 0);
        IDirect3DDevice9_Clear(dev, 2, r, D3DCLEAR_TARGET, 0xff8000, 1, 0);
        D3DVIEWPORT9 vp = { 44, 8, 16, 50, 0, 1 };
        IDirect3DDevice9_SetViewport(dev, &vp);
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x00ff80, 1, 0);
        D3DRECT r2 = { 30, 0, 50, 12 };
        IDirect3DDevice9_Clear(dev, 1, &r2, D3DCLEAR_TARGET, 0xffffff, 1, 0); /* clipped to the viewport */
        reset();
        shot("clear", 0);
    }
    /* rasterization: a Gouraud triangle, flat shading, edges on pixel centres and halves
     * (top-left rule), a fan sharing edges (no pixel twice: additive blending shows it) */
    {
        begin(0);
        struct tlv t[3] = { { 2, 2, 0.5f, 1, 0xffff0000 }, { 40, 10, 0.5f, 1, 0xff00ff00 }, { 10, 40, 0.5f, 1, 0xff0000ff } };
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, t, sizeof t[0]);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SHADEMODE, D3DSHADE_FLAT);
        struct tlv f[3] = { { 44, 2, 0.5f, 1, 0xffffff00 }, { 62, 2, 0.5f, 1, 0xff00ffff }, { 53, 20, 0.5f, 1, 0xffff00ff } };
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, f, sizeof f[0]);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SHADEMODE, D3DSHADE_GOURAUD);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_ONE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_ONE);
        struct tlv fan[8];
        fan[0] = (struct tlv){ 30.5f, 46.5f, 0.5f, 1, 0xff404040 };
        for (int i = 0; i < 7; i++) {
            float a = i * 6.2831853f / 6;
            fan[i + 1] = (struct tlv){ 30.5f + 15.25f * __builtin_cosf(a), 46.5f + 15.25f * __builtin_sinf(a), 0.5f, 1, 0xff404040 };
        }
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLEFAN, 6, fan, sizeof fan[0]);
        /* thin slivers and exact-centre edges */
        quad(50, 30, 51, 62, 0xff808080, 0, 0, 0, 0);
        quad(52.5f, 30.5f, 54.5f, 62.5f, 0xff808080, 0, 0, 0, 0);
        quad(56.25f, 30.75f, 57.75f, 61.25f, 0xff808080, 0, 0, 0, 0);
        end();
        reset();
        shot("raster", 2);
    }
    /* texture sampling: a 4x4 texture, point and linear, 1:1 and enlarged */
    {
        for (int i = 0; i < 16; i++) checker[i] = 0xff000000 | (i & 3) * 0x55 << 16 | (i >> 2) * 0x55 << 8 | ((i ^ (i >> 2)) & 1) * 0xff;
        IDirect3DTexture9 *t = tex32(4, 4, checker);
        begin(0x101010);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
        quad(-0.5f + 2, -0.5f + 2, 4 - 0.5f + 2, 4 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        quad(-0.5f + 8, -0.5f + 2, 24 - 0.5f + 8, 24 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        quad(-0.5f + 36, -0.5f + 2, 24 - 0.5f + 36, 24 - 0.5f + 2, 0xffffffff, 0, 0, 1, 1);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
        quad(-0.5f + 8, -0.5f + 32, 24 - 0.5f + 8, 24 - 0.5f + 32, 0xffffffff, 0, 0, 1, 1);
        /* shrunk (linear between texels) and modulated by the vertex color */
        quad(-0.5f + 36, -0.5f + 32, 6 - 0.5f + 36, 6 - 0.5f + 32, 0xff80ff40, 0, 0, 1, 1);
        quad(-0.5f + 44, -0.5f + 32, 24 - 0.5f + 44, 24 - 0.5f + 32, 0xff80ff40, 0.125f, 0.125f, 0.875f, 0.875f);
        end();
        reset();
        shot("sampling", 10); /* (WARP weighs texels in 16ths) */
        /* address modes: texcoords -1..2 */
        begin(0);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
        static const DWORD modes[4] = { D3DTADDRESS_WRAP, D3DTADDRESS_MIRROR, D3DTADDRESS_CLAMP, D3DTADDRESS_BORDER };
        for (int m = 0; m < 4; m++) {
            IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, modes[m]);
            IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, modes[m]);
            IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_BORDERCOLOR, 0xff40c0e0);
            float x = (m & 1) * 32, y = (m >> 1) * 32;
            quad(x - 0.5f + 2, y - 0.5f + 2, x + 30 - 0.5f, y + 30 - 0.5f, 0xffffffff, -1, -1, 2, 2);
        }
        end();
        reset();
        shot("address", 0);
        IDirect3DTexture9_Release(t);
    }
    /* texture formats, each 8x8 drawn 1:1 (point) in a 8-pixel cell */
    {
        begin(0x808080);
        int cell = 0;
        unsigned short w16[64];
        unsigned char b8[64], al[128];
        DWORD w32[64];
        for (int i = 0; i < 64; i++) {
            w16[i] = (unsigned short)(i * 1031 + 0x1234);
            b8[i] = (unsigned char)(i * 4 + 2);
            al[2 * i] = (unsigned char)(i * 4); al[2 * i + 1] = (unsigned char)(255 - i * 4);
            w32[i] = 0x80000000u | (unsigned)i * 0x030507;
        }
        /* DXT blocks: 2x2 of 4x4 blocks */
        static const unsigned char dxt1[32] = { 0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
                                                0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char dxt5[64] = {
            0xff, 0x00, 0x88, 0xc6, 0x3e, 0x88, 0xc6, 0x3e,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0x40, 0xc0, 0x24, 0x49, 0x92, 0x24, 0x49, 0x92,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0x10, 0x20, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x80, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        static const unsigned char dxt3[64] = {
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe,  0x00, 0xf8, 0x1f, 0x00, 0xe4, 0xe4, 0x1b, 0x1b,
            0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00,  0xe0, 0x07, 0x00, 0x00, 0x00, 0x55, 0xaa, 0xff,
            0xf0, 0xf0, 0xf0, 0xf0, 0x0f, 0x0f, 0x0f, 0x0f,  0x1f, 0x00, 0xe0, 0xff, 0x27, 0x27, 0x27, 0x27,
            0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88,  0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00 };
        struct { D3DFORMAT f; const char *n; const void *d; int row, rows; } fm[] = {
            { D3DFMT_A8R8G8B8, "A8R8G8B8", w32, 32, 8 }, { D3DFMT_X8R8G8B8, "X8R8G8B8", w32, 32, 8 },
            { D3DFMT_R5G6B5, "R5G6B5", w16, 16, 8 }, { D3DFMT_X1R5G5B5, "X1R5G5B5", w16, 16, 8 },
            { D3DFMT_A1R5G5B5, "A1R5G5B5", w16, 16, 8 }, { D3DFMT_A4R4G4B4, "A4R4G4B4", w16, 16, 8 },
            { D3DFMT_L8, "L8", b8, 8, 8 }, { D3DFMT_A8, "A8", b8, 8, 8 }, { D3DFMT_A8L8, "A8L8", al, 16, 8 },
            { D3DFMT_DXT1, "DXT1", dxt1, 16, 2 }, { D3DFMT_DXT3, "DXT3", dxt3, 32, 2 }, { D3DFMT_DXT5, "DXT5", dxt5, 32, 2 },
            { D3DFMT_A8B8G8R8, "A8B8G8R8", w32, 32, 8 }, { D3DFMT_X4R4G4B4, "X4R4G4B4", w16, 16, 8 },
        };
        /* alpha shows as a blend over the gray background */
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        for (int i = 0; i < (int)(sizeof fm / sizeof fm[0]); i++) {
            IDirect3DTexture9 *t = texfmt(fm[i].f, 8, 8, fm[i].d, fm[i].row, fm[i].rows);
            printf("texture %s %s\n", fm[i].n, t ? "ok" : "failed");
            if (!t) continue;
            IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
            float x = (cell % 7) * 9 + 1, y = (cell / 7) * 9 + 1;
            quad(x - 0.5f, y - 0.5f, x + 8 - 0.5f, y + 8 - 0.5f, 0xffffffff, 0, 0, 1, 1);
            cell++;
            IDirect3DTexture9_Release(t);
        }
        end();
        reset();
        shot("formats", 1);
    }
    /* blending: over a gradient, src alpha, add, multiply, reverse subtract, min, max, separate
     * alpha factors, alpha test */
    {
        begin(0);
        struct tlv g[4] = { { 0, 0, 0.5f, 1, 0xff0000ff }, { 64, 0, 0.5f, 1, 0xffff0000 }, { 0, 64, 0.5f, 1, 0xff00ff00 }, { 64, 64, 0.5f, 1, 0xffffffff } };
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG2);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, g, sizeof g[0]);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
        static const DWORD bl[8][3] = {
            { D3DBLENDOP_ADD, D3DBLEND_SRCALPHA, D3DBLEND_INVSRCALPHA }, { D3DBLENDOP_ADD, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_ADD, D3DBLEND_DESTCOLOR, D3DBLEND_ZERO }, { D3DBLENDOP_REVSUBTRACT, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_SUBTRACT, D3DBLEND_SRCALPHA, D3DBLEND_ONE }, { D3DBLENDOP_MIN, D3DBLEND_ONE, D3DBLEND_ONE },
            { D3DBLENDOP_MAX, D3DBLEND_ONE, D3DBLEND_ONE }, { D3DBLENDOP_ADD, D3DBLEND_INVDESTCOLOR, D3DBLEND_SRCCOLOR },
        };
        for (int i = 0; i < 8; i++) {
            IDirect3DDevice9_SetRenderState(dev, D3DRS_BLENDOP, bl[i][0]);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, bl[i][1]);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, bl[i][2]);
            float x = (i & 3) * 16, y = (i >> 2) * 16;
            quad(x + 2, y + 2, x + 14, y + 14, 0x80c06020, 0, 0, 0, 0);
        }
        IDirect3DDevice9_SetRenderState(dev, D3DRS_BLENDOP, D3DBLENDOP_ADD);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        /* alpha test on a horizontal alpha ramp */
        struct tlv a[4] = { { 2, 34, 0.5f, 1, 0x00ffffff }, { 62, 34, 0.5f, 1, 0xffffffff }, { 2, 46, 0.5f, 1, 0x00ffffff }, { 62, 46, 0.5f, 1, 0xffffffff } };
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHATESTENABLE, TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHAREF, 0x80);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHAFUNC, D3DCMP_GREATER);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, a, sizeof a[0]);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHAFUNC, D3DCMP_LESS);
        for (int i = 0; i < 4; i++) a[i].y += 14;
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, a, sizeof a[0]);
        end();
        reset();
        shot("blend", 2);
    }
    /* texture stage operations on a texture x diffuse, TFACTOR, two stages */
    {
        IDirect3DTexture9 *t = tex32(4, 4, checker);
        DWORD c2[16];
        for (int i = 0; i < 16; i++) c2[i] = 0x80000000u | (unsigned)(i * 16) << 16 | 0x4080;
        IDirect3DTexture9 *t2 = tex32(4, 4, c2);
        begin(0);
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_TEXTUREFACTOR, 0x8040c0ff);
        static const DWORD ops[12] = { D3DTOP_MODULATE, D3DTOP_MODULATE2X, D3DTOP_MODULATE4X, D3DTOP_ADD, D3DTOP_ADDSIGNED,
                                       D3DTOP_ADDSIGNED2X, D3DTOP_SUBTRACT, D3DTOP_ADDSMOOTH, D3DTOP_BLENDDIFFUSEALPHA,
                                       D3DTOP_BLENDTEXTUREALPHA, D3DTOP_DOTPRODUCT3, D3DTOP_SELECTARG2 };
        for (int i = 0; i < 12; i++) {
            IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, ops[i]);
            IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, i == 11 ? D3DTA_TFACTOR : D3DTA_DIFFUSE);
            float x = (i % 4) * 16, y = (i / 4) * 16;
            quad(x - 0.5f, y - 0.5f, x + 16 - 0.5f, y + 16 - 0.5f, 0x80608090, 0, 0, 1, 1);
        }
        /* two stages: stage 1 modulates stage 0's result with a second texture, alpha from it */
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
        IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)t2);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_COLOROP, D3DTOP_ADD);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_COLORARG2, D3DTA_CURRENT);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_TEXCOORDINDEX, 0);
        quad(-0.5f, 48 - 0.5f, 32 - 0.5f, 64 - 0.5f, 0xffc08040, 0, 0, 1, 1);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_COLOROP, D3DTOP_LERP);
        IDirect3DDevice9_SetTextureStageState(dev, 1, D3DTSS_COLORARG0, D3DTA_TEXTURE | D3DTA_ALPHAREPLICATE);
        quad(32 - 0.5f, 48 - 0.5f, 64 - 0.5f, 64 - 0.5f, 0xffc08040, 0, 0, 1, 1);
        end();
        reset();
        shot("stages", 2);
        IDirect3DTexture9_Release(t);
        IDirect3DTexture9_Release(t2);
    }
    /* depth (LESS, write on/off, GREATER) and scissor */
    {
        begin(0x000020);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_TRUE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_LESS);
        struct tlv a[3] = { { 2, 2, 0.2f, 1, 0xffff0000 }, { 40, 2, 0.8f, 1, 0xffff0000 }, { 2, 40, 0.8f, 1, 0xffff0000 } };
        struct tlv b[3] = { { 40, 40, 0.2f, 1, 0xff00ff00 }, { 2, 10, 0.5f, 1, 0xff00ff00 }, { 30, 2, 0.5f, 1, 0xff00ff00 } };
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, a, sizeof a[0]);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, b, sizeof b[0]);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZWRITEENABLE, FALSE);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_GREATER);
        quad(10, 10, 30, 30, 0xff0000ff, 0, 0, 0, 0);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_FALSE);
        RECT sc = { 44, 4, 60, 60 };
        IDirect3DDevice9_SetScissorRect(dev, &sc);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, TRUE);
        quad(36, 20, 64, 50, 0xffffff00, 0, 0, 0, 0);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE, D3DCOLORWRITEENABLE_RED);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, FALSE);
        quad(2, 46, 40, 62, 0xff80ffff, 0, 0, 0, 0);
        end();
        reset();
        shot("depth_scissor", 1);
    }
    /* surfaces: ColorFill, StretchRect (point, linear, a sub-rect, from a texture level) */
    {
        IDirect3DSurface9 *s = NULL;
        IDirect3DDevice9_CreateOffscreenPlainSurface(dev, 8, 8, D3DFMT_X8R8G8B8, D3DPOOL_DEFAULT, &s, NULL);
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0x000000, 1, 0);
        RECT f = { 0, 0, 8, 8 };
        hr = IDirect3DDevice9_ColorFill(dev, s, &f, 0xff2080c0);
        printf("ColorFill %#lx\n", hr);
        RECT f2 = { 2, 2, 6, 6 };
        IDirect3DDevice9_ColorFill(dev, s, &f2, 0xffffff00);
        RECT d1 = { 0, 0, 24, 24 }, d2 = { 32, 0, 56, 24 }, d3 = { 0, 32, 10, 42 }, s3 = { 1, 1, 7, 5 };
        printf("StretchRect point %#lx\n", IDirect3DDevice9_StretchRect(dev, s, NULL, back, &d1, D3DTEXF_POINT));
        printf("StretchRect linear %#lx\n", IDirect3DDevice9_StretchRect(dev, s, NULL, back, &d2, D3DTEXF_LINEAR));
        printf("StretchRect sub %#lx\n", IDirect3DDevice9_StretchRect(dev, s, &s3, back, &d3, D3DTEXF_NONE));
        RECT bf = { 40, 40, 60, 60 };
        IDirect3DDevice9_ColorFill(dev, back, &bf, 0xffc04080);
        shot("surfaces", 2);
        IDirect3DSurface9_Release(s);
    }
    /* what Present and a lost device report in windowed mode */
    printf("TestCooperativeLevel %#lx\n", IDirect3DDevice9_TestCooperativeLevel(dev));
    printf("Present %#lx\n", IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL));
    IDirect3DSurface9_Release(sys);
    IDirect3DSurface9_Release(back);
    printf("Release device %lu\n", IDirect3DDevice9_Release(dev));
    printf("Release d3d %lu\n", IDirect3D9_Release(d3d));
    return 0;
}
