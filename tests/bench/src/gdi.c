/* GDI: how engines that compose in software (KiriKiri, NScripter, many older ones) put their
 * frames on the screen, at 1280x720. Each case reports ms per frame.
 *
 *   vn          static scene composed in a DIB section by the CPU, a typing message (ExtTextOutW
 *               on the DIB's DC), BitBlt of the whole frame to the window
 *   vn_dirty    the same scene, but only the message window's rectangle is blitted (engines
 *               that track dirty rectangles)
 *   alphablend  the scene with GDI doing the blending: AlphaBlend from premultiplied DIBs
 *   stretch     a 640x360 frame scaled to 1280x720 by StretchDIBits (old games in a big window)
 *   text        30 lines of antialiased text per frame on a memory DC, then BitBlt
 *   small       60 small (64x64) BitBlts per frame to the window: buttons, cursors, effects
 *
 * Build: i686-w64-mingw32-gcc -O2 gdi.c -o gdi.exe -mwindows -lgdi32 -lmsimg32 */
#include "bench.h"

static HWND hwnd;
static HDC wdc, mdc;
static unsigned *bits, *bg, *chars[3], *win;
static HFONT font;

static HDC dib_dc(int w, int h, unsigned **out)
{
    BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), w, -h, 1, 32, BI_RGB } };
    HDC dc = CreateCompatibleDC(wdc);
    HBITMAP bm = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)out, NULL, 0);
    if (!bm) bench_fail("CreateDIBSection");
    SelectObject(dc, bm);
    return dc;
}

static void blend(unsigned *dst, int dw, const unsigned *src, int w, int h, int x0, int y0)
{
    int x, y;
    for (y = 0; y < h; y++)
    {
        unsigned *d = dst + (y + y0) * dw + x0;
        const unsigned *s = src + y * w;
        for (x = 0; x < w; x++)
        {
            unsigned sp = s[x], a = sp >> 24, dp;
            if (!a) continue;
            if (a == 255) { d[x] = sp; continue; }
            dp = d[x];
            d[x] = 0xff000000u | ((((sp & 0xff00ff) * a + (dp & 0xff00ff) * (255 - a)) >> 8) & 0xff00ff) |
                   ((((sp & 0xff00) * a + (dp & 0xff00) * (255 - a)) >> 8) & 0xff00);
        }
    }
}

static unsigned *scene; /* the composed scene without the message text */

static void message(HDC dc, int frame, int x, int y)
{
    const WCHAR *line = bench_lines[(frame / 40) % BENCH_LINES];
    int n = frame % 40 + 1, len = lstrlenW(line);
    SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    ExtTextOutW(dc, x, y, 0, NULL, line, n < len ? n : len, NULL);
}

static void frame_vn(int f)
{
    /* the message window's area from the composed scene, then the text so far */
    int y;
    for (y = 500; y < 700; y++) memcpy(bits + y * SCREEN_W + 40, scene + y * SCREEN_W + 40, 1200 * 4);
    message(mdc, f, 80, 540);
    GdiFlush();
    BitBlt(wdc, 0, 0, SCREEN_W, SCREEN_H, mdc, 0, 0, SRCCOPY);
}

static void frame_vn_dirty(int f)
{
    int y;
    for (y = 500; y < 700; y++) memcpy(bits + y * SCREEN_W + 40, scene + y * SCREEN_W + 40, 1200 * 4);
    message(mdc, f, 80, 540);
    GdiFlush();
    BitBlt(wdc, 40, 500, 1200, 200, mdc, 40, 500, SRCCOPY);
}

static HDC bg_dc, char_dc[3], win_dc;

