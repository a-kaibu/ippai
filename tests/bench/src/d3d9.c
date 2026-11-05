/* Direct3D 9: the API most 2000s-2010s VN engines draw with (SiglusEngine, BGI, CatSystem2,
 * Artemis, ...), at 1280x720 with pretransformed (XYZRHW) quads as 2D engines draw. Each case
 * reports ms per frame.
 *
 *   vn               background, 3 characters, message window (fixed function, alpha blending),
 *                    and the typing text: drawn by GDI into a DIB and copied into a texture
 *                    (LockRect) every frame, as engines render their glyphs
 *   fade             crossfade of two full-screen pictures with a ps_2_0 pixel shader (lrp)
 *   sprites_batched  2000 particles in one dynamic vertex buffer (Lock DISCARD), one draw
 *   sprites_calls    2000 particles, one DrawPrimitiveUP each (engines that never batch)
 *   rt               the scene into a render target texture, then 4 blur passes ping-ponging
 *                    between two render targets, then to the back buffer (transitions, effects)
 *   upload           a 1280x720 dynamic texture rewritten every frame (LockRect DISCARD: movies)
 *   state            500 draws with texture, blend, sampler and stage state changes between them
 *
 * Build: i686-w64-mingw32-gcc -O2 d3d9.c -o d3d9.exe -mwindows -ld3d9 -ld3dx9_43 -lgdi32 */
#define COBJMACROS
#include "bench.h"
#include <d3d9.h>
#include <d3dx9.h>

typedef struct { float x, y, z, rhw; DWORD c; float u, v; } V;
#define FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1)
#define SPRITES 2000

static IDirect3DDevice9 *dev;
static IDirect3DTexture9 *t_bg, *t_bg2, *t_chr[3], *t_win, *t_text, *t_spr, *t_movie, *rt[2];
static IDirect3DSurface9 *backbuf;
static IDirect3DVertexBuffer9 *vb;
static IDirect3DIndexBuffer9 *ib;
static IDirect3DPixelShader9 *ps_fade, *ps_blur;
static HDC text_dc;
static unsigned *text_bits;
static HFONT font;

static IDirect3DTexture9 *texture(int w, int h, const unsigned *pixels, DWORD usage, D3DPOOL pool)
{
    IDirect3DTexture9 *t;
    D3DLOCKED_RECT lr;
    int y;
    BCHECK(IDirect3DDevice9_CreateTexture(dev, w, h, 1, usage, D3DFMT_A8R8G8B8, pool, &t, NULL));
    if (pixels)
    {
        BCHECK(IDirect3DTexture9_LockRect(t, 0, &lr, NULL, 0));
        for (y = 0; y < h; y++) memcpy((BYTE *)lr.pBits + y * lr.Pitch, pixels + y * w, w * 4);
        IDirect3DTexture9_UnlockRect(t, 0);
    }
    return t;
}

static IDirect3DTexture9 *picture(int w, int h, void (*make)(unsigned *, int, int, int), int arg)
{
    unsigned *p = pic_alloc(w, h);
    IDirect3DTexture9 *t;
    make(p, w, h, arg);
    t = texture(w, h, p, 0, D3DPOOL_MANAGED);
    free(p);
    return t;
}

static void char_pic(unsigned *p, int w, int h, int hue) { pic_character(p, w, h, hue, 0); }
static void win_pic(unsigned *p, int w, int h, int x) { (void)x, pic_window(p, w, h, 0); }
static void spr_pic(unsigned *p, int w, int h, int x) { (void)x, pic_particle(p, w, h, 0); }

static void quad(V *v, float x, float y, float w, float h, DWORD c)
{
    V q[4] = { { x - 0.5f, y - 0.5f, 0, 1, c, 0, 0 }, { x + w - 0.5f, y - 0.5f, 0, 1, c, 1, 0 },
               { x - 0.5f, y + h - 0.5f, 0, 1, c, 0, 1 }, { x + w - 0.5f, y + h - 0.5f, 0, 1, c, 1, 1 } };
    memcpy(v, q, sizeof(q));
}

static void draw(IDirect3DTexture9 *t, float x, float y, float w, float h, DWORD c)
{
    V v[4];
    quad(v, x, y, w, h, c);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(V));
}

static void states_2d(void)
{
    IDirect3DDevice9_SetFVF(dev, FVF);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
}

static void scene(int f)
{
    int i;
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    for (i = 0; i < 3; i++) draw(t_chr[i], 40 + i * 410, 80, 400, 640, 0xffffffff);
    draw(t_win, 40, 500, 1200, 200, 0xffffffff);
    (void)f;
}

static void text_update(int f)
{
    /* the line typed so far, by GDI into a DIB, then into the text texture */
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), y;
    D3DLOCKED_RECT lr;
    memset(text_bits, 0, 1024 * 64 * 4);
    ExtTextOutW(text_dc, 0, 16, 0, NULL, line, n < len ? n : len, NULL);
    GdiFlush();
    if (SUCCEEDED(IDirect3DTexture9_LockRect(t_text, 0, &lr, NULL, 0)))
    {
        for (y = 0; y < 64; y++)
        {
            unsigned *d = (unsigned *)((BYTE *)lr.pBits + y * lr.Pitch), *s = text_bits + y * 1024;
            int x;
            for (x = 0; x < 1024; x++) d[x] = (s[x] & 0xff) << 24 | 0xffffff; /* (coverage as alpha) */
        }
        IDirect3DTexture9_UnlockRect(t_text, 0);
    }
}

