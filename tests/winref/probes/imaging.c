/* Picture decoding and pixel conversions as games use them: WIC decoders (PNG incl. 16-bit,
 * palettes, tRNS and Adam7; BMP 1/4/8/16/24/32 bpp, RLE, bitfields, top-down; JPEG 4:2:0, 4:2:2,
 * 4:4:4, progressive, gray, restart markers, CMYK; GIF incl. transparency, interlace and
 * frames) to 32bppBGRA, GDI+ decoding to 32bppARGB, WIC format conversions (premultiplied,
 * 24/16/8 bpp, gray, RGBA) and WIC scaling. Pictures: imaging_data.h (imaging_gen.py).
 * libs: -lole32 -lwindowscodecs -lgdiplus -loleaut32 -luuid */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <objbase.h>
#include <wincodec.h>
#include "imaging_data.h"

static IWICImagingFactory *wic;

static const struct { const GUID *g; const char *n; } formats[] = {
    { &GUID_WICPixelFormat1bppIndexed, "1bppIndexed" }, { &GUID_WICPixelFormat2bppIndexed, "2bppIndexed" },
    { &GUID_WICPixelFormat4bppIndexed, "4bppIndexed" }, { &GUID_WICPixelFormat8bppIndexed, "8bppIndexed" },
    { &GUID_WICPixelFormatBlackWhite, "BlackWhite" }, { &GUID_WICPixelFormat2bppGray, "2bppGray" },
    { &GUID_WICPixelFormat4bppGray, "4bppGray" }, { &GUID_WICPixelFormat8bppGray, "8bppGray" },
    { &GUID_WICPixelFormat16bppGray, "16bppGray" }, { &GUID_WICPixelFormat16bppBGR555, "16bppBGR555" },
    { &GUID_WICPixelFormat16bppBGR565, "16bppBGR565" }, { &GUID_WICPixelFormat24bppBGR, "24bppBGR" },
    { &GUID_WICPixelFormat24bppRGB, "24bppRGB" }, { &GUID_WICPixelFormat32bppBGR, "32bppBGR" },
    { &GUID_WICPixelFormat32bppBGRA, "32bppBGRA" }, { &GUID_WICPixelFormat32bppPBGRA, "32bppPBGRA" },
    { &GUID_WICPixelFormat32bppRGBA, "32bppRGBA" }, { &GUID_WICPixelFormat32bppPRGBA, "32bppPRGBA" },
    { &GUID_WICPixelFormat48bppRGB, "48bppRGB" }, { &GUID_WICPixelFormat64bppRGBA, "64bppRGBA" },
    { &GUID_WICPixelFormat32bppCMYK, "32bppCMYK" }, { &GUID_WICPixelFormat8bppAlpha, "8bppAlpha" },
};

static const char *format_name(const GUID *g) {
    for (int i = 0; i < (int)(sizeof formats / sizeof formats[0]); i++)
        if (IsEqualGUID(g, formats[i].g)) return formats[i].n;
    static char s[64];
    snprintf(s, sizeof s, "%08lx-%04x", g->Data1, g->Data2);
    return s;
}

/* BGRA rows as a picture and, when any alpha is not 255, its alpha as another */
static void show(const char *name, const unsigned char *px, int w, int h, int stride, int tol) {
    probe_image(name, px, w, h, stride, tol);
    int opaque = 1;
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) opaque &= px[y * stride + x * 4 + 3] == 255;
    if (opaque) return;
    unsigned char *a = malloc(w * h * 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) memset(a + (y * w + x) * 4, px[y * stride + x * 4 + 3], 4);
    char n[80];
    snprintf(n, sizeof n, "%s_alpha", name);
    probe_image(n, a, w, h, w * 4, tol);
    free(a);
}

static IWICBitmapDecoder *decoder(const unsigned char *data, unsigned size) {
    IWICStream *s = NULL;
    IWICBitmapDecoder *d = NULL;
    if (FAILED(IWICImagingFactory_CreateStream(wic, &s))) return NULL;
    if (FAILED(IWICStream_InitializeFromMemory(s, (BYTE *)data, size))) { IWICStream_Release(s); return NULL; }
    HRESULT hr = IWICImagingFactory_CreateDecoderFromStream(wic, (IStream *)s, NULL, WICDecodeMetadataCacheOnDemand, &d);
    IWICStream_Release(s);
    if (FAILED(hr)) { printf("CreateDecoderFromStream %#lx\n", hr); return NULL; }
    return d;
}

