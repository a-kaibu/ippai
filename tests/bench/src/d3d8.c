/* Direct3D 8: older 3D-accelerated 2D engines, at 1280x720 with pretransformed quads. Each case
 * reports ms per frame.
 *
 *   vn       background, 3 characters, message window, the typing text (GDI into a DIB, copied
 *            into a texture every frame)
 *   sprites  2000 particles, one DrawPrimitiveUP each
 *   upload   a 1280x720 dynamic texture rewritten every frame (movies)
 *
 * Build: i686-w64-mingw32-gcc -O2 d3d8.c -o d3d8.exe -mwindows -ld3d8 -lgdi32 */
#define COBJMACROS
#include "bench.h"
#include <d3d8.h>

typedef struct { float x, y, z, rhw; DWORD c; float u, v; } V;
#define FVF (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1)
#define SPRITES 2000

static IDirect3DDevice8 *dev;
static IDirect3DTexture8 *t_bg, *t_chr[3], *t_win, *t_text, *t_spr, *t_movie;
static HDC text_dc;
static unsigned *text_bits, *movie;

static IDirect3DTexture8 *texture(int w, int h, const unsigned *pixels, DWORD usage, D3DPOOL pool)
{
    IDirect3DTexture8 *t;
    D3DLOCKED_RECT lr;
    int y;
    BCHECK(IDirect3DDevice8_CreateTexture(dev, w, h, 1, usage, D3DFMT_A8R8G8B8, pool, &t));
    if (pixels)
    {
        BCHECK(IDirect3DTexture8_LockRect(t, 0, &lr, NULL, 0));
        for (y = 0; y < h; y++) memcpy((BYTE *)lr.pBits + y * lr.Pitch, pixels + y * w, w * 4);
        IDirect3DTexture8_UnlockRect(t, 0);
    }
    return t;
}

static void quad(V *v, float x, float y, float w, float h, DWORD c)
{
    V q[4] = { { x - 0.5f, y - 0.5f, 0, 1, c, 0, 0 }, { x + w - 0.5f, y - 0.5f, 0, 1, c, 1, 0 },
               { x - 0.5f, y + h - 0.5f, 0, 1, c, 0, 1 }, { x + w - 0.5f, y + h - 0.5f, 0, 1, c, 1, 1 } };
    memcpy(v, q, sizeof(q));
}

static void draw(IDirect3DTexture8 *t, float x, float y, float w, float h)
{
    V v[4];
    quad(v, x, y, w, h, 0xffffffff);
    IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)t);
    IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(V));
}

static void begin(void)
{
    IDirect3DDevice8_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0, 1, 0);
    IDirect3DDevice8_BeginScene(dev);
}

/* when asked: the back buffer copied to system memory (CopyRects) and read */
static void read_back(void)
{
    static IDirect3DSurface8 *bb, *sys;
    D3DLOCKED_RECT lr;
    if (!sys)
    {
        BCHECK(IDirect3DDevice8_GetBackBuffer(dev, 0, D3DBACKBUFFER_TYPE_MONO, &bb));
        BCHECK(IDirect3DDevice8_CreateImageSurface(dev, SCREEN_W, SCREEN_H, D3DFMT_X8R8G8B8, &sys));
    }
    BCHECK(IDirect3DDevice8_CopyRects(dev, bb, NULL, 0, sys, NULL));
    BCHECK(IDirect3DSurface8_LockRect(sys, &lr, NULL, D3DLOCK_READONLY));
    bench_frame_pixels(lr.pBits, SCREEN_W, SCREEN_H, lr.Pitch);
    IDirect3DSurface8_UnlockRect(sys);
}

static void end(void)
{
    IDirect3DDevice8_EndScene(dev);
    if (bench_capturing()) read_back();
    IDirect3DDevice8_Present(dev, NULL, NULL, NULL, NULL);
}

static void frame_vn(int f)
{
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), i, y;
    D3DLOCKED_RECT lr;
    memset(text_bits, 0, 1024 * 64 * 4);
    ExtTextOutW(text_dc, 0, 16, 0, NULL, line, n < len ? n : len, NULL);
    GdiFlush();
    if (SUCCEEDED(IDirect3DTexture8_LockRect(t_text, 0, &lr, NULL, 0)))
    {
        for (y = 0; y < 64; y++)
        {
            unsigned *d = (unsigned *)((BYTE *)lr.pBits + y * lr.Pitch), *s = text_bits + y * 1024;
            int x;
            for (x = 0; x < 1024; x++) d[x] = (s[x] & 0xff) << 24 | 0xffffff;
        }
        IDirect3DTexture8_UnlockRect(t_text, 0);
    }
    begin();
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 3; i++) draw(t_chr[i], 40 + i * 410, 80, 400, 640);
    draw(t_win, 40, 500, 1200, 200);
    draw(t_text, 80, 530, 1024, 64);
    end();
}

