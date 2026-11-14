/* Media Foundation's video path as Unity's VideoPlayer (ニャンコイ, #227) and other engines play an
 * MP4 movie with it: the mfplat helpers they look up first (MFGetStrideForBitmapInfoHeader,
 * MFCalculateImageSize, MFAverageTimePerFrameToFrameRate, MFFrameRateToAverageTimePerFrame,
 * MFTEnumEx for an H.264 decoder, MFCreateAsyncResult + MFPutWorkItem + MFInvokeCallback, MFCreateFile),
 * then the source resolver on the file's byte stream, the presentation descriptor and the native
 * H.264 type, MFCreateSourceReaderFromMediaSource with NV12 out, every frame to the end of the
 * stream (presentation times, the colour of each frame), a seek, and the reader from the URL with
 * RGB32 (video processing on). The movie (mfvideo_gen.sh) has B-frames. Colours are compared
 * coarsely (decoders round differently): r, g or b per frame.
 * run: browser (sake decodes H.264 only with a page)
 * libs: -lmfplat -lmfreadwrite -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include <mftransform.h>
#include "mfvideo_data.h"

#define FIRST_VIDEO MF_SOURCE_READER_FIRST_VIDEO_STREAM

static void helpers(void) {
    static const struct { const char *name; DWORD fcc; } fmts[] = {
        { "NV12", MAKEFOURCC('N', 'V', '1', '2') }, { "YV12", MAKEFOURCC('Y', 'V', '1', '2') },
        { "I420", MAKEFOURCC('I', '4', '2', '0') }, { "YUY2", MAKEFOURCC('Y', 'U', 'Y', '2') },
        { "UYVY", MAKEFOURCC('U', 'Y', 'V', 'Y') }, { "RGB32", 22 }, { "ARGB32", 21 }, { "RGB24", 20 },
        { "RGB565", 23 }, { "L8", 50 }, { "junk", MAKEFOURCC('J', 'U', 'N', 'K') },
    };
    static const DWORD widths[] = { 1, 3, 64, 65, 1920 };
    for (int i = 0; i < sizeof fmts / sizeof *fmts; i++) {
        printf("stride %s:", fmts[i].name);
        for (int j = 0; j < 5; j++) {
            LONG s = 0;
            HRESULT hr = MFGetStrideForBitmapInfoHeader(fmts[i].fcc, widths[j], &s);
            if (FAILED(hr)) printf(" %#lx", hr);
            else printf(" %ld", s);
        }
        GUID sub = MFVideoFormat_Base;
        sub.Data1 = fmts[i].fcc;
        UINT32 size = 0;
        HRESULT hr = MFCalculateImageSize(&sub, 65, 33, &size);
        printf("; size 65x33 %#lx %u\n", hr, size);
    }
    static const UINT64 avgs[] = { 0, 1, 166833, 166667, 333667, 333333, 400000, 416666, 416667, 417083, 200000, 1000000, 10000000, 12345, 83333 };
    printf("frame rate of:");
    for (int i = 0; i < sizeof avgs / sizeof *avgs; i++) {
        UINT32 n = 0, d = 0;
        HRESULT hr = MFAverageTimePerFrameToFrameRate(avgs[i], &n, &d);
        printf(" %llu=%#lx:%u/%u", (unsigned long long)avgs[i], hr, n, d);
    }
    printf("\n");
    static const UINT32 rates[][2] = { { 30000, 1001 }, { 24000, 1001 }, { 60000, 1001 }, { 24, 1 }, { 25, 1 }, { 30, 1 }, { 60, 1 }, { 7, 3 }, { 1, 0 }, { 0, 1 } };
    printf("time per frame of:");
    for (int i = 0; i < sizeof rates / sizeof *rates; i++) {
        UINT64 t = 0;
        HRESULT hr = MFFrameRateToAverageTimePerFrame(rates[i][0], rates[i][1], &t);
        printf(" %u/%u=%#lx:%llu", rates[i][0], rates[i][1], hr, (unsigned long long)t);
    }
    printf("\n");
    MFT_REGISTER_TYPE_INFO in = { MFMediaType_Video, MFVideoFormat_H264 };
    IMFActivate **list = NULL;
    UINT32 count = 0;
    HRESULT hr = MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, &in, NULL, &list, &count);
    printf("H.264 decoders: %#lx, any %d\n", hr, count > 0);
    for (UINT32 i = 0; i < count; i++) IMFActivate_Release(list[i]);
    CoTaskMemFree(list);
    in.guidSubtype = MFVideoFormat_Base;
    in.guidSubtype.Data1 = MAKEFOURCC('J', 'U', 'N', 'K');
    list = NULL;
    count = 7;
    hr = MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SYNCMFT, &in, NULL, &list, &count);
    printf("JUNK decoders: %#lx, %u, list %d\n", hr, count, list != NULL);
    CoTaskMemFree(list);
}

/* an IMFAsyncCallback: Invoke counts, notes the thread and its result's status and state */
typedef struct {
    IMFAsyncCallback iface;
    LONG refs;
    HANDLE done;
    volatile LONG calls;
    DWORD thread;
    HRESULT status;
    IUnknown *state, *object;
} Cb;
static HRESULT WINAPI cb_qi(IMFAsyncCallback *t, REFIID iid, void **o) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IMFAsyncCallback)) {
        *o = t;
        IMFAsyncCallback_AddRef(t);
        return S_OK;
    }
    *o = NULL;
    return E_NOINTERFACE;
}
static ULONG WINAPI cb_add(IMFAsyncCallback *t) { return InterlockedIncrement(&((Cb *)t)->refs); }
static ULONG WINAPI cb_rel(IMFAsyncCallback *t) { return InterlockedDecrement(&((Cb *)t)->refs); }
static HRESULT WINAPI cb_params(IMFAsyncCallback *t, DWORD *f, DWORD *q) { return E_NOTIMPL; }
static HRESULT WINAPI cb_invoke(IMFAsyncCallback *t, IMFAsyncResult *r) {
    Cb *c = (Cb *)t;
    c->thread = GetCurrentThreadId();
    c->status = IMFAsyncResult_GetStatus(r);
    IUnknown *s = NULL, *o = NULL;
    IMFAsyncResult_GetState(r, &s);
    c->state = s;
    if (s) IUnknown_Release(s);
    if (SUCCEEDED(IMFAsyncResult_GetObject(r, &o)) && o) IUnknown_Release(o);
    c->object = o;
    InterlockedIncrement(&c->calls);
    SetEvent(c->done);
    return S_OK;
}
static IMFAsyncCallbackVtbl cb_vt = { cb_qi, cb_add, cb_rel, cb_params, cb_invoke };

