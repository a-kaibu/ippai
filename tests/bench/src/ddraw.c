/* DirectDraw (7): how late-90s / 2000s engines draw, at 1280x720 windowed (640x480 for the
 * exclusive-mode case). Each case reports ms per frame.
 *
 *   vn        background BltFast, characters with a source color key, the message window blended
 *             by the CPU under Lock, typing text through GetDC, then Blt to the primary (clipper)
 *   lock      a software renderer: Lock the whole back surface, write every pixel, Unlock, Blt
 *   colorkey  200 color-keyed 64x64 sprites Blt per frame (sprite engines)
 *   flip      exclusive 640x480x32 with a flip chain: BltFast of a background and 20 sprites, Flip
 *
 * Build: i686-w64-mingw32-gcc -O2 ddraw.c -o ddraw.exe -mwindows -lddraw -ldxguid -lgdi32 */
#include "bench.h"
#include <ddraw.h>

static HWND hwnd;
static IDirectDraw7 *dd;
static IDirectDrawSurface7 *primary, *back, *bg, *chr[3], *spr, *flipchain;
static unsigned *winpic;
static HFONT font;
static int W = SCREEN_W, H = SCREEN_H;

static IDirectDrawSurface7 *surface(int w, int h, const unsigned *pixels, int colorkey)
{
    DDSURFACEDESC2 sd = { sizeof(sd) };
    IDirectDrawSurface7 *s;
    int y;
    sd.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    sd.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    sd.dwWidth = w;
    sd.dwHeight = h;
    sd.ddpfPixelFormat.dwSize = sizeof(sd.ddpfPixelFormat);
    sd.ddpfPixelFormat.dwFlags = DDPF_RGB;
    sd.ddpfPixelFormat.dwRGBBitCount = 32;
    sd.ddpfPixelFormat.dwRBitMask = 0xff0000;
    sd.ddpfPixelFormat.dwGBitMask = 0x00ff00;
    sd.ddpfPixelFormat.dwBBitMask = 0x0000ff;
    BCHECK(IDirectDraw7_CreateSurface(dd, &sd, &s, NULL));
    if (pixels)
    {
        BCHECK(IDirectDrawSurface7_Lock(s, NULL, &sd, DDLOCK_WAIT | DDLOCK_WRITEONLY, 0));
        for (y = 0; y < h; y++)
        {
            unsigned *row = (unsigned *)((BYTE *)sd.lpSurface + y * sd.lPitch);
            int x;
            for (x = 0; x < w; x++)
            {
                unsigned p = pixels[y * w + x];
                /* (color key: magenta where transparent) */
                row[x] = colorkey && (p >> 24) < 128 ? 0xff00ff : p & 0xffffff;
            }
        }
        IDirectDrawSurface7_Unlock(s, NULL);
    }
    if (colorkey)
    {
        DDCOLORKEY ck = { 0xff00ff, 0xff00ff };
        BCHECK(IDirectDrawSurface7_SetColorKey(s, DDCKEY_SRCBLT, &ck));
    }
    return s;
}

static void to_screen(void)
{
    RECT r;
    POINT p = { 0, 0 };
    GetClientRect(hwnd, &r);
    ClientToScreen(hwnd, &p);
    OffsetRect(&r, p.x, p.y);
    IDirectDrawSurface7_Blt(primary, &r, back, NULL, DDBLT_WAIT, NULL);
}

static void message(IDirectDrawSurface7 *s, int f)
{
    HDC dc;
    if (SUCCEEDED(IDirectDrawSurface7_GetDC(s, &dc)))
    {
        const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
        int n = f % 40 + 1, len = lstrlenW(line);
        SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(255, 255, 255));
        ExtTextOutW(dc, 80, 540, 0, NULL, line, n < len ? n : len, NULL);
        IDirectDrawSurface7_ReleaseDC(s, dc);
    }
}

static void frame_vn(int f)
{
    DDSURFACEDESC2 sd = { sizeof(sd) };
    int i, x, y;
    IDirectDrawSurface7_BltFast(back, 0, 0, bg, NULL, DDBLTFAST_WAIT | DDBLTFAST_NOCOLORKEY);
    for (i = 0; i < 3; i++) IDirectDrawSurface7_BltFast(back, 40 + i * 410, 80, chr[i], NULL, DDBLTFAST_WAIT | DDBLTFAST_SRCCOLORKEY);
    if (SUCCEEDED(IDirectDrawSurface7_Lock(back, NULL, &sd, DDLOCK_WAIT, 0)))
    {
        for (y = 0; y < 200; y++)
        {
            unsigned *d = (unsigned *)((BYTE *)sd.lpSurface + (500 + y) * sd.lPitch) + 40;
            const unsigned *s = winpic + y * 1200;
            for (x = 0; x < 1200; x++)
            {
                unsigned sp = s[x], a = sp >> 24, dp = d[x];
                d[x] = ((((sp & 0xff00ff) * a + (dp & 0xff00ff) * (255 - a)) >> 8) & 0xff00ff) |
                       ((((sp & 0xff00) * a + (dp & 0xff00) * (255 - a)) >> 8) & 0xff00);
            }
        }
        IDirectDrawSurface7_Unlock(back, NULL);
    }
    message(back, f);
    to_screen();
}

