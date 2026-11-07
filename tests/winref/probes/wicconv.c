/* WIC's pixel arithmetic, measured: every pixel format games hand to WIC converted to
 * 32bppBGRA, 32bppBGRA converted to the rest, conversion to indexed formats through a custom
 * palette, the predefined palettes, every flip/rotate option, the scaler's weights per mode
 * (impulse pictures: one bright column or row of an opaque picture, at two levels), and a
 * 9x9x9x9 grid of 32bppCMYK colors as sRGB, and every 16-bit level of 16bppGray, 48bppRGB and
 * 64bppRGBA (opaque) as 8 bits.
 * libs: -lole32 -lwindowscodecs -luuid */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <objbase.h>
#include <wincodec.h>

static IWICImagingFactory *wic;

static const struct { const GUID *g; const char *n; int bits; } formats[] = {
    { &GUID_WICPixelFormatBlackWhite, "BlackWhite", 1 }, { &GUID_WICPixelFormat2bppGray, "2bppGray", 2 },
    { &GUID_WICPixelFormat4bppGray, "4bppGray", 4 }, { &GUID_WICPixelFormat8bppGray, "8bppGray", 8 },
    { &GUID_WICPixelFormat16bppGray, "16bppGray", 16 }, { &GUID_WICPixelFormat16bppBGR555, "16bppBGR555", 16 },
    { &GUID_WICPixelFormat16bppBGR565, "16bppBGR565", 16 }, { &GUID_WICPixelFormat24bppBGR, "24bppBGR", 24 },
    { &GUID_WICPixelFormat24bppRGB, "24bppRGB", 24 }, { &GUID_WICPixelFormat32bppBGR, "32bppBGR", 32 },
    { &GUID_WICPixelFormat32bppBGRA, "32bppBGRA", 32 }, { &GUID_WICPixelFormat32bppPBGRA, "32bppPBGRA", 32 },
    { &GUID_WICPixelFormat32bppRGB, "32bppRGB", 32 }, { &GUID_WICPixelFormat32bppRGBA, "32bppRGBA", 32 },
    { &GUID_WICPixelFormat32bppPRGBA, "32bppPRGBA", 32 }, { &GUID_WICPixelFormat48bppRGB, "48bppRGB", 48 },
    { &GUID_WICPixelFormat64bppRGBA, "64bppRGBA", 64 }, { &GUID_WICPixelFormat32bppCMYK, "32bppCMYK", 32 },
    { &GUID_WICPixelFormat8bppAlpha, "8bppAlpha", 8 }, { &GUID_WICPixelFormat1bppIndexed, "1bppIndexed", 1 },
    { &GUID_WICPixelFormat2bppIndexed, "2bppIndexed", 2 }, { &GUID_WICPixelFormat4bppIndexed, "4bppIndexed", 4 },
    { &GUID_WICPixelFormat8bppIndexed, "8bppIndexed", 8 },
};
enum { NF = sizeof formats / sizeof formats[0] };

static void hex(const char *label, const unsigned char *p, int n) {
    printf("%s ", label);
    for (int i = 0; i < n; i++) printf("%02x", p[i]);
    printf("\n");
}

static IWICPalette *palette16(void) {
    IWICPalette *p = NULL;
    if (FAILED(IWICImagingFactory_CreatePalette(wic, &p)) || !p) return NULL;
    WICColor c[16];
    for (int i = 0; i < 16; i++) c[i] = (i == 5 ? 0x40000000u : 0xff000000u) | (unsigned)(i * 17) << 16 | (unsigned)(255 - i * 13) << 8 | (unsigned)((i * 71) & 255);
    IWICPalette_InitializeCustom(p, c, 16);
    return p;
}

/* a W x H bitmap of format f with deterministic bytes */
enum { W = 8, H = 4 };
static IWICBitmap *bitmap_of(int f, unsigned seed) {
    int stride = (W * formats[f].bits + 7) / 8;
    unsigned char px[W * 8 * H];
    for (int i = 0; i < stride * H; i++) px[i] = (unsigned char)((i * 73 + seed * 29 + 41) ^ (i >> 3) * 151);
    IWICBitmap *b = NULL;
    HRESULT hr = IWICImagingFactory_CreateBitmapFromMemory(wic, W, H, formats[f].g, stride, stride * H, px, &b);
    if (FAILED(hr)) { printf("%s: CreateBitmapFromMemory %#lx\n", formats[f].n, hr); return NULL; }
    if (f >= NF - 4) {
        IWICPalette *p = palette16();
        if (p) { printf("%s: SetPalette %#lx\n", formats[f].n, IWICBitmap_SetPalette(b, p)); IWICPalette_Release(p); }
    }
    return b;
}