static void async(void) {
    Cb c = { { &cb_vt }, 1, CreateEventW(NULL, FALSE, FALSE, NULL) };
    IMFAsyncResult *r = NULL;
    IUnknown *state = (IUnknown *)&c.iface;
    HRESULT hr = MFCreateAsyncResult(NULL, &c.iface, state, &r);
    printf("async result: %#lx, status %#lx\n", hr, r ? IMFAsyncResult_GetStatus(r) : 0);
    if (!r) return;
    IUnknown *o = (IUnknown *)1;
    printf("  no object: %#lx %d\n", IMFAsyncResult_GetObject(r, &o), o == NULL);
    printf("  state object: %d\n", IMFAsyncResult_GetStateNoAddRef(r) == state);
    IMFAsyncResult_SetStatus(r, E_FAIL);
    hr = MFInvokeCallback(r);
    DWORD w = WaitForSingleObject(c.done, 5000);
    printf("  invoke: %#lx, called %ld, on another thread %d, status %#lx, state %d\n", hr, c.calls, w == 0 && c.thread != GetCurrentThreadId(), c.status, c.state == state);
    IMFAsyncResult_Release(r);
    c.calls = 0;
    hr = MFPutWorkItem(MFASYNC_CALLBACK_QUEUE_STANDARD, &c.iface, NULL);
    w = WaitForSingleObject(c.done, 5000);
    printf("  work item: %#lx, called %ld, on another thread %d, status %#lx, state %d\n", hr, c.calls, w == 0 && c.thread != GetCurrentThreadId(), c.status, c.state == NULL);
    CloseHandle(c.done);
}