static void frame_lock(int f)
{
    DDSURFACEDESC2 sd = { sizeof(sd) };
    int x, y;
    if (SUCCEEDED(IDirectDrawSurface7_Lock(back, NULL, &sd, DDLOCK_WAIT | DDLOCK_WRITEONLY, 0)))
    {
        for (y = 0; y < H; y++)
        {
            unsigned *row = (unsigned *)((BYTE *)sd.lpSurface + y * sd.lPitch);
            for (x = 0; x < W; x++) row[x] = ((x + f * 4) & 255) << 16 | ((y + f) & 255) << 8 | ((x ^ y) & 255);
        }
        IDirectDrawSurface7_Unlock(back, NULL);
    }
    to_screen();
}

static void frame_colorkey(int f)
{
    int i;
    IDirectDrawSurface7_BltFast(back, 0, 0, bg, NULL, DDBLTFAST_WAIT | DDBLTFAST_NOCOLORKEY);
    for (i = 0; i < 200; i++)
    {
        RECT d;
        int x = (i * 211 + f * (1 + i % 5)) % (W - 64), y = (i * 97 + f * (1 + i % 3)) % (H - 64);
        SetRect(&d, x, y, x + 64, y + 64);
        IDirectDrawSurface7_Blt(back, &d, spr, NULL, DDBLT_WAIT | DDBLT_KEYSRC, NULL);
    }
    to_screen();
}

static void frame_flip(int f)
{
    int i;
    IDirectDrawSurface7_BltFast(flipchain, 0, 0, bg, NULL, DDBLTFAST_WAIT | DDBLTFAST_NOCOLORKEY);
    for (i = 0; i < 20; i++)
        IDirectDrawSurface7_BltFast(flipchain, (i * 97 + f * 3) % (640 - 64), (i * 53 + f * 2) % (480 - 64), spr, NULL,
                                    DDBLTFAST_WAIT | DDBLTFAST_SRCCOLORKEY);
    IDirectDrawSurface7_Flip(primary, NULL, DDFLIP_WAIT);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    DDSURFACEDESC2 sd = { sizeof(sd) };
    IDirectDrawClipper *clip;
    unsigned *pic;
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    font = CreateFontW(-26, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic");
    BCHECK(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL));
    if (!strcmp(bench_case, "flip"))
    {
        DDSCAPS2 caps = { DDSCAPS_BACKBUFFER };
        W = 640, H = 480;
        hwnd = CreateWindowA("STATIC", "ddrawbench", WS_POPUP | WS_VISIBLE, 0, 0, W, H, 0, 0, inst, 0);
        BCHECK(IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_EXCLUSIVE | DDSCL_FULLSCREEN));
        BCHECK(IDirectDraw7_SetDisplayMode(dd, W, H, 32, 0, 0));
        sd.dwFlags = DDSD_CAPS | DDSD_BACKBUFFERCOUNT;
        sd.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE | DDSCAPS_FLIP | DDSCAPS_COMPLEX;
        sd.dwBackBufferCount = 1;
        BCHECK(IDirectDraw7_CreateSurface(dd, &sd, &primary, NULL));
        BCHECK(IDirectDrawSurface7_GetAttachedSurface(primary, &caps, &flipchain));
    }
    else
    {
        hwnd = bench_window("ddrawbench", W, H, NULL);
        BCHECK(IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL));
        sd.dwFlags = DDSD_CAPS;
        sd.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
        BCHECK(IDirectDraw7_CreateSurface(dd, &sd, &primary, NULL));
        BCHECK(IDirectDraw7_CreateClipper(dd, 0, &clip, NULL));
        BCHECK(IDirectDrawClipper_SetHWnd(clip, 0, hwnd));
        BCHECK(IDirectDrawSurface7_SetClipper(primary, clip));
    }
    back = surface(W, H, NULL, 0);
    pic = pic_alloc(W, H);
    pic_background(pic, W, H, 1);
    bg = surface(W, H, pic, 0);
    free(pic);
    pic = pic_alloc(400, 640);
    for (i = 0; i < 3; i++)
    {
        pic_character(pic, 400, 640, i + 1, 0);
        chr[i] = surface(400, 640, pic, 1);
    }
    free(pic);
    pic = pic_alloc(64, 64);
    pic_particle(pic, 64, 64, 0);
    spr = surface(64, 64, pic, 1);
    free(pic);
    winpic = pic_alloc(1200, 200);
    pic_window(winpic, 1200, 200, 0);

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("lock")) bench_frames("lock", 10, 120, frame_lock);
    if (bench_is("colorkey")) bench_frames("colorkey", 10, 120, frame_colorkey);
    if (!strcmp(bench_case, "flip")) bench_frames("flip", 20, 240, frame_flip);
    bench_end();
    return 0;
}