/* a source converted to 32bppBGRA and shown */
static void show_source(const char *name, IWICBitmapSource *src, int tol) {
    UINT w = 0, h = 0;
    IWICBitmapSource_GetSize(src, &w, &h);
    IWICFormatConverter *c = NULL;
    if (FAILED(IWICImagingFactory_CreateFormatConverter(wic, &c)) || !c) { printf("%s: CreateFormatConverter failed\n", name); return; }
    HRESULT hr = IWICFormatConverter_Initialize(c, src, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, NULL, 0, WICBitmapPaletteTypeCustom);
    if (FAILED(hr)) { printf("%s: to 32bppBGRA %#lx\n", name, hr); IWICFormatConverter_Release(c); return; }
    unsigned char *px = calloc(w * h, 4);
    hr = IWICFormatConverter_CopyPixels(c, NULL, w * 4, w * h * 4, px);
    if (SUCCEEDED(hr)) show(name, px, w, h, w * 4, tol);
    else printf("%s: CopyPixels %#lx\n", name, hr);
    free(px);
    IWICFormatConverter_Release(c);
}

static void decode_all(void) {
    for (int i = 0; i < (int)(sizeof pictures / sizeof pictures[0]); i++) {
        const char *n = pictures[i].name;
        IWICBitmapDecoder *d = decoder(pictures[i].data, pictures[i].size);
        if (!d) { printf("%s: no decoder\n", n); continue; }
        GUID cf;
        UINT frames = 0;
        IWICBitmapDecoder_GetContainerFormat(d, &cf);
        IWICBitmapDecoder_GetFrameCount(d, &frames);
        IWICBitmapFrameDecode *f = NULL;
        HRESULT hr = IWICBitmapDecoder_GetFrame(d, 0, &f);
        if (FAILED(hr)) { printf("%s: GetFrame %#lx\n", n, hr); IWICBitmapDecoder_Release(d); continue; }
        UINT w = 0, h = 0;
        GUID pf;
        double dx = 0, dy = 0;
        IWICBitmapFrameDecode_GetSize(f, &w, &h);
        IWICBitmapFrameDecode_GetPixelFormat(f, &pf);
        IWICBitmapFrameDecode_GetResolution(f, &dx, &dy);
        printf("%s: container %08lx frames %u size %ux%u format %s dpi %.0f %.0f\n", n, cf.Data1, frames, w, h, format_name(&pf), dx, dy);
        /* palette formats: the palette's size and first colors */
        IWICPalette *p = NULL;
        if (FAILED(IWICImagingFactory_CreatePalette(wic, &p)) || !p) printf("%s: CreatePalette failed\n", n);
        else if (SUCCEEDED(IWICBitmapFrameDecode_CopyPalette(f, p))) {
            UINT nc = 0;
            WICColor col[256];
            BOOL alpha = FALSE;
            IWICPalette_GetColors(p, 256, col, &nc);
            IWICPalette_HasAlpha(p, &alpha);
            printf("%s: palette %u alpha %d first %08x %08x last %08x\n", n, nc, alpha, nc > 0 ? col[0] : 0, nc > 1 ? col[1] : 0, nc ? col[nc - 1] : 0);
        }
        if (p) IWICPalette_Release(p);
        int jpeg = strncmp(n, "jpeg", 4) == 0;
        show_source(n, (IWICBitmapSource *)f, jpeg ? 8 : 0);
        IWICBitmapFrameDecode_Release(f);
        /* the other frames of an animation */
        for (UINT k = 1; k < frames && k < 4; k++) {
            if (FAILED(IWICBitmapDecoder_GetFrame(d, k, &f))) { printf("%s: GetFrame %u failed\n", n, k); continue; }
            char fn[64];
            snprintf(fn, sizeof fn, "%s_frame%u", n, k);
            IWICBitmapFrameDecode_GetSize(f, &w, &h);
            printf("%s: size %ux%u\n", fn, w, h);
            show_source(fn, (IWICBitmapSource *)f, 0);
            IWICBitmapFrameDecode_Release(f);
        }
        IWICBitmapDecoder_Release(d);
    }
}