static int classify(int y, int u, int v) {
    /* BT.601 studio range: red Y 81 U 90 V 240, lime 145 54 34, blue 41 240 110 */
    if (v > 180) return 'r';
    if (u > 180) return 'b';
    if (y > 100 && u < 100 && v < 100) return 'g';
    return '?';
}
static int classify_rgb(int r, int g, int b) {
    if (r > 180 && g < 80 && b < 80) return 'r';
    if (g > 180 && r < 80 && b < 80) return 'g';
    if (b > 180 && r < 80 && g < 80) return 'b';
    return '?';
}

static void type(const char *what, IMFMediaType *t) {
    GUID major = { 0 }, sub = { 0 };
    UINT64 size = 0, rate = 0, ratio = 0;
    UINT32 stride = 0, inter = 0, indep = 0;
    IMFMediaType_GetMajorType(t, &major);
    IMFMediaType_GetGUID(t, &MF_MT_SUBTYPE, &sub);
    IMFMediaType_GetUINT64(t, &MF_MT_FRAME_SIZE, &size);
    IMFMediaType_GetUINT64(t, &MF_MT_FRAME_RATE, &rate);
    HRESULT hs = IMFMediaType_GetUINT32(t, &MF_MT_DEFAULT_STRIDE, &stride);
    IMFMediaType_GetUINT32(t, &MF_MT_INTERLACE_MODE, &inter);
    HRESULT hr = IMFMediaType_GetUINT64(t, &MF_MT_PIXEL_ASPECT_RATIO, &ratio);
    char fcc[5] = { 0 };
    memcpy(fcc, &sub.Data1, 4);
    for (int i = 0; i < 4; i++)
        if (fcc[i] < ' ') fcc[i] = '.';
    printf("  %s: video %d subtype %s (%#lx) %ux%u rate %u/%u stride %s%ld aspect %s%u:%u interlace %u\n", what, IsEqualGUID(&major, &MFMediaType_Video),
           sub.Data1 < 256 ? "" : fcc, sub.Data1, (UINT32)(size >> 32), (UINT32)size, (UINT32)(rate >> 32), (UINT32)rate,
           SUCCEEDED(hs) ? "" : "- ", SUCCEEDED(hs) ? (LONG)stride : 0, SUCCEEDED(hr) ? "" : "- ", (UINT32)(ratio >> 32), (UINT32)ratio, inter);
}

/* every frame of the stream: its presentation time in ms, its colour; rgb: RGB32 out */
static void frames(const char *what, IMFSourceReader *r, int rgb, int max) {
    char cols[64] = { 0 };
    int n = 0, gaps = 0, bad = 0;
    LONGLONG first = -1, prev = -1, last = 0;
    DWORD eos = 0;
    while (n < max) {
        IMFSample *s = NULL;
        DWORD idx = 99, flags = 0;
        LONGLONG ts = 0;
        HRESULT hr = IMFSourceReader_ReadSample(r, FIRST_VIDEO, 0, &idx, &flags, &ts, &s);
        if (FAILED(hr)) {
            printf("  %s: read %d failed %#lx\n", what, n, hr);
            break;
        }
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) eos = flags;
        if (!s) {
            if (eos) break;
            continue;
        }
        if (first < 0) first = ts;
        if (prev >= 0 && (ts - prev < 400000 || ts - prev > 434000)) gaps++;
        prev = last = ts;
        LONGLONG dur = 0;
        IMFSample_GetSampleDuration(s, &dur);
        if (n == 0) printf("  %s: first sample at %ld ms, %ld ms long, stream %lu\n", what, (long)(ts / 10000), (long)(dur / 10000), idx);
        IMFMediaBuffer *b = NULL;
        IMFSample_ConvertToContiguousBuffer(s, &b);
        BYTE *p = NULL;
        DWORD len = 0;
        IMFMediaBuffer_Lock(b, &p, NULL, &len);
        int c;
        if (rgb) {
            if (len < 64 * 48 * 4) bad++;
            const BYTE *q = p + (24 * 64 + 32) * 4;
            c = len >= 64 * 48 * 4 ? classify_rgb(q[2], q[1], q[0]) : '?';
        } else {
            if (len < 64 * 48 * 3 / 2) bad++;
            const BYTE *uv = p + 64 * 48 + 12 * 64 + 32;
            c = len >= 64 * 48 * 3 / 2 ? classify(p[24 * 64 + 32], uv[0], uv[1]) : '?';
        }
        if (n < 63) cols[n] = c;
        IMFMediaBuffer_Unlock(b);
        IMFMediaBuffer_Release(b);
        IMFSample_Release(s);
        n++;
        if (eos) break;
    }
    printf("  %s: %d frames %s, last at %ld ms, uneven steps %d, short buffers %d, end flags %#lx\n", what, n, cols, (long)(last / 10000), gaps, bad, eos);
}

