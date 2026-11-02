/* Direct2D / DirectWrite check for vnvm: a D3D11 swap chain whose back buffer is a Direct2D
 * render target (CreateDxgiSurfaceRenderTarget): a cleared background, a filled rectangle, an
 * ellipse outline, a gradient bar and DirectWrite text (a text layout, its metrics printed).
 * Prints each step and "frame N"; quits after 60 s. "still" is accepted for tests/gfx.mjs.
 * Build: i686-w64-mingw32-gcc -O2 d2dtest.c -o d2dtest.exe -mwindows -ld3d11 -ld2d1 -ldwrite -ldxguid -luuid */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <d2d1.h>
#include <dwrite.h>
#include <stdio.h>
#include <string.h>

static const GUID iid_ID2D1Factory = { 0x06152247, 0x6f50, 0x465a, { 0x92, 0x45, 0x11, 0x8b, 0xfd, 0x3b, 0x60, 0x07 } };
static const GUID iid_IDWriteFactory = { 0xb859ee5a, 0xd838, 0x4b5b, { 0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48 } };
static const GUID iid_IDXGISurface = { 0xcafcb56c, 0x6ac3, 0x4889, { 0xbf, 0x47, 0x9e, 0x23, 0xbb, 0xd2, 0x60, 0xec } };

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(x) do { HRESULT hr_ = (x); printf("%s -> %#lx\n", #x, (unsigned long)hr_); fflush(stdout); if (FAILED(hr_)) return 1; } while (0)

static D2D1_COLOR_F rgb(float r, float g, float b)
{
    D2D1_COLOR_F c = { r, g, b, 1 };
    return c;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    WNDCLASSA wc = { 0 };
    DXGI_SWAP_CHAIN_DESC sd = { 0 };
    D2D1_RENDER_TARGET_PROPERTIES rp = { 0 };
    ID3D11Device *dev; ID3D11DeviceContext *ctx; IDXGISwapChain *sc; IDXGISurface *surface;
    ID2D1Factory *d2d; ID2D1RenderTarget *rt; ID2D1SolidColorBrush *red, *blue, *white;
    ID2D1GradientStopCollection *stops; ID2D1LinearGradientBrush *grad;
    IDWriteFactory *dw; IDWriteTextFormat *fmt; IDWriteTextLayout *layout;
    DWRITE_TEXT_METRICS tm;
    D2D1_GRADIENT_STOP gs[2] = { { 0, { 1, 1, 0, 1 } }, { 1, { 0, 1, 1, 1 } } };
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES lg = { { 40, 0 }, { 600, 0 } };
    D3D_FEATURE_LEVEL level;
    static const WCHAR text[] = L"Direct2D \x30c6\x30b9\x30c8 Text";
    DWORD start = GetTickCount(), frames = 0;
    HWND hwnd;
    MSG msg;
    (void)strstr(cmd, "still");

    wc.lpfnWndProc = wndproc;
    wc.hInstance = inst;
    wc.lpszClassName = "d2dtest";
    RegisterClassA(&wc);
    hwnd = CreateWindowA("d2dtest", "d2dtest", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 656, 519, 0, 0, inst, 0);

    sd.BufferDesc.Width = 640;
    sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    CHECK(D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, NULL, 0,
                                        D3D11_SDK_VERSION, &sd, &sc, &dev, &level, &ctx));
    CHECK(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, &iid_IDWriteFactory, (IUnknown **)&dw));
    CHECK(IDWriteFactory_CreateTextFormat(dw, L"Tahoma", NULL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                                          DWRITE_FONT_STRETCH_NORMAL, 48, L"en-us", &fmt));
    CHECK(IDWriteFactory_CreateTextLayout(dw, text, lstrlenW(text), fmt, 560, 120, &layout));
    CHECK(IDWriteTextLayout_GetMetrics(layout, &tm));
    printf("text metrics %.1f x %.1f, %u lines\n", tm.width, tm.height, tm.lineCount);
    fflush(stdout);
    CHECK(IDXGISwapChain_GetBuffer(sc, 0, &iid_IDXGISurface, (void **)&surface));
    CHECK(D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &iid_ID2D1Factory, NULL, (void **)&d2d));
    rp.pixelFormat.format = DXGI_FORMAT_B8G8R8A8_UNORM;
    rp.pixelFormat.alphaMode = D2D1_ALPHA_MODE_PREMULTIPLIED;
    CHECK(ID2D1Factory_CreateDxgiSurfaceRenderTarget(d2d, surface, &rp, &rt));
    { D2D1_COLOR_F c = rgb(1, 0, 0); CHECK(ID2D1RenderTarget_CreateSolidColorBrush(rt, &c, NULL, &red)); }
    { D2D1_COLOR_F c = rgb(0, 0, 1); CHECK(ID2D1RenderTarget_CreateSolidColorBrush(rt, &c, NULL, &blue)); }
    { D2D1_COLOR_F c = rgb(1, 1, 1); CHECK(ID2D1RenderTarget_CreateSolidColorBrush(rt, &c, NULL, &white)); }
    CHECK(rt->lpVtbl->CreateGradientStopCollection(rt, gs, 2, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP, &stops)); /* (mingw's C macro takes 4) */
    CHECK(ID2D1RenderTarget_CreateLinearGradientBrush(rt, &lg, NULL, stops, &grad));


    while (GetTickCount() - start < 60000)
    {
        D2D1_RECT_F r = { 40, 40, 300, 160 }, bar = { 40, 400, 600, 440 };
        D2D1_ELLIPSE e = { { 460, 100 }, 120, 60 };
        D2D1_POINT_2F at = { 40, 220 };
        D2D1_COLOR_F bg = rgb(0.1f, 0.3f, 0.1f);
        HRESULT hr;

        while (PeekMessageA(&msg, 0, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            DispatchMessageA(&msg);
        }
        ID2D1RenderTarget_BeginDraw(rt);
        ID2D1RenderTarget_Clear(rt, &bg);
        ID2D1RenderTarget_FillRectangle(rt, &r, (ID2D1Brush *)red);
        ID2D1RenderTarget_DrawEllipse(rt, &e, (ID2D1Brush *)blue, 8, NULL);
        ID2D1RenderTarget_FillRectangle(rt, &bar, (ID2D1Brush *)grad);
        ID2D1RenderTarget_DrawTextLayout(rt, at, layout, (ID2D1Brush *)white, D2D1_DRAW_TEXT_OPTIONS_NONE);
        hr = ID2D1RenderTarget_EndDraw(rt, NULL, NULL);
        if (!frames) { printf("EndDraw -> %#lx\n", (unsigned long)hr); fflush(stdout); }
        IDXGISwapChain_Present(sc, 1, 0);
        if (++frames % 60 == 0) { printf("frame %lu\n", frames); fflush(stdout); }
    }
    printf("done, %lu frames\n", frames);
    return 0;
}
