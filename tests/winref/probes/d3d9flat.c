/* Direct3D 9 SHADEMODE flat (D3DSHADE_FLAT): each triangle takes the colors of its first
 * vertex, with the fixed function pipeline and with shaders too. 幻聖神姫LINEAGE / ヤクモ
 * (Triangle) draw every frame as one textured triangle list of XYZRHW | DIFFUSE | TEX2 vertices
 * whose colors are set only on the first vertex of each triangle (the others stay 0), with
 * SHADEMODE flat and a ps_2_0 shader (rgb = texture * v0, a = v0.a); a pipeline that
 * interpolates the colors anyway darkens the picture toward two corners. Here: quads with a
 * different color at each corner (as a list and as a strip), GOURAUD then FLAT, through (a) the
 * fixed function pipeline, (b) the fixed function vertex stage with a ps_2_0 shader (the
 * game's own shader, then one that passes v0 and v1), (c) vs_2_0 + ps_2_0, (d) ps_1_1. The
 * pictures are 8x8 per quad.
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 8
#define H 8

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *sys;

static void shot(const char *name) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, sys)) || FAILED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) {
        printf("%s: no readback\n", name);
        return;
    }
    /* (colors from interpolation differ by a step or two between GPUs) */
    probe_image(name, lr.pBits, W, H, lr.Pitch, 3);
    IDirect3DSurface9_UnlockRect(sys);
}

struct tlv { float x, y, z, rhw; DWORD c, s; float u, v; };

static const DWORD corners[4] = { 0xffff0000, 0xff00ff00, 0xff0000ff, 0xffffffff };

/* a quad as the game draws it: a list of two triangles, (TL, TR, BL) (BR, BL, TR), corner
 * colors in diffuse and the next corner's at half in specular; strip: four vertices TL TR BL BR */
#define SPEC(i) ((corners[((i) + 1) % 4] >> 1) & 0x7f7f7f)
static void quad(int strip) {
    float x0 = -0.5f, y0 = -0.5f, x1 = W - 0.5f, y1 = H - 0.5f;
    struct tlv c[4] = {
        { x0, y0, 0.5f, 1, corners[0], SPEC(0), 0, 0 }, { x1, y0, 0.5f, 1, corners[1], SPEC(1), 1, 0 },
        { x0, y1, 0.5f, 1, corners[2], SPEC(2), 0, 1 }, { x1, y1, 0.5f, 1, corners[3], SPEC(3), 1, 1 },
    };
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff404040, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_SPECULAR | D3DFVF_TEX1);
    if (strip) {
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, c, sizeof c[0]);
    } else {
        struct tlv l[6] = { c[0], c[1], c[2], c[3], c[2], c[1] };
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 2, l, sizeof l[0]);
    }
    IDirect3DDevice9_EndScene(dev);
}

static void both(const char *what) {
    char n[64];
    for (int mode = D3DSHADE_FLAT; mode <= D3DSHADE_GOURAUD; mode++) {
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SHADEMODE, mode);
        for (int strip = 0; strip < 2; strip++) {
            quad(strip);
            snprintf(n, sizeof n, "%s_%s_%s", what, mode == D3DSHADE_FLAT ? "flat" : "gouraud", strip ? "strip" : "list");
            shot(n);
        }
    }
}

/* ps_2_0: dcl v0; dcl t0.xy; dcl_2d s0; texld r0, t0, s0; mul r0.xyz, r0, v0; mov r0.w, v0.w;
 * mov oC0, r0 (幻聖神姫's, tokens as the game creates them) */