static void nv12(IMFSourceReader *r) {
    IMFMediaType *t = NULL;
    HRESULT hr = IMFSourceReader_GetNativeMediaType(r, FIRST_VIDEO, 0, &t);
    printf("  native: %#lx\n", hr);
    if (t) {
        type("native", t);
        IMFMediaType_Release(t);
    }
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Video);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFVideoFormat_NV12);
    hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_VIDEO, NULL, t);
    IMFMediaType_Release(t);
    printf("  set NV12: %#lx\n", hr);
    t = NULL;
    IMFSourceReader_GetCurrentMediaType(r, FIRST_VIDEO, &t);
    if (t) {
        type("current", t);
        IMFMediaType_Release(t);
    }
}

static void resolver(const WCHAR *path) {
    IMFByteStream *bs = NULL;
    HRESULT hr = MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, path, &bs);
    printf("file: %#lx\n", hr);
    if (!bs) return;
    QWORD len = 0, pos = 0;
    DWORD caps = 0, got = 0;
    BYTE head[8] = { 0 };
    IMFByteStream_GetLength(bs, &len);
    IMFByteStream_GetCapabilities(bs, &caps);
    IMFByteStream_Read(bs, head, 8, &got);
    IMFByteStream_GetCurrentPosition(bs, &pos);
    printf("  length %d, readable %d seekable %d, read %lu: '%.4s', at %lu\n", len == sizeof file_mp4, !!(caps & MFBYTESTREAM_IS_READABLE), !!(caps & MFBYTESTREAM_IS_SEEKABLE), got, head + 4, (DWORD)pos);
    IMFByteStream_SetCurrentPosition(bs, 0);
    IMFSourceResolver *res = NULL;
    hr = MFCreateSourceResolver(&res);
    MF_OBJECT_TYPE ot = MF_OBJECT_INVALID;
    IUnknown *obj = NULL;
    if (res) hr = IMFSourceResolver_CreateObjectFromByteStream(res, bs, path, MF_RESOLUTION_MEDIASOURCE, NULL, &ot, &obj);
    printf("resolver: %#lx, media source %d\n", hr, ot == MF_OBJECT_MEDIASOURCE);
    IMFMediaSource *src = NULL;
    if (obj) IUnknown_QueryInterface(obj, &IID_IMFMediaSource, (void **)&src);
    if (src) {
        IMFPresentationDescriptor *pd = NULL;
        hr = IMFMediaSource_CreatePresentationDescriptor(src, &pd);
        DWORD n = 0;
        UINT64 dur = 0;
        if (pd) {
            IMFPresentationDescriptor_GetStreamDescriptorCount(pd, &n);
            IMFPresentationDescriptor_GetUINT64(pd, &MF_PD_DURATION, &dur);
        }
        printf("  descriptor: %#lx, %lu streams, %ld ms\n", hr, n, (long)(dur / 10000));
        for (DWORD i = 0; i < n; i++) {
            BOOL sel = 0;
            IMFStreamDescriptor *sd = NULL;
            IMFPresentationDescriptor_GetStreamDescriptorByIndex(pd, i, &sel, &sd);
            IMFMediaTypeHandler *h = NULL;
            DWORD id = 0, types = 0;
            GUID major = { 0 };
            IMFStreamDescriptor_GetStreamIdentifier(sd, &id);
            IMFStreamDescriptor_GetMediaTypeHandler(sd, &h);
            IMFMediaTypeHandler_GetMajorType(h, &major);
            IMFMediaTypeHandler_GetMediaTypeCount(h, &types);
            printf("  stream %lu: selected %d, video %d, types %lu\n", i, sel, IsEqualGUID(&major, &MFMediaType_Video), types);
            IMFMediaType *t = NULL;
            IMFMediaTypeHandler_GetMediaTypeByIndex(h, 0, &t);
            if (t) {
                type("handler's", t);
                IMFMediaType_Release(t);
            }
            IMFMediaTypeHandler_Release(h);
            IMFStreamDescriptor_Release(sd);
        }
        if (pd) IMFPresentationDescriptor_Release(pd);
        IMFSourceReader *r = NULL;
        hr = MFCreateSourceReaderFromMediaSource(src, NULL, &r);
        printf("reader on the source: %#lx\n", hr);
        if (r) {
            nv12(r);
            frames("nv12", r, 0, 40);
            PROPVARIANT at;
            PropVariantInit(&at);
            at.vt = VT_I8;
            at.hVal.QuadPart = 4000000; /* 400 ms: frame 9.6, after the key frame at 250 ms */
            hr = IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &at);
            printf("  seek to 400 ms: %#lx\n", hr);
            frames("after the seek", r, 0, 40);
            IMFSourceReader_Release(r);
        }
        IMFMediaSource_Shutdown(src);
        IMFMediaSource_Release(src);
    }
    if (obj) IUnknown_Release(obj);
    if (res) IMFSourceResolver_Release(res);
    IMFByteStream_Close(bs);
    IMFByteStream_Release(bs);
}

