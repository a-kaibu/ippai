/* WIC CPU scaling: Fant/Cubic grow/shrink and a linear control, milliseconds per image.
 * Each iteration creates a scaler and reads every destination pixel, so Windows'
 * lazy scaler and sake's eager scaler both perform the complete operation.
 * Build: i686-w64-mingw32-gcc -O2 wic.c -o wic.exe -lole32 -lwindowscodecs -luuid */
#define COBJMACROS
#define INITGUID
#include "bench.h"
#include <wincodec.h>

int main(void)
{
    bench_args();
    unsigned sw = 512, sh = 384, w = 1024, h = 768;
    WICBitmapInterpolationMode mode = WICBitmapInterpolationModeFant;
    if (!strcmp(bench_case, "fant_shrink")) { w = 193; h = 137; }
    else if (!strcmp(bench_case, "cubic_grow")) mode = WICBitmapInterpolationModeCubic;
    else if (!strcmp(bench_case, "cubic_shrink")) { mode = WICBitmapInterpolationModeCubic; w = 193; h = 137; }
    else if (!strcmp(bench_case, "linear_grow")) mode = WICBitmapInterpolationModeLinear;
    else if (strcmp(bench_case, "fant_grow")) bench_fail("case: fant_grow / fant_shrink / cubic_grow / cubic_shrink / linear_grow");
    unsigned char *src = malloc(sw * sh * 4), *dst = malloc(w * h * 4);
    if (!src || !dst) bench_fail("allocation");
    for (unsigned i = 0; i < sw * sh; ++i) {
        src[i * 4] = (unsigned char)(i * 73);
        src[i * 4 + 1] = (unsigned char)(i * 29);
        src[i * 4 + 2] = (unsigned char)(i * 151);
        src[i * 4 + 3] = 255;
    }
    BCHECK(CoInitializeEx(NULL, COINIT_MULTITHREADED));
    IWICImagingFactory *factory = NULL;
    IWICBitmap *bitmap = NULL;
    BCHECK(CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER,
                           &IID_IWICImagingFactory, (void **)&factory));
    BCHECK(IWICImagingFactory_CreateBitmapFromMemory(factory, sw, sh, &GUID_WICPixelFormat32bppBGRA,
                                                    sw * 4, sw * sh * 4, src, &bitmap));
    int count = (int)(8 * bench_scale);
    if (count < 1) count = 1;
    if (w < sw) count *= 4;
    double begin = 0;
    // Warm up COM dispatch, the guest JIT and Wasm tiering before measuring.
    for (int i = -3; i < count; ++i) {
        if (i == 0) begin = now_ms();
        IWICBitmapScaler *scaler = NULL;
        BCHECK(IWICImagingFactory_CreateBitmapScaler(factory, &scaler));
        BCHECK(IWICBitmapScaler_Initialize(scaler, (IWICBitmapSource *)bitmap, w, h, mode));
        BCHECK(IWICBitmapScaler_CopyPixels(scaler, NULL, w * 4, w * h * 4, dst));
        IWICBitmapScaler_Release(scaler);
    }
    bench_report(bench_case, (now_ms() - begin) / count);
    unsigned checksum = 2166136261u;
    for (unsigned i = 0; i < w * h * 4; ++i) checksum = (checksum ^ dst[i]) * 16777619u;
    bench_check(bench_case, checksum);
    IWICBitmap_Release(bitmap);
    IWICImagingFactory_Release(factory);
    CoUninitialize();
    free(dst);
    free(src);
    bench_end();
    return 0;
}
