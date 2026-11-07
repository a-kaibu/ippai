/* Direct3D 9 Clear: what it returns and clears for rectangle lists (NULL with a count, a count
 * of 0 with a list, inverted, partly outside), the viewport and the scissor rectangle, depth and
 * stencil flags without a depth buffer, with one without stencil, smaller or larger than the
 * target, and every render target of a multiple render target set; depth and stencil clears seen
 * through the depth and stencil tests of a draw.
 * libs: -ld3d9
 * arch: 32
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <d3d9.h>

#define W 16
#define H 16

static IDirect3DDevice9 *dev;
static IDirect3DSurface9 *back, *sys;

/* the target's pixel (x, y) (back buffer unless t) */
static DWORD px(IDirect3DSurface9 *t, int x, int y) {
    D3DLOCKED_RECT lr;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, t ? t : back, sys))) return 0xbad0bad0;
    if (FAILED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) return 0xbad1bad1;
    DWORD v = ((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff;
    IDirect3DSurface9_UnlockRect(sys);
    return v;
}

/* What of the whole target is drawn (not black): "drawn N in x0,y0-x1,y1" (#63: a depth buffer
 * smaller than the target, and nothing drawn past it whatever the depth test) */
static const char *drawn(void) {
    static char out[64];
    D3DLOCKED_RECT lr;
    D3DSURFACE_DESC d;
    if (FAILED(IDirect3DDevice9_GetRenderTargetData(dev, back, sys))) return "bad";
    IDirect3DSurface9_GetDesc(sys, &d);
    if (FAILED(IDirect3DSurface9_LockRect(sys, &lr, NULL, D3DLOCK_READONLY))) return "bad";
    int n = 0, x0 = 99999, y0 = 99999, x1 = -1, y1 = -1;
    for (UINT y = 0; y < d.Height; y++)
        for (UINT x = 0; x < d.Width; x++)
            if (((DWORD *)((char *)lr.pBits + y * lr.Pitch))[x] & 0xffffff) {
                n++;
                if ((int)x < x0) x0 = x;
                if ((int)y < y0) y0 = y;
                if ((int)x > x1) x1 = x;
                if ((int)y > y1) y1 = y;
            }
    IDirect3DSurface9_UnlockRect(sys);
    if (!n) return "drawn 0";
    snprintf(out, sizeof out, "drawn %d in %d,%d-%d,%d", n, x0, y0, x1, y1);
    return out;
}

static HRESULT clr(DWORD n, const D3DRECT *r, DWORD flags, DWORD c, float z, DWORD s) {
    return IDirect3DDevice9_Clear(dev, n, r, flags, c, z, s);
}

struct tlv { float x, y, z, rhw; DWORD c; };

/* a quad over the whole target at depth z */
static void quad(float z, DWORD c) {
    struct tlv v[4] = { { -0.5f, -0.5f, z, 1, c }, { W - 0.5f, -0.5f, z, 1, c }, { -0.5f, H - 0.5f, z, 1, c }, { W - 0.5f, H - 0.5f, z, 1, c } };
    IDirect3DDevice9_BeginScene(dev);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof v[0]);
    IDirect3DDevice9_EndScene(dev);
}