static void url(const WCHAR *path) {
    IMFAttributes *a = NULL;
    MFCreateAttributes(&a, 1);
    IMFAttributes_SetUINT32(a, &MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, TRUE);
    IMFSourceReader *r = NULL;
    HRESULT hr = MFCreateSourceReaderFromURL(path, a, &r);
    IMFAttributes_Release(a);
    printf("reader from the URL: %#lx\n", hr);
    if (!r) return;
    IMFMediaType *t;
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Video);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
    hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_VIDEO, NULL, t);
    IMFMediaType_Release(t);
    printf("  set RGB32: %#lx\n", hr);
    t = NULL;
    IMFSourceReader_GetCurrentMediaType(r, FIRST_VIDEO, &t);
    if (t) {
        type("current", t);
        IMFMediaType_Release(t);
    }
    PROPVARIANT v;
    PropVariantInit(&v);
    hr = IMFSourceReader_GetPresentationAttribute(r, MF_SOURCE_READER_MEDIASOURCE, &MF_PD_DURATION, &v);
    printf("  duration %#lx vt %d %ld ms\n", hr, v.vt, (long)(v.uhVal.QuadPart / 10000));
    if (SUCCEEDED(hr)) frames("rgb32", r, 1, 40);
    IMFSourceReader_Release(r);
}

int main(void) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if (FAILED(hr)) {
        printf("skip no Media Foundation (%#lx)\n", hr);
        return 0;
    }
    helpers();
    async();
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfvideo-probe.mp4", dir);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, file_mp4, sizeof file_mp4, &w, NULL);
    CloseHandle(f);
    IMFByteStream *none = NULL;
    printf("missing file: %#lx\n", MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, L"C:\\no-such-dir\\none.mp4", &none));
    resolver(path);
    url(path);
    DeleteFileW(path);
    MFShutdown();
    return 0;
}