/* converting a 32bppBGRA bitmap with alpha to the formats games ask for: the bytes */
static void conversions(void) {
    enum { CW = 8, CH = 4 };
    unsigned char px[CW * CH * 4];
    for (int i = 0; i < CW * CH; i++) {
        px[i * 4 + 0] = (unsigned char)(i * 37 + 5);
        px[i * 4 + 1] = (unsigned char)(i * 91 + 17);
        px[i * 4 + 2] = (unsigned char)(255 - i * 8);
        px[i * 4 + 3] = (unsigned char)(i * 8 + (i & 1) * 127);
    }
    IWICBitmap *b = NULL;
    HRESULT hr = IWICImagingFactory_CreateBitmapFromMemory(wic, CW, CH, &GUID_WICPixelFormat32bppBGRA, CW * 4, sizeof px, px, &b);
    if (FAILED(hr)) { printf("CreateBitmapFromMemory %#lx\n", hr); return; }
    static const struct { const GUID *g; int bits; } to[] = {
        { &GUID_WICPixelFormat32bppPBGRA, 32 }, { &GUID_WICPixelFormat32bppRGBA, 32 }, { &GUID_WICPixelFormat32bppBGR, 32 },
        { &GUID_WICPixelFormat24bppBGR, 24 }, { &GUID_WICPixelFormat24bppRGB, 24 }, { &GUID_WICPixelFormat16bppBGR565, 16 },
        { &GUID_WICPixelFormat16bppBGR555, 16 }, { &GUID_WICPixelFormat8bppGray, 8 }, { &GUID_WICPixelFormat32bppPRGBA, 32 },
        { &GUID_WICPixelFormat64bppRGBA, 64 }, { &GUID_WICPixelFormat8bppAlpha, 8 }, { &GUID_WICPixelFormatBlackWhite, 1 },
    };
    for (int i = 0; i < (int)(sizeof to / sizeof to[0]); i++) {
        IWICFormatConverter *c = NULL;
        if (FAILED(IWICImagingFactory_CreateFormatConverter(wic, &c)) || !c) { puts("CreateFormatConverter failed"); continue; }
        BOOL can = FALSE;
        IWICFormatConverter_CanConvert(c, &GUID_WICPixelFormat32bppBGRA, to[i].g, &can);
        hr = IWICFormatConverter_Initialize(c, (IWICBitmapSource *)b, to[i].g, WICBitmapDitherTypeNone, NULL, 0, WICBitmapPaletteTypeCustom);
        printf("convert to %s: can %d init %#lx\n", format_name(to[i].g), can, hr);
        if (SUCCEEDED(hr)) {
            UINT stride = (CW * to[i].bits + 7) / 8;
            unsigned char out[CW * CH * 8];
            hr = IWICFormatConverter_CopyPixels(c, NULL, stride, stride * CH, out);
            if (FAILED(hr)) printf("  CopyPixels %#lx\n", hr);
            else
                for (int y = 0; y < CH; y++) {
                    printf("  ");
                    for (UINT k = 0; k < stride; k++) printf("%02x", out[y * stride + k]);
                    printf("\n");
                }
        }
        IWICFormatConverter_Release(c);
    }
    /* scaling: 8x4 to 13x7 and 3x2 with each mode */
    static const WICBitmapInterpolationMode modes[] = { WICBitmapInterpolationModeNearestNeighbor, WICBitmapInterpolationModeLinear,
                                                         WICBitmapInterpolationModeCubic, WICBitmapInterpolationModeFant };
    static const int sizes[][2] = { { 13, 7 }, { 3, 2 }, { 16, 8 } };
    for (int m = 0; m < 4; m++)
        for (int s = 0; s < 3; s++) {
            IWICBitmapScaler *sc = NULL;
            if (FAILED(IWICImagingFactory_CreateBitmapScaler(wic, &sc)) || !sc) { puts("CreateBitmapScaler failed"); continue; }
            hr = IWICBitmapScaler_Initialize(sc, (IWICBitmapSource *)b, sizes[s][0], sizes[s][1], modes[m]);
            char n[64];
            snprintf(n, sizeof n, "scale_%d_%dx%d", m, sizes[s][0], sizes[s][1]);
            if (FAILED(hr)) printf("%s: Initialize %#lx\n", n, hr);
            else {
                unsigned char out[16 * 8 * 4];
                hr = IWICBitmapScaler_CopyPixels(sc, NULL, sizes[s][0] * 4, sizeof out, out);
                if (FAILED(hr)) printf("%s: CopyPixels %#lx\n", n, hr);
                else show(n, out, sizes[s][0], sizes[s][1], sizes[s][0] * 4, 1);
            }
            IWICBitmapScaler_Release(sc);
        }
    /* clipping and flipping */
    IWICBitmapClipper *cl = NULL;
    WICRect r = { 2, 1, 5, 2 };
    if (FAILED(IWICImagingFactory_CreateBitmapClipper(wic, &cl)) || !cl) puts("CreateBitmapClipper failed");
    else {
        printf("clipper %#lx\n", IWICBitmapClipper_Initialize(cl, (IWICBitmapSource *)b, &r));
        show_source("clip", (IWICBitmapSource *)cl, 0);
        IWICBitmapClipper_Release(cl);
    }
    IWICBitmapFlipRotator *fr = NULL;
    if (FAILED(IWICImagingFactory_CreateBitmapFlipRotator(wic, &fr)) || !fr) puts("CreateBitmapFlipRotator failed");
    else {
        printf("fliprotator %#lx\n", IWICBitmapFlipRotator_Initialize(fr, (IWICBitmapSource *)b, WICBitmapTransformRotate90 | WICBitmapTransformFlipHorizontal));
        show_source("rotate90_flip", (IWICBitmapSource *)fr, 0);
        IWICBitmapFlipRotator_Release(fr);
    }
    IWICBitmap_Release(b);
}