static void frame_alphablend(int f)
{
    BLENDFUNCTION bf = { AC_SRC_OVER, 0, 255, AC_SRC_ALPHA };
    int i;
    BitBlt(mdc, 0, 0, SCREEN_W, SCREEN_H, bg_dc, 0, 0, SRCCOPY);
    for (i = 0; i < 3; i++) AlphaBlend(mdc, 40 + i * 410, 80 - (f + i * 5) % 8, 400, 640, char_dc[i], 0, 0, 400, 640, bf);
    AlphaBlend(mdc, 40, 500, 1200, 200, win_dc, 0, 0, 1200, 200, bf);
    message(mdc, f, 80, 540);
    BitBlt(wdc, 0, 0, SCREEN_W, SCREEN_H, mdc, 0, 0, SRCCOPY);
}

static unsigned *small;
static BITMAPINFO small_bi = { { sizeof(BITMAPINFOHEADER), 640, -360, 1, 32, BI_RGB } };

static void frame_stretch(int f)
{
    int y;
    for (y = 0; y < 360; y += 2) memset(small + y * 640, f & 255, 640 * 4); /* (half the lines change) */
    StretchDIBits(wdc, 0, 0, SCREEN_W, SCREEN_H, 0, 0, 640, 360, small, &small_bi, DIB_RGB_COLORS, SRCCOPY);
}

static void frame_text(int f)
{
    RECT r = { 0, 0, SCREEN_W, SCREEN_H };
    int i;
    FillRect(mdc, &r, GetStockObject(BLACK_BRUSH));
    for (i = 0; i < 30; i++) message(mdc, f + i * 13, 20 + (i % 2) * 600, 10 + i * 23);
    BitBlt(wdc, 0, 0, SCREEN_W, SCREEN_H, mdc, 0, 0, SRCCOPY);
}

static void frame_small(int f)
{
    int i;
    for (i = 0; i < 60; i++)
    {
        int x = (i * 211 + f * 3) % (SCREEN_W - 64), y = (i * 97 + f * 2) % (SCREEN_H - 64);
        BitBlt(wdc, x, y, 64, 64, bg_dc, (i * 64) % (SCREEN_W - 64), 0, SRCCOPY);
    }
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    int i;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("gdibench", SCREEN_W, SCREEN_H, NULL);
    wdc = GetDC(hwnd);
    font = CreateFontW(-26, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, L"MS Gothic");
    mdc = dib_dc(SCREEN_W, SCREEN_H, &bits);
    bg_dc = dib_dc(SCREEN_W, SCREEN_H, &bg);
    pic_background(bg, SCREEN_W, SCREEN_H, 1);
    scene = pic_alloc(SCREEN_W, SCREEN_H);
    memcpy(scene, bg, SCREEN_W * SCREEN_H * 4);
    for (i = 0; i < 3; i++)
    {
        unsigned *c = pic_alloc(400, 640);
        pic_character(c, 400, 640, i + 1, 0);
        blend(scene, SCREEN_W, c, 400, 640, 40 + i * 410, 80);
        char_dc[i] = dib_dc(400, 640, &chars[i]);
        pic_character(chars[i], 400, 640, i + 1, 1);
        free(c);
    }
    {
        unsigned *w = pic_alloc(1200, 200);
        pic_window(w, 1200, 200, 0);
        blend(scene, SCREEN_W, w, 1200, 200, 40, 500);
        free(w);
    }
    win_dc = dib_dc(1200, 200, &win);
    pic_window(win, 1200, 200, 1);
    memcpy(bits, scene, SCREEN_W * SCREEN_H * 4);
    small = pic_alloc(640, 360);
    pic_background(small, 640, 360, 2);

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("vn_dirty")) bench_frames("vn_dirty", 20, 240, frame_vn_dirty);
    if (bench_is("alphablend")) bench_frames("alphablend", 10, 120, frame_alphablend);
    if (bench_is("stretch")) bench_frames("stretch", 10, 120, frame_stretch);
    if (bench_is("text")) bench_frames("text", 10, 120, frame_text);
    if (bench_is("small")) bench_frames("small", 10, 240, frame_small);
    bench_end();
    return 0;
}
