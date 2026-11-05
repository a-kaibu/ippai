/* Direct2D + DirectWrite on a Direct3D 11 swap chain (newer engines' 2D and text path), at
 * 1280x720. Each case reports ms per frame.
 *
 *   vn      a scene of shapes (gradient sky, 3 character ellipses, translucent message window
 *           with a border) and the typing line by DrawText
 *   bitmap  DrawBitmap of the background, 3 characters and the message window (bitmaps from
 *           memory: CreateBitmap), and the typing line
 *   text    30 lines of DrawText per frame (DirectWrite layout and glyph rendering)
 *   shapes  1000 rectangles and ellipses with solid and gradient brushes
 *
 * Build: i686-w64-mingw32-gcc -O2 d2d.c -o d2d.exe -mwindows -ld3d11 -ld2d1 -ldwrite -ldxguid -luuid */
#define COBJMACROS
#include "bench.h"
#include <d3d11.h>
#include <d2d1.h>
#include <dwrite.h>
#include "d3d11_read.h"

static const GUID iid_ID2D1Factory = { 0x06152247, 0x6f50, 0x465a, { 0x92, 0x45, 0x11, 0x8b, 0xfd, 0x3b, 0x60, 0x07 } };
static const GUID iid_IDWriteFactory = { 0xb859ee5a, 0xd838, 0x4b5b, { 0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48 } };
static const GUID iid_IDXGISurface = { 0xcafcb56c, 0x6ac3, 0x4889, { 0xbf, 0x47, 0x9e, 0x23, 0xbb, 0xd2, 0x60, 0xec } };

static IDXGISwapChain *sc;
static ID2D1RenderTarget *rt;
static ID2D1Bitmap *b_bg, *b_chr[3], *b_win;
static ID2D1SolidColorBrush *white, *solid;
static ID2D1LinearGradientBrush *grad;
static IDWriteTextFormat *fmt, *small_fmt;

static ID2D1Bitmap *bitmap(int w, int h, const unsigned *pixels)
{
    D2D1_SIZE_U size = { w, h };
    D2D1_BITMAP_PROPERTIES bp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED }, 96, 96 };
    ID2D1Bitmap *b;
    BCHECK(ID2D1RenderTarget_CreateBitmap(rt, size, pixels, w * 4, &bp, &b));
    return b;
}

static void blit(ID2D1Bitmap *b, float x, float y, float w, float h)
{
    D2D1_RECT_F d = { x, y, x + w, y + h };
    ID2D1RenderTarget_DrawBitmap(rt, b, &d, 1, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, NULL);
}

static void text(IDWriteTextFormat *f, const WCHAR *s, int n, float x, float y)
{
    D2D1_RECT_F r = { x, y, x + 1200, y + 60 };
    ID2D1RenderTarget_DrawText(rt, s, n, f, &r, (ID2D1Brush *)white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
}

static void begin(void)
{
    D2D1_COLOR_F black = { 0, 0, 0, 1 };
    ID2D1RenderTarget_BeginDraw(rt);
    ID2D1RenderTarget_Clear(rt, &black);
}

static void end(void)
{
    HRESULT hr = ID2D1RenderTarget_EndDraw(rt, NULL, NULL);
    if (FAILED(hr)) bench_fail("EndDraw");
    d3d11_read_back(sc);
    IDXGISwapChain_Present(sc, 0, 0);
}

static void frame_vn(int f)
{
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), i;
    D2D1_RECT_F sky = { 0, 0, SCREEN_W, SCREEN_H }, win = { 40, 500, 1240, 700 };
    D2D1_COLOR_F panel = { 0.08f, 0.08f, 0.12f, 0.7f };
    begin();
    ID2D1RenderTarget_FillRectangle(rt, &sky, (ID2D1Brush *)grad);
    for (i = 0; i < 3; i++)
    {
        D2D1_ELLIPSE body = { { 240.0f + i * 410, 420 }, 170, 300 }, head = { { 240.0f + i * 410, 200 }, 80, 90 };
        D2D1_COLOR_F c = { 0.5f + 0.4f * (i == 0), 0.5f + 0.4f * (i == 1), 0.5f + 0.4f * (i == 2), 1 };
        ID2D1SolidColorBrush_SetColor(solid, &c);
        ID2D1RenderTarget_FillEllipse(rt, &body, (ID2D1Brush *)solid);
        ID2D1RenderTarget_FillEllipse(rt, &head, (ID2D1Brush *)solid);
    }
    ID2D1SolidColorBrush_SetColor(solid, &panel);
    ID2D1RenderTarget_FillRectangle(rt, &win, (ID2D1Brush *)solid);
    ID2D1RenderTarget_DrawRectangle(rt, &win, (ID2D1Brush *)white, 3, NULL);
    text(fmt, line, n < len ? n : len, 80, 540);
    end();
}

static void frame_bitmap(int f)
{
    const WCHAR *line = bench_lines[(f / 40) % BENCH_LINES];
    int n = f % 40 + 1, len = lstrlenW(line), i;
    begin();
    blit(b_bg, 0, 0, SCREEN_W, SCREEN_H);
    for (i = 0; i < 3; i++) blit(b_chr[i], 40 + i * 410, 80, 400, 640);
    blit(b_win, 40, 500, 1200, 200);
    text(fmt, line, n < len ? n : len, 80, 540);
    end();
}