static void begin(DWORD clear)
{
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, clear, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
}

static void end(void)
{
    IDirect3DDevice9_EndScene(dev);
    IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL);
}

static void frame_vn(int f)
{
    text_update(f);
    begin(0);
    scene(f);
    draw(t_text, 80, 530, 1024, 64, 0xffffffff);
    end();
}

static void frame_fade(int f)
{
    float k[4] = { (f % 120) / 119.0f, 0, 0, 0 };
    begin(0);
    IDirect3DDevice9_SetPixelShader(dev, ps_fade);
    IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, k, 1);
    IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)t_bg2);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DDevice9_SetTexture(dev, 1, NULL);
    end();
}

static void sprite_quad(V *v, int i, int f)
{
    float x = (float)((i * 211 + f * (1 + i % 5)) % (SCREEN_W - 32)), y = (float)((i * 97 + f * (1 + i % 3)) % (SCREEN_H - 32));
    quad(v, x, y, 32, 32, 0xffffffff - (i % 64) * 0x01000000u);
}

static void frame_sprites_batched(int f)
{
    V *v;
    int i;
    begin(0);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    if (SUCCEEDED(IDirect3DVertexBuffer9_Lock(vb, 0, 0, (void **)&v, D3DLOCK_DISCARD)))
    {
        for (i = 0; i < SPRITES; i++) sprite_quad(v + i * 4, i, f);
        IDirect3DVertexBuffer9_Unlock(vb);
    }
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t_spr);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb, 0, sizeof(V));
    IDirect3DDevice9_SetIndices(dev, ib);
    IDirect3DDevice9_DrawIndexedPrimitive(dev, D3DPT_TRIANGLELIST, 0, 0, SPRITES * 4, 0, SPRITES * 2);
    end();
}

static void frame_sprites_calls(int f)
{
    V v[4];
    int i;
    begin(0);
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t_spr);
    for (i = 0; i < SPRITES; i++)
    {
        sprite_quad(v, i, f);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(V));
    }
    end();
}

static void target(int k)
{
    IDirect3DSurface9 *s;
    IDirect3DTexture9_GetSurfaceLevel(rt[k], 0, &s);
    IDirect3DDevice9_SetRenderTarget(dev, 0, s);
    IDirect3DSurface9_Release(s);
}

static void frame_rt(int f)
{
    float off[4][4] = { { 1.0f / SCREEN_W, 0, 0, 0 }, { 0, 1.0f / SCREEN_H, 0, 0 }, { 2.0f / SCREEN_W, 0, 0, 0 }, { 0, 2.0f / SCREEN_H, 0, 0 } };
    int p;
    target(0);
    begin(0);
    scene(f);
    IDirect3DDevice9_SetPixelShader(dev, ps_blur);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, FALSE);
    for (p = 0; p < 4; p++)
    {
        target((p + 1) & 1);
        IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, off[p], 1);
        draw(rt[p & 1], 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    }
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DDevice9_SetRenderTarget(dev, 0, backbuf);
    draw(rt[0], 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    end();
}

static unsigned *movie;

static void frame_upload(int f)
{
    D3DLOCKED_RECT lr;
    int y;
    pic_movie(movie, SCREEN_W, SCREEN_H, f);
    if (SUCCEEDED(IDirect3DTexture9_LockRect(t_movie, 0, &lr, NULL, D3DLOCK_DISCARD)))
    {
        for (y = 0; y < SCREEN_H; y++) memcpy((BYTE *)lr.pBits + y * lr.Pitch, movie + y * SCREEN_W, SCREEN_W * 4);
        IDirect3DTexture9_UnlockRect(t_movie, 0);
    }
    begin(0);
    draw(t_movie, 0, 0, SCREEN_W, SCREEN_H, 0xffffffff);
    end();
}

static void frame_state(int f)
{
    IDirect3DTexture9 *ts[4] = { t_bg, t_chr[0], t_spr, t_win };
    int i;
    begin(0);
    for (i = 0; i < 500; i++)
    {
        IDirect3DDevice9_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, i % 3 != 0);
        IDirect3DDevice9_SetRenderState(dev, D3DRS_DESTBLEND, i % 2 ? D3DBLEND_INVSRCALPHA : D3DBLEND_ONE);
        IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, i % 4 ? D3DTEXF_LINEAR : D3DTEXF_POINT);
        IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, i % 5 ? D3DTOP_MODULATE : D3DTOP_SELECTARG1);
        draw(ts[(i + f) % 4], (float)(i * 37 % (SCREEN_W - 64)), (float)(i * 23 % (SCREEN_H - 64)), 64, 64, 0xffffffff);
    }
    states_2d();
    end();
}

