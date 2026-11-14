/* Media Foundation's Direct3D 11 buffers (#31): MFCreateDXGISurfaceBuffer on a B8G8R8A8 texture
 * (IMFDXGIBuffer: GetResource, GetSubresourceIndex, Get/SetUnknown; IMFMediaBuffer Lock reading
 * the texture, Unlock writing changes back; IMF2DBuffer pitch top-down and bottom-up; refused
 * arguments), then a Source Reader given the device (MF_SOURCE_READER_D3D_MANAGER, advanced
 * video processing) reading mfwmv's movie as RGB32: whether its samples are textures, their
 * size and format, and the colour of each frame through Lock. WARP on the Windows runner.
 * libs: -lmfplat -lmfreadwrite -lmfuuid -ld3d11 -lole32 -luuid
 * run: browser (sake's D3D11 is WebGPU: tests/winref/run-browser.mjs) */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <d3d11.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include "mfwmv_data.h"

DEFINE_GUID(KEY, 0x12345678, 0x1111, 0x2222, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x66, 0x66);

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

/* the texel (x, y) of a texture, read back through a staging copy */
static DWORD texel(ID3D11Texture2D *t, int x, int y) {
    D3D11_TEXTURE2D_DESC d;
    ID3D11Texture2D_GetDesc(t, &d);
    d.Usage = D3D11_USAGE_STAGING;
    d.BindFlags = 0;
    d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    d.MiscFlags = 0;
    d.MipLevels = d.ArraySize = 1;
    ID3D11Texture2D *s = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(dev, &d, NULL, &s))) return 0xdead;
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)s, 0, 0, 0, 0, (ID3D11Resource *)t, 0, NULL);
    D3D11_MAPPED_SUBRESOURCE m;
    DWORD v = 0xbeef;
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)s, 0, D3D11_MAP_READ, 0, &m))) {
        v = ((DWORD *)((BYTE *)m.pData + y * m.RowPitch))[x];
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)s, 0);
    }
    ID3D11Texture2D_Release(s);
    return v;
}

