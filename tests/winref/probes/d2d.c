/* Direct2D on a DXGI surface of a Direct3D 11 texture (as games use it): Clear, aliased and
 * antialiased rectangles, ellipses and their outlines, stroke widths, transforms (the matrix
 * helpers), opacity, axis-aligned clips, linear gradients, bitmaps (nearest/linear, opacity,
 * source rects), and what the render target reports (DPI, sizes). Each picture is the 64x64
 * B8G8R8A8 texture read back through a staging copy. Also bitmaps of another D3D11 texture
 * (ID2D1DeviceContext::CreateBitmapFromDxgiSurface, premultiplied and alpha ignored), drawn as
 * KiriKiri's DrawDeviceD2D draws its picture onto the window. WARP on the Windows runner.
 * libs: -ld3d11 -ld2d1 -luuid
 * run: browser (sake draws through WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d3d11.h>
#include <d2d1.h>
#include <d2d1_1.h>

#define W 64
#define H 64

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11Texture2D *tex, *stage;
static ID2D1RenderTarget *rt;

static void shot(const char *name, int tol) {
    D3D11_MAPPED_SUBRESOURCE m;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)stage, (ID3D11Resource *)tex);
    if (FAILED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)stage, 0, D3D11_MAP_READ, 0, &m))) { printf("%s: Map failed\n", name); return; }
    probe_image(name, m.pData, W, H, m.RowPitch, tol);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)stage, 0);
}

static D2D1_COLOR_F rgba(float r, float g, float b, float a) { D2D1_COLOR_F c = { r, g, b, a }; return c; }
static D2D1_RECT_F rect(float l, float t, float r, float b) { D2D1_RECT_F x = { l, t, r, b }; return x; }
static D2D1_ELLIPSE ell(float x, float y, float rx, float ry) { D2D1_ELLIPSE e = { { x, y }, rx, ry }; return e; }

static ID2D1SolidColorBrush *brush(float r, float g, float b, float a) {
    ID2D1SolidColorBrush *s = NULL;
    D2D1_COLOR_F c = rgba(r, g, b, a);
    HRESULT hr = ID2D1RenderTarget_CreateSolidColorBrush(rt, &c, NULL, &s);
    if (FAILED(hr)) printf("CreateSolidColorBrush %#lx\n", hr);
    return s;
}

static const D2D1_MATRIX_3X2_F ident = { { { 1, 0, 0, 1, 0, 0 } } };

static void begin(D2D1_COLOR_F clear) {
    ID2D1RenderTarget_BeginDraw(rt);
    ID2D1RenderTarget_SetTransform(rt, &ident);
    ID2D1RenderTarget_Clear(rt, &clear);
}
static void end(const char *name) {
    HRESULT hr = ID2D1RenderTarget_EndDraw(rt, NULL, NULL);
    if (hr) printf("%s: EndDraw %#lx\n", name, hr);
}

static void matrix(const char *n, const D2D1_MATRIX_3X2_F *m) {
    printf("%s %.6g %.6g %.6g %.6g %.6g %.6g\n", n, m->_11, m->_12, m->_21, m->_22, m->_31, m->_32);
}

int main(void) {
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    HRESULT hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) { printf("skip D3D11CreateDevice %#lx\n", hr); return 0; }
    D3D11_TEXTURE2D_DESC td = { W, H, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0 };
    ID3D11Device_CreateTexture2D(dev, &td, NULL, &tex);
    td.Usage = D3D11_USAGE_STAGING; td.BindFlags = 0; td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Device_CreateTexture2D(dev, &td, NULL, &stage);

    /* the matrix helpers */
    {
        D2D1_MATRIX_3X2_F m;
        D2D1_POINT_2F c = { 10, 20 };
        D2D1MakeRotateMatrix(30, c, &m); matrix("rotate 30 (10,20)", &m);
        D2D1MakeRotateMatrix(-90, c, &m); matrix("rotate -90 (10,20)", &m);
        D2D1MakeSkewMatrix(15, 30, c, &m); matrix("skew 15 30 (10,20)", &m);
        D2D1_MATRIX_3X2_F a = { { { 2, 1, 1, 3, 5, 7 } } }, z = { { { 1, 2, 2, 4, 0, 0 } } };
        printf("invertible %d %d\n", D2D1IsMatrixInvertible(&a), D2D1IsMatrixInvertible(&z));
        printf("invert %d", D2D1InvertMatrix(&a)); matrix("", &a);
    }

    ID2D1Factory *f = NULL;
    hr = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &IID_ID2D1Factory, NULL, (void **)&f);
    if (FAILED(hr)) { printf("skip D2D1CreateFactory %#lx\n", hr); return 0; }
    IDXGISurface *surf = NULL;
    ID3D11Texture2D_QueryInterface(tex, &IID_IDXGISurface, (void **)&surf);
    D2D1_RENDER_TARGET_PROPERTIES p = { D2D1_RENDER_TARGET_TYPE_DEFAULT, { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED }, 0, 0, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_FEATURE_LEVEL_DEFAULT };
    hr = ID2D1Factory_CreateDxgiSurfaceRenderTarget(f, surf, &p, &rt);
    if (FAILED(hr)) { printf("skip CreateDxgiSurfaceRenderTarget %#lx\n", hr); return 0; }
    {
        FLOAT dx = 0, dy = 0;
        ID2D1RenderTarget_GetDpi(rt, &dx, &dy);
        printf("dpi %g %g\n", dx, dy);
        p.dpiX = p.dpiY = 192;
        ID2D1RenderTarget *r2 = NULL;
        hr = ID2D1Factory_CreateDxgiSurfaceRenderTarget(f, surf, &p, &r2);
        if (r2) {
            ID2D1RenderTarget_GetDpi(r2, &dx, &dy);
            printf("dpi 192 target: %#lx %g %g\n", hr, dx, dy);
            ID2D1RenderTarget_SetDpi(r2, 48, 72);
            ID2D1RenderTarget_GetDpi(r2, &dx, &dy);
            printf("SetDpi 48 72: %g %g\n", dx, dy);
            ID2D1RenderTarget_Release(r2);
        }
    }

    /* aliased fills: integer and half-pixel edges, overlap with alpha, ellipses */
    ID2D1SolidColorBrush *red = brush(1, 0, 0, 1), *green = brush(0, 1, 0, 1), *blue = brush(0, 0, 1, 0.5f), *white = brush(1, 1, 1, 1);
    {
        ID2D1RenderTarget_SetAntialiasMode(rt, D2D1_ANTIALIAS_MODE_ALIASED);
        begin(rgba(0.125f, 0.25f, 0.5f, 1));
        D2D1_RECT_F r1 = rect(2, 2, 20, 12), r2 = rect(22.5f, 2.5f, 40.5f, 12.5f), r3 = rect(42.25f, 2.75f, 60.75f, 12.25f), r4 = rect(10, 6, 50, 30);
        ID2D1RenderTarget_FillRectangle(rt, &r1, (ID2D1Brush *)red);
        ID2D1RenderTarget_FillRectangle(rt, &r2, (ID2D1Brush *)green);
        ID2D1RenderTarget_FillRectangle(rt, &r3, (ID2D1Brush *)red);
        ID2D1RenderTarget_FillRectangle(rt, &r4, (ID2D1Brush *)blue);
        D2D1_ELLIPSE e1 = ell(16, 46, 12, 8), e2 = ell(46.5f, 46.5f, 10.25f, 14);
        ID2D1RenderTarget_FillEllipse(rt, &e1, (ID2D1Brush *)white);
        ID2D1RenderTarget_FillEllipse(rt, &e2, (ID2D1Brush *)green);
        end("fills_aliased");
        shot("fills_aliased", 0);
    }
    /* the same antialiased */
    {
        ID2D1RenderTarget_SetAntialiasMode(rt, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        begin(rgba(0.125f, 0.25f, 0.5f, 1));
        D2D1_RECT_F r2 = rect(22.5f, 2.5f, 40.5f, 12.5f), r3 = rect(42.25f, 2.75f, 60.75f, 12.25f);
        ID2D1RenderTarget_FillRectangle(rt, &r2, (ID2D1Brush *)green);
        ID2D1RenderTarget_FillRectangle(rt, &r3, (ID2D1Brush *)red);
        D2D1_ELLIPSE e1 = ell(16, 46, 12, 8), e2 = ell(46.5f, 46.5f, 10.25f, 14);
        ID2D1RenderTarget_FillEllipse(rt, &e1, (ID2D1Brush *)white);
        ID2D1RenderTarget_FillEllipse(rt, &e2, (ID2D1Brush *)green);
        end("fills_aa");
        shot("fills_aa", 24);
    }
    /* outlines: stroke widths 1, 2, 3.5 on rectangles and an ellipse (aliased) */
    {
        ID2D1RenderTarget_SetAntialiasMode(rt, D2D1_ANTIALIAS_MODE_ALIASED);
        begin(rgba(0, 0, 0, 1));
        D2D1_RECT_F r1 = rect(2.5f, 2.5f, 20.5f, 20.5f), r2 = rect(24, 3, 40, 19), r3 = rect(44, 4, 60, 20);
        ID2D1RenderTarget_DrawRectangle(rt, &r1, (ID2D1Brush *)white, 1, NULL);
        ID2D1RenderTarget_DrawRectangle(rt, &r2, (ID2D1Brush *)red, 2, NULL);
        ID2D1RenderTarget_DrawRectangle(rt, &r3, (ID2D1Brush *)green, 3.5f, NULL);
        D2D1_ELLIPSE e = ell(32, 44, 20, 12);
        ID2D1RenderTarget_DrawEllipse(rt, &e, (ID2D1Brush *)white, 2, NULL);
        end("outlines");
        shot("outlines", 0);
    }
    /* a bitmap of another texture (a DXGI surface): the texture's pixels as D3D11 left them */
    {
        enum { SW = 8, SH = 8 };
        UINT px[SW * SH];
        for (int y = 0; y < SH; y++)
            for (int x = 0; x < SW; x++) /* premultiplied: red, green, blue, half white quadrants */
                px[y * SW + x] = y < 4 ? (x < 4 ? 0xffff0000 : 0xff00ff00) : (x < 4 ? 0xff0000ff : 0x80808080);
        D3D11_TEXTURE2D_DESC sd = { SW, SH, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0 };
        D3D11_SUBRESOURCE_DATA init = { px, SW * 4, 0 };
        ID3D11Texture2D *src = NULL;
        IDXGISurface *ss = NULL;
        ID2D1DeviceContext *dc = NULL;
        ID2D1Bitmap1 *b1 = NULL, *b2 = NULL;
        hr = ID3D11Device_CreateTexture2D(dev, &sd, &init, &src);
        if (src) ID3D11Texture2D_QueryInterface(src, &IID_IDXGISurface, (void **)&ss);
        hr = ID2D1RenderTarget_QueryInterface(rt, &IID_ID2D1DeviceContext, (void **)&dc);
        printf("ID2D1DeviceContext %#lx\n", hr);
        if (dc && ss) {
            D2D1_BITMAP_PROPERTIES1 bp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED }, 96, 96, D2D1_BITMAP_OPTIONS_NONE, NULL };
            hr = ID2D1DeviceContext_CreateBitmapFromDxgiSurface(dc, ss, &bp, &b1);
            printf("CreateBitmapFromDxgiSurface %#lx\n", hr);
            bp.pixelFormat.alphaMode = D2D1_ALPHA_MODE_IGNORE;
            hr = ID2D1DeviceContext_CreateBitmapFromDxgiSurface(dc, ss, &bp, &b2);
            printf("CreateBitmapFromDxgiSurface ignore %#lx\n", hr);
        }
        if (b1 && b2) {
            D2D1_SIZE_U ps;
            /* (by the vtable: mingw's C ID2D1Bitmap1 has no struct-returning methods) slots 5, 6 */
            ((D2D1_SIZE_U *(STDMETHODCALLTYPE *)(void *, D2D1_SIZE_U *))(*(void ***)b1)[5])(b1, &ps);
            D2D1_PIXEL_FORMAT pf;
            ((D2D1_PIXEL_FORMAT *(STDMETHODCALLTYPE *)(void *, D2D1_PIXEL_FORMAT *))(*(void ***)b2)[6])(b2, &pf);
            printf("surface bitmap %ux%u, ignore format %u alpha %u\n", ps.width, ps.height, pf.format, pf.alphaMode);
            begin(rgba(0, 0, 0.25f, 1));
            D2D1_RECT_F d1 = rect(2, 2, 10, 10), d2 = rect(12, 2, 44, 34), d3 = rect(46, 2, 62, 18), d4 = rect(2, 40, 18, 56), s4 = rect(4, 4, 8, 8);
            ID2D1RenderTarget_DrawBitmap(rt, (ID2D1Bitmap *)b1, &d1, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, (ID2D1Bitmap *)b1, &d2, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, (ID2D1Bitmap *)b2, &d3, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, (ID2D1Bitmap *)b1, &d4, 0.5f, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, &s4);
            end("surface_bitmaps");
            shot("surface_bitmaps", 2);
            /* the texture changed by D3D11 between frames: the next draw shows the new pixels */
            for (int i = 0; i < SW * SH; i++) px[i] = 0xffffff00;
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)src, 0, NULL, px, SW * 4, 0);
            begin(rgba(0, 0, 0, 1));
            ID2D1RenderTarget_DrawBitmap(rt, (ID2D1Bitmap *)b1, &d2, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            end("surface_bitmap_updated");
            shot("surface_bitmap_updated", 0);
        }
        if (b1) ID2D1Bitmap_Release((ID2D1Bitmap *)b1);
        if (b2) ID2D1Bitmap_Release((ID2D1Bitmap *)b2);
        if (dc) ID2D1RenderTarget_Release((ID2D1RenderTarget *)dc);
        if (ss) IDXGISurface_Release(ss);
        if (src) ID3D11Texture2D_Release(src);
    }
    /* transforms: translate+scale, rotation about a point, opacity of a brush, clips */
    {
        begin(rgba(0, 0, 0, 1));
        D2D1_MATRIX_3X2_F m = { { { 2, 0, 0, 1.5f, 4, 2 } } };
        ID2D1RenderTarget_SetTransform(rt, &m);
        D2D1_RECT_F r = rect(0, 0, 8, 8);
        ID2D1RenderTarget_FillRectangle(rt, &r, (ID2D1Brush *)red);
        D2D1_POINT_2F c = { 40, 16 };
        D2D1MakeRotateMatrix(45, c, &m);
        ID2D1RenderTarget_SetTransform(rt, &m);
        D2D1_RECT_F r2 = rect(32, 8, 48, 24);
        ID2D1RenderTarget_FillRectangle(rt, &r2, (ID2D1Brush *)green);
        ID2D1RenderTarget_SetTransform(rt, &ident);
        ID2D1Brush_SetOpacity((ID2D1Brush *)white, 0.25f);
        D2D1_RECT_F r3 = rect(4, 30, 30, 40);
        ID2D1RenderTarget_FillRectangle(rt, &r3, (ID2D1Brush *)white);
        ID2D1Brush_SetOpacity((ID2D1Brush *)white, 1);
        D2D1_RECT_F clip = rect(36, 34, 52, 58), r4 = rect(30, 30, 62, 62);
        ID2D1RenderTarget_PushAxisAlignedClip(rt, &clip, D2D1_ANTIALIAS_MODE_ALIASED);
        ID2D1RenderTarget_FillRectangle(rt, &r4, (ID2D1Brush *)red);
        ID2D1RenderTarget_PopAxisAlignedClip(rt);
        D2D1_RECT_F clip2 = rect(4.5f, 44.5f, 20.5f, 60.5f), r5 = rect(0, 42, 26, 62);
        ID2D1RenderTarget_PushAxisAlignedClip(rt, &clip2, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        ID2D1RenderTarget_FillRectangle(rt, &r5, (ID2D1Brush *)green);
        ID2D1RenderTarget_PopAxisAlignedClip(rt);
        end("transforms");
        shot("transforms", 8);
    }
    /* a linear gradient (three stops) horizontally and diagonally */
    {
        D2D1_GRADIENT_STOP st[3] = { { 0, { 1, 0, 0, 1 } }, { 0.5f, { 0, 1, 0, 1 } }, { 1, { 0, 0, 1, 0.5f } } };
        ID2D1GradientStopCollection *sc = NULL;
        hr = rt->lpVtbl->CreateGradientStopCollection(rt, st, 3, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP, &sc);
        printf("CreateGradientStopCollection %#lx\n", hr);
        if (sc) {
            ID2D1LinearGradientBrush *g = NULL, *g2 = NULL;
            D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES lp = { { 8, 0 }, { 56, 0 } }, lp2 = { { 0, 34 }, { 30, 62 } };
            ID2D1RenderTarget_CreateLinearGradientBrush(rt, &lp, NULL, sc, &g);
            ID2D1RenderTarget_CreateLinearGradientBrush(rt, &lp2, NULL, sc, &g2);
            begin(rgba(1, 1, 1, 1));
            D2D1_RECT_F r = rect(0, 0, 64, 30), r2 = rect(0, 34, 64, 64);
            if (g) ID2D1RenderTarget_FillRectangle(rt, &r, (ID2D1Brush *)g);
            if (g2) ID2D1RenderTarget_FillRectangle(rt, &r2, (ID2D1Brush *)g2);
            end("gradient");
            shot("gradient", 3);
            if (g) ID2D1LinearGradientBrush_Release(g);
            if (g2) ID2D1LinearGradientBrush_Release(g2);
            ID2D1GradientStopCollection_Release(sc);
        }
    }
    /* bitmaps: a 4x4 premultiplied picture drawn 1:1, enlarged nearest and linear, with
     * opacity, and a source rect */
    {
        DWORD px[16];
        for (int i = 0; i < 16; i++) {
            unsigned a = i == 5 ? 0x80 : 0xff, c = (unsigned)(i * 16) * a / 255;
            px[i] = a << 24 | c << 16 | ((i & 3) * 0x55 * a / 255) << 8 | ((i >> 2) * 0x55 * a / 255);
        }
        ID2D1Bitmap *bm = NULL;
        D2D1_SIZE_U sz = { 4, 4 };
        D2D1_BITMAP_PROPERTIES bp = { { DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED }, 96, 96 };
        hr = ID2D1RenderTarget_CreateBitmap(rt, sz, px, 16, &bp, &bm);
        printf("CreateBitmap %#lx\n", hr);
        if (bm) {
            begin(rgba(0.25f, 0.25f, 0.25f, 1));
            D2D1_RECT_F d1 = rect(2, 2, 6, 6), d2 = rect(10, 2, 34, 26), d3 = rect(38, 2, 62, 26), d4 = rect(2, 34, 26, 58), d5 = rect(38, 34, 62, 58), s5 = rect(1, 1, 3, 3);
            ID2D1RenderTarget_DrawBitmap(rt, bm, &d1, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, bm, &d2, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, bm, &d3, 1, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, bm, &d4, 0.5f, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, NULL);
            ID2D1RenderTarget_DrawBitmap(rt, bm, &d5, 1, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, &s5);
            end("bitmaps");
            shot("bitmaps", 4);
            ID2D1Bitmap_Release(bm);
        }
    }
    /* drawing state (formal/tla/D2dDraw.tla): what EndDraw and Flush return out of order */
    {
        D2D1_TAG t1, t2;
        D2D1_RECT_F r = rect(0, 0, 8, 8), cl = rect(0, 0, 32, 32);
        printf("state: EndDraw alone %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        printf("state: Flush alone %#lx\n", ID2D1RenderTarget_Flush(rt, &t1, &t2));
        ID2D1RenderTarget_BeginDraw(rt);
        ID2D1RenderTarget_BeginDraw(rt);
        printf("state: Begin Begin End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        ID2D1RenderTarget_BeginDraw(rt);
        ID2D1RenderTarget_PopAxisAlignedClip(rt);
        printf("state: Begin Pop Flush %#lx\n", ID2D1RenderTarget_Flush(rt, &t1, &t2));
        printf("state: ... End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        ID2D1RenderTarget_BeginDraw(rt);
        ID2D1RenderTarget_PushAxisAlignedClip(rt, &cl, D2D1_ANTIALIAS_MODE_ALIASED);
        printf("state: Begin Push End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        ID2D1RenderTarget_BeginDraw(rt);
        printf("state: then Begin End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        ID2D1RenderTarget_FillRectangle(rt, &r, (ID2D1Brush *)red);
        ID2D1RenderTarget_BeginDraw(rt);
        printf("state: Fill Begin End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
        ID2D1RenderTarget_BeginDraw(rt);
        ID2D1RenderTarget_PushAxisAlignedClip(rt, &cl, D2D1_ANTIALIAS_MODE_ALIASED);
        ID2D1RenderTarget_PopAxisAlignedClip(rt);
        printf("state: Begin Push Pop End %#lx\n", ID2D1RenderTarget_EndDraw(rt, NULL, NULL));
    }
    ID2D1SolidColorBrush_Release(red);
    ID2D1SolidColorBrush_Release(green);
    ID2D1SolidColorBrush_Release(blue);
    ID2D1SolidColorBrush_Release(white);
    printf("Release target %lu\n", ID2D1RenderTarget_Release(rt));
    IDXGISurface_Release(surf);
    ID2D1Factory_Release(f);
    return 0;
}