/* GDI+: the same pictures through GdipCreateBitmapFromStream, read as 32bppARGB */
typedef struct { UINT32 version; void *callback; BOOL nobg, noext; } GdiplusStartupInput;
typedef struct { UINT width, height; INT stride; INT format; void *scan0; UINT_PTR reserved; } BitmapData;
int WINAPI GdiplusStartup(ULONG_PTR *, const GdiplusStartupInput *, void *);
int WINAPI GdipCreateBitmapFromStream(IStream *, void **);
int WINAPI GdipGetImageWidth(void *, UINT *);
int WINAPI GdipGetImageHeight(void *, UINT *);
int WINAPI GdipGetImagePixelFormat(void *, INT *);
int WINAPI GdipBitmapLockBits(void *, const void *, UINT, INT, BitmapData *);
int WINAPI GdipBitmapUnlockBits(void *, BitmapData *);
int WINAPI GdipDisposeImage(void *);
int WINAPI GdipBitmapGetPixel(void *, INT, INT, DWORD *);

static void gdiplus(void) {
    ULONG_PTR tok;
    GdiplusStartupInput in = { 1, NULL, FALSE, FALSE };
    int st = GdiplusStartup(&tok, &in, NULL);
    printf("GdiplusStartup %d\n", st);
    if (st) return;
    for (int i = 0; i < (int)(sizeof pictures / sizeof pictures[0]); i++) {
        const char *n = pictures[i].name;
        HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE, pictures[i].size);
        memcpy(GlobalLock(g), pictures[i].data, pictures[i].size);
        GlobalUnlock(g);
        IStream *s = NULL;
        CreateStreamOnHGlobal(g, TRUE, &s);
        void *bm = NULL;
        st = GdipCreateBitmapFromStream(s, &bm);
        if (st) { printf("gdip %s: %d\n", n, st); IStream_Release(s); continue; }
        UINT w = 0, h = 0;
        INT pf = 0;
        GdipGetImageWidth(bm, &w);
        GdipGetImageHeight(bm, &h);
        GdipGetImagePixelFormat(bm, &pf);
        printf("gdip %s: %ux%u format %#x\n", n, w, h, pf);
        BitmapData bd;
        INT rect[4] = { 0, 0, (INT)w, (INT)h };
        st = GdipBitmapLockBits(bm, rect, 1 /* read */, 0x26200A /* 32bppARGB */, &bd);
        if (!st) {
            char nn[64];
            snprintf(nn, sizeof nn, "gdip_%s", n);
            show(nn, bd.scan0, w, h, bd.stride, strncmp(n, "jpeg", 4) == 0 ? 8 : 0);
            GdipBitmapUnlockBits(bm, &bd);
        } else printf("gdip %s: LockBits %d\n", n, st);
        DWORD c = 0;
        GdipBitmapGetPixel(bm, 3, 2, &c);
        printf("gdip %s: pixel 3,2 %08lx\n", n, c);
        GdipDisposeImage(bm);
        IStream_Release(s);
    }
}

int main(void) {
    CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    HRESULT hr = CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&wic);
    printf("WICImagingFactory %#lx\n", hr);
    if (wic) {
        decode_all();
        conversions();
    }
    gdiplus();
    return 0;
}