static const DWORD ps_game[] = {
    0xffff0200, 0x0200001f, 0x80000000, 0x900f0000, 0x0200001f, 0x80000000, 0xb0030000, 0x0200001f, 0x90000000, 0xa00f0800,
    0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x03000005, 0x80070000, 0x80e40000, 0x90e40000, 0x02000001, 0x80080000,
    0x90ff0000, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff,
};
/* ps_2_0: dcl v0; dcl v1; add r0, v0, v1 (.5 each: c0); mov oC0 */
static const DWORD ps_two[] = {
    0xffff0200, 0x05000051, 0xa00f0000, 0x3f000000, 0x3f000000, 0x3f000000, 0x3f800000,
    0x0200001f, 0x80000000, 0x900f0000, 0x0200001f, 0x80000000, 0x900f0001,
    0x03000005, 0x800f0000, 0x90e40000, 0xa0e40000, 0x04000004, 0x800f0000, 0x90e40001, 0xa0e40000, 0x80e40000,
    0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff,
};
/* vs_2_0: dcl_position v0; dcl_color v1; mov oPos, v0; mov oD0, v1 (positions already in clip space) */
static const DWORD vs_pass[] = {
    0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x0200001f, 0x8000000a, 0x900f0001,
    0x02000001, 0xc00f0000, 0x90e40000, 0x02000001, 0xd00f0000, 0x90e40001, 0x0000ffff,
};
/* ps_2_0: dcl v0; mov oC0, v0 */
static const DWORD ps_pass[] = {
    0xffff0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0x800f0800, 0x90e40000, 0x0000ffff,
};
/* ps_1_1: mov r0, v0 */
static const DWORD ps11[] = { 0xffff0101, 0x00000001, 0x800f0000, 0x90e40000, 0x0000ffff };

struct cv { float x, y, z, w; DWORD c; };

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9flat", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);

    /* a white texture (the game's texture: here every texel 1) */
    IDirect3DTexture9 *tex;
    hr = IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &tex, NULL);
    D3DLOCKED_RECT lr;
    if (SUCCEEDED(hr) && SUCCEEDED(IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0))) {
        for (int y = 0; y < 4; y++) memset((char *)lr.pBits + y * lr.Pitch, 0xff, 16);
        IDirect3DTexture9_UnlockRect(tex, 0);
    }

    /* (a) fixed function: texture * diffuse + specular */
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SPECULARENABLE, TRUE);
    both("ff");
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SPECULARENABLE, FALSE);

    /* (b) the fixed function vertex stage, pixel shaders */
    IDirect3DPixelShader9 *ps;
    hr = IDirect3DDevice9_CreatePixelShader(dev, ps_game, &ps);
    printf("game ps %#lx\n", hr);
    IDirect3DDevice9_SetPixelShader(dev, ps);
    both("ps_game");
    IDirect3DPixelShader9 *ps2;
    hr = IDirect3DDevice9_CreatePixelShader(dev, ps_two, &ps2);
    printf("two-color ps %#lx\n", hr);
    IDirect3DDevice9_SetPixelShader(dev, ps2);
    both("ps_two");
    IDirect3DPixelShader9 *p11;
    hr = IDirect3DDevice9_CreatePixelShader(dev, ps11, &p11);
    printf("ps_1_1 %#lx\n", hr);
    IDirect3DDevice9_SetPixelShader(dev, p11);
    both("ps11");

    /* (c) vs_2_0 + ps_2_0: clip-space positions and a color */
    IDirect3DVertexShader9 *vs;
    IDirect3DPixelShader9 *pp0;
    HRESULT h1 = IDirect3DDevice9_CreateVertexShader(dev, vs_pass, &vs), h2 = IDirect3DDevice9_CreatePixelShader(dev, ps_pass, &pp0);
    printf("vs %#lx ps %#lx\n", h1, h2);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, pp0);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZW | D3DFVF_DIFFUSE);
    for (int mode = D3DSHADE_FLAT; mode <= D3DSHADE_GOURAUD; mode++) {
        IDirect3DDevice9_SetRenderState(dev, D3DRS_SHADEMODE, mode);
        float a = -1 - 1.0f / W, b = 1 - 1.0f / W, t = 1 + 1.0f / H, u = -1 + 1.0f / H;
        struct cv c[4] = { { a, t, 0.5f, 1, corners[0] }, { b, t, 0.5f, 1, corners[1] }, { a, u, 0.5f, 1, corners[2] }, { b, u, 0.5f, 1, corners[3] } };
        struct cv l[6] = { c[0], c[1], c[2], c[3], c[2], c[1] };
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff404040, 1, 0);
        IDirect3DDevice9_BeginScene(dev);
        IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZW | D3DFVF_DIFFUSE);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 2, l, sizeof l[0]);
        IDirect3DDevice9_EndScene(dev);
        shot(mode == D3DSHADE_FLAT ? "vs_flat" : "vs_gouraud");
    }
    return 0;
}