static void frame_sprites(int f)
{
    V v[4];
    int i;
    begin();
    draw(t_bg, 0, 0, SCREEN_W, SCREEN_H);
    IDirect3DDevice8_SetTexture(dev, 0, (IDirect3DBaseTexture8 *)t_spr);
    for (i = 0; i < SPRITES; i++)
    {
        quad(v, (float)((i * 211 + f * (1 + i % 5)) % (SCREEN_W - 32)), (float)((i * 97 + f * (1 + i % 3)) % (SCREEN_H - 32)), 32, 32,
             0xffffffff - (i % 64) * 0x01000000u);
        IDirect3DDevice8_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(V));
    }
    end();
}

static void frame_upload(int f)
{
    D3DLOCKED_RECT lr;
    int y;
    pic_movie(movie, SCREEN_W, SCREEN_H, f);
    if (SUCCEEDED(IDirect3DTexture8_LockRect(t_movie, 0, &lr, NULL, D3DLOCK_DISCARD)))
    {
        for (y = 0; y < SCREEN_H; y++) memcpy((BYTE *)lr.pBits + y * lr.Pitch, movie + y * SCREEN_W, SCREEN_W * 4);
        IDirect3DTexture8_UnlockRect(t_movie, 0);
    }
    begin();
    draw(t_movie, 0, 0, SCREEN_W, SCREEN_H);
    end();
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    D3DPRESENT_PARAMETERS pp = { 0 };
    IDirect3D8 *d3d;
    HWND hwnd;
    unsigned *p;
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("d3d8bench", SCREEN_W, SCREEN_H, NULL);
    d3d = Direct3DCreate8(D3D_SDK_VERSION);
    if (!d3d) bench_fail("Direct3DCreate8");
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferWidth = SCREEN_W;
    pp.BackBufferHeight = SCREEN_H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.hDeviceWindow = hwnd;
    BCHECK(IDirect3D8_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, D3DCREATE_SOFTWARE_VERTEXPROCESSING, &pp, &dev));
    IDirect3DDevice8_SetVertexShader(dev, FVF);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_ALPHABLENDENABLE, TRUE);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    IDirect3DDevice8_SetRenderState(dev, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_MODULATE);
    IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MINFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice8_SetTextureStageState(dev, 0, D3DTSS_MAGFILTER, D3DTEXF_LINEAR);

    p = pic_alloc(SCREEN_W, SCREEN_H);
    pic_background(p, SCREEN_W, SCREEN_H, 1);
    t_bg = texture(SCREEN_W, SCREEN_H, p, 0, D3DPOOL_MANAGED);
    for (i = 0; i < 3; i++)
    {
        pic_character(p, 400, 640, i + 1, 0);
        t_chr[i] = texture(400, 640, p, 0, D3DPOOL_MANAGED);
    }
    pic_window(p, 1200, 200, 0);
    t_win = texture(1200, 200, p, 0, D3DPOOL_MANAGED);
    pic_particle(p, 32, 32, 0);
    t_spr = texture(32, 32, p, 0, D3DPOOL_MANAGED);
    movie = p;
    t_text = texture(1024, 64, NULL, 0, D3DPOOL_MANAGED);
    {
        BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), 1024, -64, 1, 32, BI_RGB } };
        text_dc = CreateCompatibleDC(NULL);
        SelectObject(text_dc, CreateDIBSection(text_dc, &bi, DIB_RGB_COLORS, (void **)&text_bits, NULL, 0));
        SelectObject(text_dc, CreateFontW(-32, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic"));
        SetBkMode(text_dc, TRANSPARENT);
        SetTextColor(text_dc, RGB(255, 255, 255));
    }

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("sprites")) bench_frames("sprites", 10, 120, frame_sprites);
    if (bench_is("upload"))
    {
        t_movie = texture(SCREEN_W, SCREEN_H, NULL, D3DUSAGE_DYNAMIC, D3DPOOL_DEFAULT);
        bench_frames("upload", 20, 240, frame_upload);
    }
    bench_end();
    return 0;
}
