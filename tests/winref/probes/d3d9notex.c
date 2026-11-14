/* Direct3D 9 fixed-function texture stages that read D3DTA_TEXTURE where no texture is set.
 * 正しい性奴○の使い方 (アイリスフィールド) draws every sprite with stage 0 MODULATE TEXTURE
 * DIFFUSE (its texture set) and leaves stage 1 at MODULATE TEXTURE DIFFUSE with no texture: on
 * Windows the picture shows, so stage 1 passes the current color through. A pipeline that samples
 * "no texture" as white makes stage 1 give the diffuse, white for every sprite: a white screen.
 * Here: one stage set up at a time after stage 0, or stage 0 itself, with and without a texture,
 * each as a 4x4 picture (rgb, and the alpha replicated by a last SELECTARG1 CURRENT|ALPHAREPLICATE
 * stage). Wine's rule (is_invalid_op): an op that reads the texture with none set becomes
 * SELECTARG1 CURRENT; arg1 with SELECTARG2 and arg2 with SELECTARG1 are not read, arg0 only by
 * MULTIPLYADD and LERP.
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 4
#define H 4

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *sys;
static IDirect3DTexture9 *tex;

static void shot(const char *name) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, sys)) || FAILED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) {
        printf("%s: no readback\n", name);
        return;
    }
    probe_image(name, lr.pBits, W, H, lr.Pitch, 2);
    IDirect3DSurface9_UnlockRect(sys);
}

struct tlv { float x, y, z, rhw; DWORD c, s; float u, v; };

static void draw(void) {
    float x0 = -0.5f, y0 = -0.5f, x1 = W - 0.5f, y1 = H - 0.5f;
    DWORD c = 0x80c08040, s = 0x00204060;
    struct tlv v[4] = { { x0, y0, 0.5f, 1, c, s, 0, 0 }, { x1, y0, 0.5f, 1, c, s, 1, 0 }, { x0, y1, 0.5f, 1, c, s, 0, 1 }, { x1, y1, 0.5f, 1, c, s, 1, 1 } };
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xff102030, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_SPECULAR | D3DFVF_TEX1);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
    IDirect3DDevice9_EndScene(dev);
}

static void tss(int s, D3DTEXTURESTAGESTATETYPE t, DWORD v) { IDirect3DDevice9_SetTextureStageState(dev, s, t, v); }
static void stage(int s, DWORD cop, DWORD c1, DWORD c2, DWORD c0, DWORD aop, DWORD a1, DWORD a2, DWORD a0) {
    tss(s, D3DTSS_COLOROP, cop); tss(s, D3DTSS_COLORARG1, c1); tss(s, D3DTSS_COLORARG2, c2); tss(s, D3DTSS_COLORARG0, c0);
    tss(s, D3DTSS_ALPHAOP, aop); tss(s, D3DTSS_ALPHAARG1, a1); tss(s, D3DTSS_ALPHAARG2, a2); tss(s, D3DTSS_ALPHAARG0, a0);
}
static void off(int s) { tss(s, D3DTSS_COLOROP, D3DTOP_DISABLE); tss(s, D3DTSS_ALPHAOP, D3DTOP_DISABLE); }

struct st { const char *name; DWORD cop, c1, c2, c0, aop, a1, a2, a0; };

/* the stage under test, with D3DTA_TEXTURE in various places */
static const struct st cases[] = {
    { "modulate_tex_diffuse", D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT },
    { "select1_tex", D3DTOP_SELECTARG1, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT },
    { "select2_arg1tex", D3DTOP_SELECTARG2, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTOP_SELECTARG2, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT },
    { "select1_arg2tex", D3DTOP_SELECTARG1, D3DTA_TFACTOR, D3DTA_TEXTURE, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_TFACTOR, D3DTA_TEXTURE, D3DTA_CURRENT },
    { "add_no_tex", D3DTOP_ADD, D3DTA_DIFFUSE, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTOP_ADD, D3DTA_DIFFUSE, D3DTA_TFACTOR, D3DTA_CURRENT },
    { "complement_tex", D3DTOP_MODULATE, D3DTA_TEXTURE | D3DTA_COMPLEMENT, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTOP_SELECTARG2, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT },
    { "lerp_arg0tex", D3DTOP_LERP, D3DTA_TFACTOR, D3DTA_SPECULAR, D3DTA_TEXTURE, D3DTOP_LERP, D3DTA_TFACTOR, D3DTA_DIFFUSE, D3DTA_TEXTURE },
    { "madd_arg0tex", D3DTOP_MULTIPLYADD, D3DTA_TFACTOR, D3DTA_SPECULAR, D3DTA_TEXTURE, D3DTOP_MULTIPLYADD, D3DTA_TFACTOR, D3DTA_DIFFUSE, D3DTA_TEXTURE },
    { "add_arg0tex", D3DTOP_ADD, D3DTA_TFACTOR, D3DTA_SPECULAR, D3DTA_TEXTURE, D3DTOP_ADD, D3DTA_TFACTOR, D3DTA_SPECULAR, D3DTA_TEXTURE },
    { "alpha_only_tex", D3DTOP_ADD, D3DTA_CURRENT, D3DTA_SPECULAR, D3DTA_CURRENT, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT },
    { "color_only_tex", D3DTOP_MODULATE, D3DTA_TFACTOR, D3DTA_TEXTURE, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTA_CURRENT },
};

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9notex", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
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
    IDirect3DDevice9_SetRenderState(dev, D3DRS_TEXTUREFACTOR, 0x60406080);

    /* a texture of one color, a0 a8 c8 30 (A R G B) */
    hr = IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &tex, NULL);
    D3DLOCKED_RECT lr;
    printf("texture %#lx\n", hr);
    if (SUCCEEDED(hr) && SUCCEEDED(IDirect3DTexture9_LockRect(tex, 0, &lr, NULL, 0))) {
        for (int y = 0; y < 4; y++)
            for (int x = 0; x < 4; x++) ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] = 0xa0a8c830;
        IDirect3DTexture9_UnlockRect(tex, 0);
    }
    char n[96];
    for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++) {
        const struct st *c = &cases[i];
        /* (1) the stage as stage 0, no texture; then with the texture */
        for (int t = 0; t < 2; t++) {
            IDirect3DDevice9_SetTexture(dev, 0, t ? (IDirect3DBaseTexture9 *)tex : NULL);
            IDirect3DDevice9_SetTexture(dev, 1, NULL);
            stage(0, c->cop, c->c1, c->c2, c->c0, c->aop, c->a1, c->a2, c->a0);
            for (int a = 0; a < 2; a++) {
                if (a) stage(1, D3DTOP_SELECTARG1, D3DTA_CURRENT | D3DTA_ALPHAREPLICATE, D3DTA_CURRENT, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_CURRENT, D3DTA_CURRENT, D3DTA_CURRENT);
                else off(1);
                off(2);
                draw();
                snprintf(n, sizeof n, "s0_%s_%s_%s", c->name, t ? "tex" : "none", a ? "alpha" : "rgb");
                shot(n);
            }
        }
        /* (2) stage 1, no texture of its own, after stage 0 = the texture * diffuse (the game) */
        IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
        IDirect3DDevice9_SetTexture(dev, 1, NULL);
        stage(0, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_DIFFUSE, D3DTA_CURRENT);
        stage(1, c->cop, c->c1, c->c2, c->c0, c->aop, c->a1, c->a2, c->a0);
        for (int a = 0; a < 2; a++) {
            if (a) stage(2, D3DTOP_SELECTARG1, D3DTA_CURRENT | D3DTA_ALPHAREPLICATE, D3DTA_CURRENT, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_CURRENT, D3DTA_CURRENT, D3DTA_CURRENT);
            else off(2);
            off(3);
            draw();
            snprintf(n, sizeof n, "s1_%s_%s", c->name, a ? "alpha" : "rgb");
            shot(n);
        }
        off(1);
    }
    /* (3) a stage with no texture between two textured ones: stage 2 still reads its texture */
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)tex);
    IDirect3DDevice9_SetTexture(dev, 1, NULL);
    IDirect3DDevice9_SetTexture(dev, 2, (IDirect3DBaseTexture9 *)tex);
    stage(0, D3DTOP_SELECTARG1, D3DTA_DIFFUSE, D3DTA_CURRENT, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_DIFFUSE, D3DTA_CURRENT, D3DTA_CURRENT);
    stage(1, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_TFACTOR, D3DTA_CURRENT);
    stage(2, D3DTOP_MODULATE, D3DTA_TEXTURE, D3DTA_CURRENT, D3DTA_CURRENT, D3DTOP_SELECTARG1, D3DTA_CURRENT, D3DTA_CURRENT, D3DTA_CURRENT);
    off(3);
    draw();
    shot("between_rgb");
    return 0;
}
