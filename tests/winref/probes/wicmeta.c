/* WIC frame metadata readers as DirectXTex (linked into games) uses them: what
 * IWICBitmapFrameDecode::GetMetadataQueryReader answers per container (BMP's
 * WINCODEC_ERR_UNSUPPORTEDOPERATION is the one failure DirectXTex accepts), and for PNG the
 * reader's container, location, enumerator and GetMetadataByName of the sRGB, gAMA and tEXt
 * chunks in several spellings, absent names and a NULL value. Pictures: wicmeta_data.h
 * (wicmeta_gen.py).
 * libs: -lole32 -lwindowscodecs -loleaut32 -luuid */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <objbase.h>
#include <propvarutil.h>
#include <wincodec.h>
#include "wicmeta_data.h"

static IWICImagingFactory *wic;

static IWICBitmapFrameDecode *frame_of(const char *name, const unsigned char *data, unsigned size) {
    IWICStream *s = NULL;
    IWICBitmapDecoder *d = NULL;
    IWICBitmapFrameDecode *f = NULL;
    if (FAILED(IWICImagingFactory_CreateStream(wic, &s))) return NULL;
    IWICStream_InitializeFromMemory(s, (BYTE *)data, size);
    HRESULT hr = IWICImagingFactory_CreateDecoderFromStream(wic, (IStream *)s, NULL, WICDecodeMetadataCacheOnDemand, &d);
    IWICStream_Release(s);
    if (FAILED(hr)) { printf("%s: CreateDecoderFromStream %#lx\n", name, hr); return NULL; }
    hr = IWICBitmapDecoder_GetFrame(d, 0, &f);
    IWICBitmapDecoder_Release(d);
    if (FAILED(hr)) { printf("%s: GetFrame %#lx\n", name, hr); return NULL; }
    return f;
}

static const char *container(const GUID *g) {
    if (IsEqualGUID(g, &GUID_ContainerFormatPng)) return "png";
    if (IsEqualGUID(g, &GUID_ContainerFormatJpeg)) return "jpeg";
    if (IsEqualGUID(g, &GUID_ContainerFormatGif)) return "gif";
    if (IsEqualGUID(g, &GUID_ContainerFormatBmp)) return "bmp";
    return "other";
}

static void query(IWICMetadataQueryReader *r, const WCHAR *name) {
    PROPVARIANT v;
    PropVariantInit(&v);
    HRESULT hr = IWICMetadataQueryReader_GetMetadataByName(r, name, &v);
    printf("  %ls: %#lx vt %u", name, hr, v.vt);
    if (v.vt == VT_UI1) printf(" %u", v.bVal);
    else if (v.vt == VT_UI2) printf(" %u", v.uiVal);
    else if (v.vt == VT_UI4) printf(" %lu", (unsigned long)v.ulVal);
    else if (v.vt == VT_LPSTR) printf(" \"%s\"", v.pszVal);
    printf("\n");
    PropVariantClear(&v);
    printf("  %ls (no value): %#lx\n", name, IWICMetadataQueryReader_GetMetadataByName(r, name, NULL));
}

static void reader(const char *name, const unsigned char *data, unsigned size, int detail) {
    IWICBitmapFrameDecode *f = frame_of(name, data, size);
    if (!f) return;
    IWICMetadataQueryReader *r = NULL;
    HRESULT hr = IWICBitmapFrameDecode_GetMetadataQueryReader(f, &r);
    printf("%s: GetMetadataQueryReader %#lx %s\n", name, hr, r ? "reader" : "null");
    if (r) {
        GUID g;
        printf("  GetContainerFormat %#lx", IWICMetadataQueryReader_GetContainerFormat(r, &g));
        printf(" %s\n", container(&g));
        WCHAR loc[16];
        UINT n = 0;
        hr = IWICMetadataQueryReader_GetLocation(r, 0, NULL, &n);
        printf("  GetLocation size %#lx %u\n", hr, n);
        hr = IWICMetadataQueryReader_GetLocation(r, 16, loc, &n);
        printf("  GetLocation %#lx %u \"%ls\"\n", hr, n, SUCCEEDED(hr) ? loc : L"");
        if (detail) {
            IEnumString *e = NULL;
            hr = IWICMetadataQueryReader_GetEnumerator(r, &e);
            printf("  GetEnumerator %#lx\n", hr);
            if (e) {
                LPOLESTR s;
                ULONG got;
                while (IEnumString_Next(e, 1, &s, &got) == S_OK) {
                    printf("    %ls\n", s);
                    CoTaskMemFree(s);
                }
                IEnumString_Release(e);
            }
            query(r, L"/sRGB/RenderingIntent");
            query(r, L"/srgb/renderingintent");
            query(r, L"/gAMA/ImageGamma");
            query(r, L"/tEXt/{str=Software}");
            query(r, L"/tEXt/{str=Title}");
            query(r, L"/tEXt/{str=Author}");
            query(r, L"/iCCP/ProfileName");
            query(r, L"System.Image.ColorSpace");
        }
        IWICMetadataQueryReader_Release(r);
    }
    IWICBitmapFrameDecode_Release(f);
}

int main(void) {
    CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    HRESULT hr = CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&wic);
    printf("WICImagingFactory %#lx\n", hr);
    if (!wic) return 1;
    reader("png_plain", png_plain, sizeof png_plain, 1);
    reader("png_meta", png_meta, sizeof png_meta, 1);
    reader("bmp24", bmp24, sizeof bmp24, 0);
    reader("jpeg", jpeg, sizeof jpeg, 0);
    reader("gif", gif, sizeof gif, 0);
    IWICImagingFactory_Release(wic);
    return 0;
}