static void surface_buffer(void) {
    DWORD px[8 * 4];
    for (int i = 0; i < 32; i++) px[i] = 0xff000000 | (i * 0x070503);
    D3D11_TEXTURE2D_DESC d = { 8, 4, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0 };
    D3D11_SUBRESOURCE_DATA init = { px, 32, 0 };
    ID3D11Texture2D *t = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(dev, &d, &init, &t);
    printf("texture: %#lx\n", hr);
    if (!t) return;
    IMFMediaBuffer *b = NULL;
    printf("bad riid: %#lx\n", MFCreateDXGISurfaceBuffer(&IID_IUnknown, (IUnknown *)t, 0, FALSE, &b));
    printf("subresource 1 of 1: %#lx\n", MFCreateDXGISurfaceBuffer(&IID_ID3D11Texture2D, (IUnknown *)t, 1, FALSE, &b));
    hr = MFCreateDXGISurfaceBuffer(&IID_ID3D11Texture2D, (IUnknown *)t, 0, FALSE, &b);
    printf("buffer: %#lx\n", hr);
    if (!b) return;
    DWORD max = 0, len = 0;
    IMFMediaBuffer_GetMaxLength(b, &max);
    IMFMediaBuffer_GetCurrentLength(b, &len);
    printf("  max %lu, current %lu\n", max, len);
    IMFDXGIBuffer *x = NULL;
    IMF2DBuffer *b2 = NULL;
    printf("  IMFDXGIBuffer %#lx, IMF2DBuffer %#lx\n", IMFMediaBuffer_QueryInterface(b, &IID_IMFDXGIBuffer, (void **)&x), IMFMediaBuffer_QueryInterface(b, &IID_IMF2DBuffer, (void **)&b2));
    if (x) {
        ID3D11Texture2D *r = NULL;
        UINT sub = 99;
        hr = IMFDXGIBuffer_GetResource(x, &IID_ID3D11Texture2D, (void **)&r);
        printf("  resource %#lx, the texture %d\n", hr, r == t);
        if (r) ID3D11Texture2D_Release(r);
        hr = IMFDXGIBuffer_GetSubresourceIndex(x, &sub);
        printf("  subresource %#lx %u\n", hr, sub);
        IUnknown *u = NULL;
        printf("  unknown not set: %#lx\n", IMFDXGIBuffer_GetUnknown(x, &KEY, &IID_IUnknown, (void **)&u));
        printf("  set: %#lx\n", IMFDXGIBuffer_SetUnknown(x, &KEY, (IUnknown *)t));
        printf("  set again: %#lx\n", IMFDXGIBuffer_SetUnknown(x, &KEY, (IUnknown *)t));
        hr = IMFDXGIBuffer_GetUnknown(x, &KEY, &IID_ID3D11Texture2D, (void **)&u);
        printf("  get: %#lx, the texture %d\n", hr, (void *)u == (void *)t);
        if (u) IUnknown_Release(u);
        printf("  set NULL: %#lx\n", IMFDXGIBuffer_SetUnknown(x, &KEY, NULL));
        u = NULL;
        printf("  get after: %#lx\n", IMFDXGIBuffer_GetUnknown(x, &KEY, &IID_IUnknown, (void **)&u));
    }
    BYTE *p = NULL;
    hr = IMFMediaBuffer_Lock(b, &p, &max, &len);
    printf("  lock %#lx, max %lu, current %lu\n", hr, max, len);
    if (p) {
        int same = 1;
        for (int i = 0; i < 32; i++)
            if (((DWORD *)p)[i] != px[i]) same = 0;
        printf("  the texture's bytes %d\n", same);
        ((DWORD *)p)[2 * 8 + 3] = 0xff123456;
        printf("  unlock %#lx\n", IMFMediaBuffer_Unlock(b));
        printf("  texel (3, 2) after: %#lx\n", texel(t, 3, 2));
    }
    printf("  unlock unlocked: %#lx\n", IMFMediaBuffer_Unlock(b));
    if (b2) {
        BYTE *s = NULL;
        LONG pitch = 0;
        BOOL cont = 2;
        DWORD clen = 0;
        printf("  scanline unlocked: %#lx\n", IMF2DBuffer_GetScanline0AndPitch(b2, &s, &pitch));
        hr = IMF2DBuffer_Lock2D(b2, &s, &pitch);
        printf("  lock2d %#lx, pitch %ld, row 1 texel 0 %#lx\n", hr, pitch, s ? *(DWORD *)(s + pitch) : 0);
        IMF2DBuffer_Unlock2D(b2);
        IMF2DBuffer_IsContiguousFormat(b2, &cont);
        IMF2DBuffer_GetContiguousLength(b2, &clen);
        printf("  contiguous %d, length %lu\n", cont, clen);
        IMF2DBuffer_Release(b2);
    }
    if (x) IMFDXGIBuffer_Release(x);
    IMFMediaBuffer_Release(b);
    b = NULL;
    MFCreateDXGISurfaceBuffer(&IID_ID3D11Texture2D, (IUnknown *)t, 0, TRUE, &b);
    if (b) {
        b2 = NULL;
        IMFMediaBuffer_QueryInterface(b, &IID_IMF2DBuffer, (void **)&b2);
        if (b2) {
            BYTE *s = NULL;
            LONG pitch = 0;
            hr = IMF2DBuffer_Lock2D(b2, &s, &pitch);
            printf("bottom-up lock2d %#lx, pitch %ld, scanline 0 texel 0 %#lx\n", hr, pitch, s ? *(DWORD *)s : 0);
            IMF2DBuffer_Unlock2D(b2);
            IMF2DBuffer_Release(b2);
        }
        IMFMediaBuffer_Release(b);
    }
    ID3D11Texture2D_Release(t);
}

static int classify_rgb(int r, int g, int b) {
    if (r > 180 && g < 80 && b < 80) return 'r';
    if (g > 180 && r < 80 && b < 80) return 'g';
    if (b > 180 && r < 80 && g < 80) return 'b';
    return '?';
}