int main(void) {
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("probe", "d3d9clear", WS_POPUP, 0, 0, W, H, 0, 0, wc.hInstance, 0);
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) { puts("skip no Direct3D 9"); return 0; }
    D3DPRESENT_PARAMETERS pp = { 0 };
    pp.BackBufferWidth = W; pp.BackBufferHeight = H; pp.BackBufferFormat = D3DFMT_X8R8G8B8; pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD; pp.hDeviceWindow = hwnd; pp.Windowed = TRUE;
    HRESULT hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev);
    if (FAILED(hr)) { printf("skip CreateDevice %#lx\n", hr); return 0; }
    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    IDirect3DSurface9 *ds0 = NULL;
    hr = IDirect3DDevice9_GetDepthStencilSurface(dev, &ds0);
    printf("no auto depth: GetDepthStencilSurface %#lx %d\n", hr, ds0 != NULL);

    /* rectangle lists */
    hr = clr(0, NULL, D3DCLEAR_TARGET, 0xff0000, 1, 0);
    printf("count 0 NULL: %#lx %06lx\n", hr, px(0, 0, 0));
    hr = clr(1, NULL, D3DCLEAR_TARGET, 0x00ff00, 1, 0);
    printf("count 1 NULL: %#lx %06lx\n", hr, px(0, 0, 0));
    D3DRECT r = { 4, 4, 8, 8 };
    hr = clr(0, &r, D3DCLEAR_TARGET, 0x0000ff, 1, 0);
    printf("count 0 list: %#lx %06lx\n", hr, px(0, 0, 0));
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    hr = clr(1, &r, D3DCLEAR_TARGET, 0xffffff, 1, 0);
    printf("count 1 list: %#lx %06lx %06lx %06lx %06lx\n", hr, px(0, 3, 3), px(0, 4, 4), px(0, 7, 7), px(0, 8, 8));
    D3DRECT inv = { 8, 8, 4, 4 }, out = { -4, -4, 2, 2 }, big = { 12, 12, 40, 40 }, two[2] = { { 0, 0, 2, 2 }, { 14, 0, 16, 2 } };
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    hr = clr(1, &inv, D3DCLEAR_TARGET, 0xffffff, 1, 0);
    printf("inverted: %#lx %06lx %06lx\n", hr, px(0, 5, 5), px(0, 8, 8));
    hr = clr(1, &out, D3DCLEAR_TARGET, 0x112233, 1, 0);
    printf("outside: %#lx %06lx %06lx\n", hr, px(0, 0, 0), px(0, 2, 2));
    hr = clr(1, &big, D3DCLEAR_TARGET, 0x445566, 1, 0);
    printf("past the end: %#lx %06lx %06lx\n", hr, px(0, 15, 15), px(0, 11, 11));
    hr = clr(2, two, D3DCLEAR_TARGET, 0x778899, 1, 0);
    printf("two: %#lx %06lx %06lx %06lx\n", hr, px(0, 1, 1), px(0, 15, 1), px(0, 8, 1));

    /* flags */
    printf("flags 0: %#lx\n", clr(0, NULL, 0, 0, 1, 0));
    printf("flags 0x10: %#lx\n", clr(0, NULL, 0x10, 0, 1, 0));
    hr = clr(0, NULL, D3DCLEAR_TARGET | 0x10, 0x0a0b0c, 1, 0);
    printf("flags target|0x10: %#lx %06lx\n", hr, px(0, 0, 0));

    /* the viewport and the scissor rectangle */
    D3DVIEWPORT9 vp = { 2, 2, 8, 8, 0, 1 }, full = { 0, 0, W, H, 0, 1 };
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    IDirect3DDevice9_SetViewport(dev, &vp);
    hr = clr(0, NULL, D3DCLEAR_TARGET, 0xff8000, 1, 0);
    printf("viewport: %#lx %06lx %06lx %06lx %06lx\n", hr, px(0, 1, 1), px(0, 2, 2), px(0, 9, 9), px(0, 10, 10));
    D3DRECT r2 = { 0, 0, 4, 4 };
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    hr = clr(1, &r2, D3DCLEAR_TARGET, 0x00ff80, 1, 0);
    printf("viewport rect: %#lx %06lx %06lx %06lx\n", hr, px(0, 1, 1), px(0, 2, 2), px(0, 3, 3));
    IDirect3DDevice9_SetViewport(dev, &full);
    RECT sc = { 0, 0, 4, 4 };
    IDirect3DDevice9_SetScissorRect(dev, &sc);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, TRUE);
    IDirect3DDevice9_SetViewport(dev, &full);
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    hr = clr(0, NULL, D3DCLEAR_TARGET, 0x8000ff, 1, 0);
    printf("scissor: %#lx %06lx %06lx\n", hr, px(0, 3, 3), px(0, 4, 4));
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SCISSORTESTENABLE, FALSE);
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    hr = clr(0, NULL, D3DCLEAR_TARGET, 0x8000ff, 1, 0);
    printf("scissor off: %#lx %06lx\n", hr, px(0, 4, 4));

    /* depth and stencil without a depth buffer */
    clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    printf("no depth: z %#lx stencil %#lx\n", clr(0, NULL, D3DCLEAR_ZBUFFER, 0, 1, 0), clr(0, NULL, D3DCLEAR_STENCIL, 0, 1, 0));
    hr = clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0x123456, 1, 0);
    printf("no depth: target|z %#lx %06lx\n", hr, px(0, 0, 0));

    /* depth buffers: D24S8, D24X8 (no stencil), D16 smaller, D24S8 larger */
    static const struct { D3DFORMAT f; int w, h; const char *n; } dz[4] = {
        { D3DFMT_D24S8, W, H, "D24S8" }, { D3DFMT_D24X8, W, H, "D24X8" }, { D3DFMT_D16, W / 2, H / 2, "D16 smaller" }, { D3DFMT_D24S8, W * 2, H * 2, "D24S8 larger" } };
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZFUNC, D3DCMP_LESSEQUAL);
    for (int i = 0; i < 4; i++) {
        IDirect3DSurface9 *z = NULL;
        hr = IDirect3DDevice9_CreateDepthStencilSurface(dev, dz[i].w, dz[i].h, dz[i].f, D3DMULTISAMPLE_NONE, 0, FALSE, &z, NULL);
        if (FAILED(hr)) { printf("%s: create %#lx\n", dz[i].n, hr); continue; }
        printf("%s: set %#lx\n", dz[i].n, IDirect3DDevice9_SetDepthStencilSurface(dev, z));
        printf("%s: z %#lx stencil %#lx z|stencil %#lx\n", dz[i].n, clr(0, NULL, D3DCLEAR_ZBUFFER, 0, 1, 0),
               clr(0, NULL, D3DCLEAR_STENCIL, 0, 1, 0), clr(0, NULL, D3DCLEAR_ZBUFFER | D3DCLEAR_STENCIL, 0, 1, 0));
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0x203040, 0.25f, 0);
        printf("%s: target|z %#lx %06lx\n", dz[i].n, hr, px(0, 0, 0));
        hr = clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL, 0x405060, 1, 0);
        printf("%s: target|stencil %#lx %06lx\n", dz[i].n, hr, px(0, 0, 0));
        /* what the depth is now: a quad at 0.5 draws where it is >= 0.5 */
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(0, NULL, D3DCLEAR_ZBUFFER, 0, 0.25f, 0);
        quad(0.5f, 0xff0000);
        printf("%s: z 0.25 %#lx, quad at 0.5 %06lx\n", dz[i].n, hr, px(0, 1, 1));
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(1, &r, D3DCLEAR_ZBUFFER, 0, 1, 0);
        quad(0.5f, 0x00ff00);
        printf("%s: z 1 in a rect %#lx, quad %06lx %06lx\n", dz[i].n, hr, px(0, 1, 1), px(0, 5, 5));
        /* depth outside 0..1 */
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(0, NULL, D3DCLEAR_ZBUFFER, 0, 2.0f, 0);
        quad(0.5f, 0x0000ff);
        printf("%s: z 2 %#lx, quad %06lx %s\n", dz[i].n, hr, px(0, 1, 1), drawn());
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(1, &r, D3DCLEAR_ZBUFFER, 0, 2.0f, 0);
        quad(0.5f, 0x00ff00);
        printf("%s: z 2 in a rect %#lx, quad %06lx %s\n", dz[i].n, hr, px(0, 5, 5), drawn());
        /* ZWRITEENABLE without ZENABLE */
        clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0, 1, 0);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_FALSE);
        quad(0.25f, 0x111111);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_TRUE);
        quad(0.5f, 0x22ff22);
        printf("%s: zwrite with z off, then quad at 0.5 %06lx %s\n", dz[i].n, px(0, 1, 1), drawn());
        clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
        hr = clr(0, NULL, D3DCLEAR_ZBUFFER, 0, -1.0f, 0);
        quad(0.0f, 0x0000ff);
        printf("%s: z -1 %#lx, quad at 0 %06lx %s\n", dz[i].n, hr, px(0, 1, 1), drawn());
        /* stencil: equal to 5 */
        if (dz[i].f == D3DFMT_D24S8) {
            clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0, 1, 0);
            hr = clr(0, NULL, D3DCLEAR_STENCIL, 0, 1, 0x105);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILENABLE, TRUE);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 5);
            quad(0.5f, 0xffff00);
            DWORD a = px(0, 1, 1);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 4);
            clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
            quad(0.5f, 0xffff00);
            printf("%s: stencil 0x105 %#lx, ref 5 %06lx ref 4 %06lx\n", dz[i].n, hr, a, px(0, 1, 1));
            /* a stencil clear in a rectangle */
            clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL, 0, 1, 0);
            hr = clr(1, &r, D3DCLEAR_STENCIL, 0, 1, 9);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 9);
            quad(0.5f, 0x00ffff);
            printf("%s: stencil 9 in a rect %#lx, ref 9 %06lx %06lx\n", dz[i].n, hr, px(0, 1, 1), px(0, 5, 5));
            /* operations: two increments, then equal 2 */
            clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL, 0, 1, 0);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_INCR);
            quad(0.5f, 0x101010);
            quad(0.5f, 0x101010);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 2);
            quad(0.5f, 0xff00ff);
            printf("%s: incr twice, equal 2 %06lx\n", dz[i].n, px(0, 1, 1));
            /* depth fail: z buffer at 0.25, a quad at 0.5 fails depth and replaces */
            clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL | D3DCLEAR_ZBUFFER, 0, 0.25f, 0);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILZFAIL, D3DSTENCILOP_REPLACE);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 3);
            quad(0.5f, 0x202020);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
            quad(0.0f, 0x80ff80);
            printf("%s: z fail replace 3 %06lx\n", dz[i].n, px(0, 1, 1));
            /* write mask 0x0f with ref 0xff, then the read mask */
            clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL | D3DCLEAR_ZBUFFER, 0, 1, 0);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_REPLACE);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILWRITEMASK, 0x0f);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 0xff);
            quad(0.5f, 0x303030);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILWRITEMASK, 0xffffffff);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 0x0f);
            quad(0.5f, 0x4080c0);
            DWORD wm = px(0, 1, 1);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILMASK, 0x0f);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 0x1f);
            quad(0.5f, 0xc08040);
            printf("%s: write mask 0x0f %06lx, read mask 0x0f ref 0x1f %06lx\n", dz[i].n, wm, px(0, 1, 1));
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILMASK, 0xffffffff);
            /* two-sided: the quad is clockwise (CW states replace with 7, CCW ones zero) */
            for (int two = 0; two < 2; two++) {
                clr(0, NULL, D3DCLEAR_TARGET | D3DCLEAR_STENCIL, 0, 1, 1);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_TWOSIDEDSTENCILMODE, two);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_REPLACE);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_CCW_STENCILFUNC, D3DCMP_ALWAYS);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_CCW_STENCILPASS, D3DSTENCILOP_ZERO);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 7);
                quad(0.5f, 0x505050);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_TWOSIDEDSTENCILMODE, FALSE);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILFUNC, D3DCMP_EQUAL);
                quad(0.5f, 0x60a0e0);
                DWORD e7 = px(0, 1, 1);
                IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILREF, 0);
                quad(0.5f, 0xe0a060);
                printf("%s: two-sided %d: equal 7 %06lx then equal 0 %06lx\n", dz[i].n, two, e7, px(0, 1, 1));
            }
            IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_CCW);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_STENCILENABLE, FALSE);
        }
        IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
        IDirect3DSurface9_Release(z);
    }
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, D3DZB_FALSE);

    /* every render target */
    IDirect3DTexture9 *t1 = NULL;
    hr = IDirect3DDevice9_CreateTexture(dev, W, H, 1, D3DUSAGE_RENDERTARGET, D3DFMT_X8R8G8B8, D3DPOOL_DEFAULT, &t1, NULL);
    if (SUCCEEDED(hr)) {
        IDirect3DSurface9 *s1;
        IDirect3DTexture9_GetSurfaceLevel(t1, 0, &s1);
        printf("rt1: set %#lx\n", IDirect3DDevice9_SetRenderTarget(dev, 1, s1));
        hr = clr(0, NULL, D3DCLEAR_TARGET, 0x5a6b7c, 1, 0);
        printf("rt1: clear %#lx %06lx %06lx\n", hr, px(0, 0, 0), px(s1, 0, 0));
        hr = clr(1, &r, D3DCLEAR_TARGET, 0x010203, 1, 0);
        printf("rt1: rect %#lx %06lx %06lx\n", hr, px(s1, 5, 5), px(s1, 0, 0));
        /* a pixel shader writing oC0 and oC1, then render target 1's write mask */
        static const DWORD ps2[] = { 0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0x02000001, 0x800f0801, 0xa0e40001, 0x0000ffff };
        IDirect3DPixelShader9 *ps = NULL;
        hr = IDirect3DDevice9_CreatePixelShader(dev, ps2, &ps);
        if (SUCCEEDED(hr)) {
            static const float k[8] = { 1, 0.5f, 0, 1, 0, 0.25f, 1, 1 };
            IDirect3DDevice9_SetPixelShader(dev, ps);
            IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, k, 2);
            clr(0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
            quad(0.5f, 0);
            DWORD a0 = px(0, 1, 1), a1 = px(s1, 1, 1);
            IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE1, D3DCOLORWRITEENABLE_RED);
            clr(0, NULL, D3DCLEAR_TARGET, 0x808080, 1, 0);
            quad(0.5f, 0);
            printf("rt1: oC0 oC1 %06lx %06lx, red only %06lx %06lx\n", a0, a1, px(0, 1, 1), px(s1, 1, 1));
            IDirect3DDevice9_SetRenderState(dev, D3DRS_COLORWRITEENABLE1, 0xf);
            IDirect3DDevice9_SetPixelShader(dev, NULL);
            IDirect3DPixelShader9_Release(ps);
        } else {
            printf("rt1: CreatePixelShader %#lx\n", hr);
        }
        IDirect3DDevice9_SetRenderTarget(dev, 1, NULL);
        hr = clr(0, NULL, D3DCLEAR_TARGET, 0x0f0f0f, 1, 0);
        printf("rt1 unset: clear %#lx %06lx\n", hr, px(s1, 0, 0));
        /* a target of its own as render target 0: the viewport resets to it */
        printf("rt0 texture: set %#lx\n", IDirect3DDevice9_SetRenderTarget(dev, 0, s1));
        IDirect3DDevice9_GetViewport(dev, &vp);
        printf("rt0 texture: viewport %lu %lu %lu %lu\n", vp.X, vp.Y, vp.Width, vp.Height);
        hr = clr(0, NULL, D3DCLEAR_TARGET, 0xabcdef, 1, 0);
        printf("rt0 texture: clear %#lx %06lx %06lx\n", hr, px(s1, 0, 0), px(0, 0, 0));
        IDirect3DDevice9_SetRenderTarget(dev, 0, back);
        IDirect3DSurface9_Release(s1);
        IDirect3DTexture9_Release(t1);
    } else {
        printf("rt1: create %#lx\n", hr);
    }
    IDirect3DSurface9_Release(sys);
    IDirect3DSurface9_Release(back);
    printf("Release %lu\n", IDirect3DDevice9_Release(dev));
    IDirect3D9_Release(d3d);
    return 0;
}