static void frame_text(int f)
{
    int i;
    begin();
    for (i = 0; i < 30; i++)
    {
        const WCHAR *line = bench_lines[(f + i) % BENCH_LINES];
        text(small_fmt, line, lstrlenW(line), 20 + (i % 2) * 600, 10 + i * 23);
    }
    end();
}

static void frame_shapes(int f)
{
    int i;
    begin();
    for (i = 0; i < 1000; i++)
    {
        float x = (float)((i * 211 + f * 3) % (SCREEN_W - 40)), y = (float)((i * 97 + f * 2) % (SCREEN_H - 40));
        D2D1_COLOR_F c = { (i % 7) / 7.0f, (i % 5) / 5.0f, (i % 3) / 3.0f, 0.7f };
        if (i % 3 == 0)
        {
            D2D1_ELLIPSE e = { { x + 20, y + 20 }, 20, 14 };
            ID2D1SolidColorBrush_SetColor(solid, &c);
            ID2D1RenderTarget_FillEllipse(rt, &e, (ID2D1Brush *)solid);
        }
        else
        {
            D2D1_RECT_F r = { x, y, x + 40, y + 30 };
            if (i % 3 == 1) ID2D1SolidColorBrush_SetColor(solid, &c);
            ID2D1RenderTarget_FillRectangle(rt, &r, i % 3 == 1 ? (ID2D1Brush *)solid : (ID2D1Brush *)grad);
        }
    }
    end();
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    DXGI_SWAP_CHAIN_DESC sd = { 0 };
    D2D1_RENDER_TARGET_PROPERTIES rp = { 0 };
    ID3D11Device *dev;
    ID3D11DeviceContext *ctx;
    IDXGISurface *surface;
    ID2D1Factory *d2d;
    IDWriteFactory *dw;
    ID2D1GradientStopCollection *stops;
    D2D1_GRADIENT_STOP gs[2] = { { 0, { 1, 1, 0, 1 } }, { 1, { 0, 1, 1, 1 } } };
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES lg = { { 0, 0 }, { SCREEN_W, 0 } };
    D2D1_COLOR_F wc = { 1, 1, 1, 1 };
    D3D_FEATURE_LEVEL level;
    HWND hwnd;
    (void)inst, (void)prev, (void)cmd, (void)show;
    bench_args();
    hwnd = bench_window("d2dbench", SCREEN_W, SCREEN_H, NULL);
    sd.BufferDesc.Width = SCREEN_W;
    sd.BufferDesc.Height = SCREEN_H;
    sd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    BCHECK(D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, NULL, 0,
                                         D3D11_SDK_VERSION, &sd, &sc, &dev, &level, &ctx));
    BCHECK(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, &iid_IDWriteFactory, (IUnknown **)&dw));
    BCHECK(IDWriteFactory_CreateTextFormat(dw, L"MS Gothic", NULL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                                           DWRITE_FONT_STRETCH_NORMAL, 32, L"ja-jp", &fmt));
    BCHECK(IDWriteFactory_CreateTextFormat(dw, L"MS Gothic", NULL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                                           DWRITE_FONT_STRETCH_NORMAL, 18, L"ja-jp", &small_fmt));
    BCHECK(IDXGISwapChain_GetBuffer(sc, 0, &iid_IDXGISurface, (void **)&surface));
    BCHECK(D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &iid_ID2D1Factory, NULL, (void **)&d2d));
    rp.pixelFormat.format = DXGI_FORMAT_B8G8R8A8_UNORM;
    rp.pixelFormat.alphaMode = D2D1_ALPHA_MODE_PREMULTIPLIED;
    BCHECK(ID2D1Factory_CreateDxgiSurfaceRenderTarget(d2d, surface, &rp, &rt));
    BCHECK(ID2D1RenderTarget_CreateSolidColorBrush(rt, &wc, NULL, &white));
    BCHECK(ID2D1RenderTarget_CreateSolidColorBrush(rt, &wc, NULL, &solid));
    BCHECK(rt->lpVtbl->CreateGradientStopCollection(rt, gs, 2, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP, &stops));
    BCHECK(ID2D1RenderTarget_CreateLinearGradientBrush(rt, &lg, NULL, stops, &grad));

    if (bench_is("vn")) bench_frames("vn", 20, 240, frame_vn);
    if (bench_is("bitmap"))
    {
        unsigned *p = pic_alloc(SCREEN_W, SCREEN_H);
        int i;
        pic_background(p, SCREEN_W, SCREEN_H, 1);
        b_bg = bitmap(SCREEN_W, SCREEN_H, p);
        for (i = 0; i < 3; i++)
        {
            pic_character(p, 400, 640, i + 1, 1);
            b_chr[i] = bitmap(400, 640, p);
        }
        pic_window(p, 1200, 200, 1);
        b_win = bitmap(1200, 200, p);
        free(p);
        bench_frames("bitmap", 20, 240, frame_bitmap);
    }
    if (bench_is("text")) bench_frames("text", 10, 120, frame_text);
    if (bench_is("shapes")) bench_frames("shapes", 10, 120, frame_shapes);
    bench_end();
    return 0;
}