/* source -> target through a converter: CanConvert, Initialize, the bytes */
static void convert(IWICBitmapSource *src, int from, int to, IWICPalette *pal) {
    char label[96];
    snprintf(label, sizeof label, "%s>%s", formats[from].n, formats[to].n);
    IWICFormatConverter *c = NULL;
    if (FAILED(IWICImagingFactory_CreateFormatConverter(wic, &c)) || !c) { printf("%s: no converter\n", label); return; }
    BOOL can = FALSE;
    IWICFormatConverter_CanConvert(c, formats[from].g, formats[to].g, &can);
    HRESULT hr = IWICFormatConverter_Initialize(c, src, formats[to].g, WICBitmapDitherTypeNone, pal, 0, pal ? WICBitmapPaletteTypeCustom : WICBitmapPaletteTypeMedianCut);
    if (FAILED(hr)) { printf("%s: can %d init %#lx\n", label, can, hr); IWICFormatConverter_Release(c); return; }
    int stride = (W * formats[to].bits + 7) / 8;
    unsigned char out[W * 8 * H];
    memset(out, 0xcd, sizeof out);
    hr = IWICFormatConverter_CopyPixels(c, NULL, stride, stride * H, out);
    if (FAILED(hr)) printf("%s: can %d CopyPixels %#lx\n", label, can, hr);
    else {
        char l2[128];
        snprintf(l2, sizeof l2, "%s: can %d", label, can);
        hex(l2, out, stride * H);
    }
    IWICFormatConverter_Release(c);
}

static void conversions(void) {
    int bgra = 10;
    /* everything to 32bppBGRA (the source bytes first) */
    for (int f = 0; f < NF; f++) {
        IWICBitmap *b = bitmap_of(f, f);
        if (!b) continue;
        convert((IWICBitmapSource *)b, f, bgra, NULL);
        IWICBitmap_Release(b);
    }
    /* 32bppBGRA (with alpha) to everything; to indexed with the custom palette */
    IWICPalette *pal = palette16();
    for (int seed = 0; seed < 2; seed++) {
        IWICBitmap *b = bitmap_of(bgra, 100 + seed);
        if (!b) continue;
        for (int f = 0; f < NF; f++) convert((IWICBitmapSource *)b, bgra, f, f >= NF - 4 ? pal : NULL);
        IWICBitmap_Release(b);
    }
    /* some direct pairs games use */
    static const int pairs[][2] = { { 7, 11 }, { 3, 11 }, { 11, 10 }, { 14, 11 }, { 16, 11 }, { 15, 7 }, { 4, 3 }, { 6, 7 }, { 13, 7 }, { 22, 11 } };
    for (int i = 0; i < (int)(sizeof pairs / sizeof pairs[0]); i++) {
        IWICBitmap *b = bitmap_of(pairs[i][0], 200 + i);
        if (!b) continue;
        convert((IWICBitmapSource *)b, pairs[i][0], pairs[i][1], NULL);
        IWICBitmap_Release(b);
    }
    if (pal) IWICPalette_Release(pal);
}

static void palettes(void) {
    for (int t = 2; t <= 12; t++)
        for (int tr = 0; tr < 2; tr++) {
            IWICPalette *p = NULL;
            if (FAILED(IWICImagingFactory_CreatePalette(wic, &p)) || !p) continue;
            HRESULT hr = IWICPalette_InitializePredefined(p, (WICBitmapPaletteType)t, tr);
            if (FAILED(hr)) { printf("palette %d %d: %#lx\n", t, tr, hr); IWICPalette_Release(p); continue; }
            UINT n = 0;
            WICColor c[257];
            BOOL bw = 0, gray = 0, alpha = 0;
            WICBitmapPaletteType type = 0;
            IWICPalette_GetColors(p, 257, c, &n);
            IWICPalette_IsBlackWhite(p, &bw);
            IWICPalette_IsGrayscale(p, &gray);
            IWICPalette_HasAlpha(p, &alpha);
            IWICPalette_GetType(p, &type);
            printf("palette %d %d: type %d count %u bw %d gray %d alpha %d\n ", t, tr, type, n, bw, gray, alpha);
            for (UINT i = 0; i < n; i++) printf(" %08x", c[i]);
            printf("\n");
            IWICPalette_Release(p);
        }
}