static IDirect3DPixelShader9 *pixel_shader(const char *src)
{
    ID3DXBuffer *code, *errors = NULL;
    IDirect3DPixelShader9 *ps;
    if (FAILED(D3DXAssembleShader(src, strlen(src), NULL, NULL, 0, &code, &errors)))
        bench_fail(errors ? (const char *)ID3DXBuffer_GetBufferPointer(errors) : "D3DXAssembleShader");
    BCHECK(IDirect3DDevice9_CreatePixelShader(dev, ID3DXBuffer_GetBufferPointer(code), &ps));
    ID3DXBuffer_Release(code);
    return ps;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    D3DPRESENT_PARAMETERS pp = { 0 };
    IDirect3D9 *d3d;
    HWND hwnd;
    WORD *idx;
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("d3d9bench", SCREEN_W, SCREEN_H, NULL);
    d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) bench_fail("Direct3DCreate9");
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferWidth = SCREEN_W;
    pp.BackBufferHeight = SCREEN_H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.hDeviceWindow = hwnd;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    BCHECK(IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev));
    IDirect3DDevice9_GetRenderTarget(dev, 0, &backbuf);
    states_2d();

    t_bg = picture(SCREEN_W, SCREEN_H, pic_background, 1);
    t_bg2 = picture(SCREEN_W, SCREEN_H, pic_background, 2);
    for (i = 0; i < 3; i++) t_chr[i] = picture(400, 640, char_pic, i + 1);
    t_win = picture(1200, 200, win_pic, 0);
    t_spr = picture(32, 32, spr_pic, 0);
    t_text = texture(1024, 64, NULL, 0, D3DPOOL_MANAGED);
    {
        BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 1024, -64, 1, 32, BI_RGB } };
        text_dc = CreateCompatibleDC(NULL);
        SelectObject(text_dc, CreateDIBSection(text_dc, &bi, DIB_RGB_COLORS, (void **)&text_bits, NULL, 0));
        font = CreateFontW(-32, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic");
        SelectObject(text_dc, font);
        SetBkMode(text_dc, TRANSPARENT);
        SetTextColor(text_dc, RGB(255, 255, 255));
    }

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("fade"))
    {
        ps_fade = pixel_shader("ps_2_0\n dcl t0.xy\n dcl_2d s0\n dcl_2d s1\n texld r0, t0, s0\n texld r1, t0, s1\n"
                               " lrp r2, c0.x, r1, r0\n mov oC0, r2\n");
        bench_frames("fade", 20, 240, frame_fade);
    }
    if (bench_is("sprites_batched") || bench_is("sprites_calls"))
    {
        BCHECK(IDirect3DDevice9_CreateVertexBuffer(dev, SPRITES * 4 * sizeof(V), D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY, FVF, D3DPOOL_DEFAULT, &vb, NULL));
        BCHECK(IDirect3DDevice9_CreateIndexBuffer(dev, SPRITES * 6 * 2, D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_MANAGED, &ib, NULL));
        BCHECK(IDirect3DIndexBuffer9_Lock(ib, 0, 0, (void **)&idx, 0));
        for (i = 0; i < SPRITES; i++)
        {
            WORD b = (WORD)(i * 4), q[6] = { b, b + 1, b + 2, b + 2, b + 1, b + 3 };
            memcpy(idx + i * 6, q, sizeof(q));
        }
        IDirect3DIndexBuffer9_Unlock(ib);
        if (bench_is("sprites_batched")) bench_frames("sprites_batched", 20, 240, frame_sprites_batched);
        if (bench_is("sprites_calls")) bench_frames("sprites_calls", 10, 120, frame_sprites_calls);
    }
    if (bench_is("rt"))
    {
        for (i = 0; i < 2; i++) rt[i] = texture(SCREEN_W, SCREEN_H, NULL, D3DUSAGE_RENDERTARGET, D3DPOOL_DEFAULT);
        ps_blur = pixel_shader("ps_2_0\n dcl t0.xy\n dcl_2d s0\n def c1, 0.25, 0, 0, 0\n"
                               " add r0.xy, t0, c0\n sub r1.xy, t0, c0\n add r2.xy, t0, c0.yxzw\n sub r3.xy, t0, c0.yxzw\n"
                               " texld r0, r0, s0\n texld r1, r1, s0\n texld r2, r2, s0\n texld r3, r3, s0\n"
                               " add r0, r0, r1\n add r0, r0, r2\n add r0, r0, r3\n mul r0, r0, c1.x\n mov oC0, r0\n");
        bench_frames("rt", 20, 240, frame_rt);
    }
    if (bench_is("upload"))
    {
        movie = pic_alloc(SCREEN_W, SCREEN_H);
        t_movie = texture(SCREEN_W, SCREEN_H, NULL, D3DUSAGE_DYNAMIC, D3DPOOL_DEFAULT);
        bench_frames("upload", 20, 240, frame_upload);
    }
    if (bench_is("state")) bench_frames("state", 10, 120, frame_state);
    bench_end();
    return 0;
}