static void reader(const WCHAR *path) {
    UINT token = 0;
    IMFDXGIDeviceManager *m = NULL;
    HRESULT hr = MFCreateDXGIDeviceManager(&token, &m);
    printf("manager: %#lx\n", hr);
    if (!m) return;
    printf("  reset: %#lx\n", IMFDXGIDeviceManager_ResetDevice(m, (IUnknown *)dev, token));
    IMFAttributes *a = NULL;
    MFCreateAttributes(&a, 2);
    IMFAttributes_SetUnknown(a, &MF_SOURCE_READER_D3D_MANAGER, (IUnknown *)m);
    IMFAttributes_SetUINT32(a, &MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, TRUE);
    IMFSourceReader *r = NULL;
    hr = MFCreateSourceReaderFromURL(path, a, &r);
    IMFAttributes_Release(a);
    printf("reader with the device: %#lx\n", hr);
    if (r) {
        IMFSourceReader_SetStreamSelection(r, (DWORD)MF_SOURCE_READER_ALL_STREAMS, FALSE);
        IMFSourceReader_SetStreamSelection(r, MF_SOURCE_READER_FIRST_VIDEO_STREAM, TRUE);
        IMFMediaType *t;
        MFCreateMediaType(&t);
        IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Video);
        IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
        hr = IMFSourceReader_SetCurrentMediaType(r, MF_SOURCE_READER_FIRST_VIDEO_STREAM, NULL, t);
        IMFMediaType_Release(t);
        printf("  set RGB32: %#lx\n", hr);
        char cols[40] = { 0 };
        int n = 0, textures = 0, sized = 0;
        DXGI_FORMAT fmt = 0;
        for (int k = 0; k < 40; k++) {
            IMFSample *s = NULL;
            DWORD idx = 0, flags = 0;
            LONGLONG ts = 0;
            hr = IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_VIDEO_STREAM, 0, &idx, &flags, &ts, &s);
            if (FAILED(hr)) {
                printf("  read %d failed %#lx\n", n, hr);
                break;
            }
            if (s) {
                IMFMediaBuffer *b = NULL;
                IMFSample_GetBufferByIndex(s, 0, &b);
                IMFDXGIBuffer *x = NULL;
                if (b && SUCCEEDED(IMFMediaBuffer_QueryInterface(b, &IID_IMFDXGIBuffer, (void **)&x))) {
                    ID3D11Texture2D *tx = NULL;
                    if (SUCCEEDED(IMFDXGIBuffer_GetResource(x, &IID_ID3D11Texture2D, (void **)&tx))) {
                        D3D11_TEXTURE2D_DESC d;
                        ID3D11Texture2D_GetDesc(tx, &d);
                        textures++;
                        if (d.Width == 64 && d.Height == 48) sized++;
                        fmt = d.Format;
                        ID3D11Texture2D_Release(tx);
                    }
                    IMFDXGIBuffer_Release(x);
                }
                BYTE *p = NULL;
                DWORD len = 0;
                int c = '?';
                if (b && SUCCEEDED(IMFMediaBuffer_Lock(b, &p, NULL, &len))) {
                    if (len >= 64 * 48 * 4) {
                        const BYTE *q = p + (24 * 64 + 32) * 4;
                        c = classify_rgb(q[2], q[1], q[0]);
                    }
                    IMFMediaBuffer_Unlock(b);
                }
                if (n < 39) cols[n] = c;
                n++;
                if (b) IMFMediaBuffer_Release(b);
                IMFSample_Release(s);
            }
            if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
        }
        printf("  %d frames %s, textures %d (64x48 %d, format %d)\n", n, cols, textures, sized, fmt);
        IMFSourceReader_Release(r);
    }
    IMFDXGIDeviceManager_Release(m);
}

int main(void) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if (FAILED(hr)) {
        printf("skip no Media Foundation (%#lx)\n", hr);
        return 0;
    }
    D3D_FEATURE_LEVEL want = D3D_FEATURE_LEVEL_11_0, got;
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, D3D11_CREATE_DEVICE_BGRA_SUPPORT, &want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    if (FAILED(hr)) {
        printf("skip D3D11CreateDevice %#lx\n", hr);
        return 0;
    }
    ID3D10Multithread *mt = NULL;
    if (SUCCEEDED(ID3D11Device_QueryInterface(dev, &IID_ID3D10Multithread, (void **)&mt))) {
        ID3D10Multithread_SetMultithreadProtected(mt, TRUE);
        ID3D10Multithread_Release(mt);
    }
    surface_buffer();
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfdxgi-probe.wmv", dir);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, file_wmv, sizeof file_wmv, &w, NULL);
    CloseHandle(f);
    reader(path);
    DeleteFileW(path);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
    MFShutdown();
    return 0;
}