static void flips(void) {
    unsigned char px[3 * 2 * 4];
    for (int i = 0; i < 6; i++) { px[i * 4] = (unsigned char)i; px[i * 4 + 1] = px[i * 4 + 2] = 0; px[i * 4 + 3] = 255; }
    IWICBitmap *b = NULL;
    if (FAILED(IWICImagingFactory_CreateBitmapFromMemory(wic, 3, 2, &GUID_WICPixelFormat32bppBGRA, 12, sizeof px, px, &b))) return;
    for (int rot = 0; rot < 4; rot++)
        for (int fl = 0; fl < 4; fl++) {
            IWICBitmapFlipRotator *fr = NULL;
            if (FAILED(IWICImagingFactory_CreateBitmapFlipRotator(wic, &fr)) || !fr) continue;
            WICBitmapTransformOptions o = (WICBitmapTransformOptions)(rot | (fl & 1 ? WICBitmapTransformFlipHorizontal : 0) | (fl & 2 ? WICBitmapTransformFlipVertical : 0));
            HRESULT hr = IWICBitmapFlipRotator_Initialize(fr, (IWICBitmapSource *)b, o);
            UINT w = 0, h = 0;
            IWICBitmapFlipRotator_GetSize(fr, &w, &h);
            unsigned char out[6 * 4];
            HRESULT hc = IWICBitmapFlipRotator_CopyPixels(fr, NULL, w * 4, sizeof out, out);
            printf("flip %#x: %#lx %ux%u %#lx", o, hr, w, h, hc);
            for (UINT i = 0; i < w * h && SUCCEEDED(hc); i++) printf(" %d", out[i * 4]);
            printf("\n");
            IWICBitmapFlipRotator_Release(fr);
        }
    IWICBitmap_Release(b);
}

/* the scaler's weights: an opaque 8x4 picture, black but for one column (or row) at level v */
static void scaler_weights(void) {
    static const int sizes[][2] = { { 13, 7 }, { 3, 2 }, { 16, 8 }, { 5, 3 }, { 21, 9 }, { 4, 2 }, { 8, 4 }, { 1, 1 } };
    static const int levels[] = { 255, 100 };
    for (int m = 0; m < 4; m++)
        for (int s = 0; s < (int)(sizeof sizes / sizeof sizes[0]); s++)
            for (int lv = 0; lv < 2; lv++)
                for (int k = 0; k < 12; k++) {
                    unsigned char px[8 * 4 * 4];
                    for (int y = 0; y < 4; y++)
                        for (int x = 0; x < 8; x++) {
                            unsigned char *p = px + (y * 8 + x) * 4;
                            int on = k < 8 ? x == k : y == k - 8;
                            p[0] = p[1] = p[2] = on ? (unsigned char)levels[lv] : 0;
                            p[3] = 255;
                        }
                    IWICBitmap *b = NULL;
                    if (FAILED(IWICImagingFactory_CreateBitmapFromMemory(wic, 8, 4, &GUID_WICPixelFormat32bppBGRA, 32, sizeof px, px, &b))) continue;
                    IWICBitmapScaler *sc = NULL;
                    int w = sizes[s][0], h = sizes[s][1];
                    if (SUCCEEDED(IWICImagingFactory_CreateBitmapScaler(wic, &sc)) && sc &&
                        SUCCEEDED(IWICBitmapScaler_Initialize(sc, (IWICBitmapSource *)b, w, h, (WICBitmapInterpolationMode)m))) {
                        unsigned char out[21 * 9 * 4];
                        if (SUCCEEDED(IWICBitmapScaler_CopyPixels(sc, NULL, w * 4, w * h * 4, out))) {
                            /* a column impulse: row 0 across; a row impulse: column 0 down */
                            printf("w %d %dx%d %d %c%d", m, w, h, levels[lv], k < 8 ? 'x' : 'y', k < 8 ? k : k - 8);
                            int n = k < 8 ? w : h;
                            for (int i = 0; i < n; i++) printf(" %d", out[(k < 8 ? i : i * w) * 4]);
                            printf("\n");
                        }
                    }
                    if (sc) IWICBitmapScaler_Release(sc);
                    IWICBitmap_Release(b);
                }
}

/* 32bppCMYK -> 32bppBGRA over a 9-level grid of each ink: an 81x81 picture */
static void cmyk(void) {
    enum { N = 9, S = N * N };
    static unsigned char in[S * S * 4], out[S * S * 4];
    for (int i = 0; i < S * S; i++) {
        int c = i % N, m = i / N % N, y = i / (N * N) % N, k = i / (N * N * N);
        unsigned char v[4] = { (unsigned char)(c * 255 / (N - 1)), (unsigned char)(m * 255 / (N - 1)), (unsigned char)(y * 255 / (N - 1)), (unsigned char)(k * 255 / (N - 1)) };
        memcpy(in + i * 4, v, 4);
    }
    IWICBitmap *b = NULL;
    if (FAILED(IWICImagingFactory_CreateBitmapFromMemory(wic, S, S, &GUID_WICPixelFormat32bppCMYK, S * 4, sizeof in, in, &b))) { puts("cmyk bitmap failed"); return; }
    IWICFormatConverter *c = NULL;
    IWICImagingFactory_CreateFormatConverter(wic, &c);
    HRESULT hr = IWICFormatConverter_Initialize(c, (IWICBitmapSource *)b, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, NULL, 0, WICBitmapPaletteTypeCustom);
    printf("cmyk init %#lx\n", hr);
    if (SUCCEEDED(hr) && SUCCEEDED(IWICFormatConverter_CopyPixels(c, NULL, S * 4, sizeof out, out))) probe_image("cmyk_grid", out, S, S, S * 4, 0);
    IWICFormatConverter_Release(c);
    IWICBitmap_Release(b);
}

/* all 65536 levels of a 16-bit format (gray, or R=G=B with alpha 65535) as a 256x256 picture */
static void ramp16(const GUID *g, int channels, const char *name) {
    static unsigned short in[65536 * 4];
    static unsigned char out[65536 * 4];
    for (int i = 0; i < 65536; i++)
        for (int c = 0; c < channels; c++) in[i * channels + c] = c == 3 ? 65535 : (unsigned short)i;
    IWICBitmap *b = NULL;
    if (FAILED(IWICImagingFactory_CreateBitmapFromMemory(wic, 256, 256, g, 256 * 2 * channels, 65536 * 2 * channels, (BYTE *)in, &b))) { printf("%s bitmap failed\n", name); return; }
    IWICFormatConverter *c = NULL;
    IWICImagingFactory_CreateFormatConverter(wic, &c);
    HRESULT hr = IWICFormatConverter_Initialize(c, (IWICBitmapSource *)b, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, NULL, 0, WICBitmapPaletteTypeCustom);
    printf("%s init %#lx\n", name, hr);
    if (SUCCEEDED(hr) && SUCCEEDED(IWICFormatConverter_CopyPixels(c, NULL, 256 * 4, sizeof out, out))) probe_image(name, out, 256, 256, 256 * 4, 0);
    IWICFormatConverter_Release(c);
    IWICBitmap_Release(b);
}

/* CreateComponentInfo of each pixel format (AdvHD asks it for the bits per pixel of what it
 * decodes): IWICPixelFormatInfo2's answers; a GUID that is no component: not found */
static void format_infos(void) {
    for (unsigned i = 0; i < sizeof formats / sizeof *formats; i++) {
        IWICComponentInfo *ci = NULL;
        HRESULT hr = IWICImagingFactory_CreateComponentInfo(wic, formats[i].g, &ci);
        if (FAILED(hr)) { printf("info %s %#lx\n", formats[i].n, hr); continue; }
        IWICPixelFormatInfo2 *pi = NULL;
        HRESULT hq = IWICComponentInfo_QueryInterface(ci, &IID_IWICPixelFormatInfo2, (void **)&pi);
        WICComponentType ty = 0;
        IWICComponentInfo_GetComponentType(ci, &ty);
        GUID g = { 0 }, cl = { 0 };
        UINT bits = 0, ch = 0;
        BOOL tr = 7;
        WICPixelFormatNumericRepresentation num = 77;
        if (pi) {
            IWICPixelFormatInfo2_GetFormatGUID(pi, &g);
            IWICPixelFormatInfo2_GetCLSID(pi, &cl);
            IWICPixelFormatInfo2_GetBitsPerPixel(pi, &bits);
            IWICPixelFormatInfo2_GetChannelCount(pi, &ch);
            IWICPixelFormatInfo2_SupportsTransparency(pi, &tr);
            IWICPixelFormatInfo2_GetNumericRepresentation(pi, &num);
            IWICPixelFormatInfo2_Release(pi);
        }
        printf("info %s qi %#lx type %d guid %d clsid %d bits %u channels %u transparency %d numeric %d\n", formats[i].n, hq, ty,
               IsEqualGUID(&g, formats[i].g), IsEqualGUID(&cl, formats[i].g), bits, ch, tr, num);
        IWICComponentInfo_Release(ci);
    }
    static const GUID none = { 0x12345678, 0x1234, 0x5678, { 1, 2, 3, 4, 5, 6, 7, 8 } };
    IWICComponentInfo *ci = (void *)1;
    HRESULT hr = IWICImagingFactory_CreateComponentInfo(wic, &none, &ci);
    printf("info unknown %#lx out %d\n", hr, ci == NULL);
}

int main(void) {
    CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    HRESULT hr = CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&wic);
    printf("WICImagingFactory %#lx\n", hr);
    if (!wic) return 1;
    format_infos();
    conversions();
    palettes();
    flips();
    scaler_weights();
    cmyk();
    ramp16(&GUID_WICPixelFormat16bppGray, 1, "gray16_ramp");
    ramp16(&GUID_WICPixelFormat48bppRGB, 3, "rgb48_ramp");
    ramp16(&GUID_WICPixelFormat64bppRGBA, 4, "rgba64_ramp");
    return 0;
}
