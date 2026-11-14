/* Media Foundation on an ASF movie (WMV2 + WMA2), as engines play a .wmv the MP4 way (#31): the
 * source resolver on the file's byte stream, the presentation descriptor and the streams' native
 * types (WMV2 with its codec data, WMA2), MFCreateSourceReaderFromMediaSource with NV12 out, every
 * frame to the end of the stream (presentation times, the colour of each frame), a seek, the reader
 * from the URL with RGB32 (video processing on), and the sound as float PCM to its end. The movie
 * (mfwmv_gen.sh) has a key frame every 6 frames. Colours are compared coarsely: r, g or b per frame.
 * libs: -lmfplat -lmfreadwrite -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include "mfwmv_data.h"

#define FIRST_VIDEO MF_SOURCE_READER_FIRST_VIDEO_STREAM
#define FIRST_AUDIO MF_SOURCE_READER_FIRST_AUDIO_STREAM

/* (not in mingw's mfidl.h) */
DEFINE_GUID(PD_MIME_TYPE, 0x6c990d33, 0xbb8e, 0x477a, 0x85, 0x98, 0x0d, 0x5d, 0x96, 0xfc, 0xd8, 0x8a);

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

static void audio_type(const char *what, IMFMediaType *t) {
    GUID sub = { 0 };
    UINT32 ch = 0, rate = 0, bits = 0, align = 0, avg = 0;
    UINT8 *ud = NULL;
    UINT32 udn = 0;
    IMFMediaType_GetGUID(t, &MF_MT_SUBTYPE, &sub);
    IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_NUM_CHANNELS, &ch);
    IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_SAMPLES_PER_SECOND, &rate);
    IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_BITS_PER_SAMPLE, &bits);
    IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_BLOCK_ALIGNMENT, &align);
    IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_AVG_BYTES_PER_SECOND, &avg);
    HRESULT hu = IMFMediaType_GetAllocatedBlob(t, &MF_MT_USER_DATA, &ud, &udn);
    printf("  %s: audio subtype %#lx %u ch %u Hz %u bits align %u avg %u, user data %s%u\n", what, sub.Data1, ch, rate, bits, align, avg, SUCCEEDED(hu) ? "" : "- ", udn);
    if (ud) CoTaskMemFree(ud);
}

static void native_wmv(IMFMediaType *t) {
    UINT32 n = 0;
    HRESULT hr = IMFMediaType_GetBlobSize(t, &MF_MT_USER_DATA, &n);
    printf("  codec data: %#lx %u bytes\n", hr, n);
}

static void resolver(const WCHAR *path) {
    IMFByteStream *bs = NULL;
    HRESULT hr = MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, path, &bs);
    printf("file: %#lx\n", hr);
    if (!bs) return;
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
        WCHAR *mime = NULL;
        if (pd) {
            IMFPresentationDescriptor_GetStreamDescriptorCount(pd, &n);
            IMFPresentationDescriptor_GetUINT64(pd, &MF_PD_DURATION, &dur);
        }
        /* (durations to 10 ms: the file's is a little over 750 ms) */
        printf("  descriptor: %#lx, %lu streams, %ld0 ms\n", hr, n, (long)(dur / 100000));
        HRESULT hm = pd ? IMFPresentationDescriptor_GetAllocatedString(pd, &PD_MIME_TYPE, &mime, NULL) : E_FAIL;
        printf("  mime: %#lx %ls\n", hm, mime ? mime : L"-");
        if (mime) CoTaskMemFree(mime);
        for (DWORD i = 0; i < n; i++) {
            BOOL sel = 0;
            IMFStreamDescriptor *sd = NULL;
            IMFPresentationDescriptor_GetStreamDescriptorByIndex(pd, i, &sel, &sd);
            IMFMediaTypeHandler *h = NULL;
            DWORD types = 0;
            GUID major = { 0 };
            IMFStreamDescriptor_GetMediaTypeHandler(sd, &h);
            IMFMediaTypeHandler_GetMajorType(h, &major);
            IMFMediaTypeHandler_GetMediaTypeCount(h, &types);
            printf("  stream %lu: selected %d, video %d, audio %d, types %lu\n", i, sel, IsEqualGUID(&major, &MFMediaType_Video), IsEqualGUID(&major, &MFMediaType_Audio), types);
            IMFMediaType *t = NULL;
            IMFMediaTypeHandler_GetMediaTypeByIndex(h, 0, &t);
            if (t) {
                if (IsEqualGUID(&major, &MFMediaType_Video)) {
                    type("handler's", t);
                    native_wmv(t);
                } else
                    audio_type("handler's", t);
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
            IMFSourceReader_SetStreamSelection(r, (DWORD)MF_SOURCE_READER_ALL_STREAMS, FALSE);
            IMFSourceReader_SetStreamSelection(r, FIRST_VIDEO, TRUE);
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
    IMFSourceReader_SetStreamSelection(r, FIRST_AUDIO, FALSE);
    frames("rgb32", r, 1, 40);
    /* the sound as float PCM */
    IMFSourceReader_SetStreamSelection(r, FIRST_VIDEO, FALSE);
    IMFSourceReader_SetStreamSelection(r, FIRST_AUDIO, TRUE);
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFAudioFormat_Float);
    hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_AUDIO, NULL, t);
    IMFMediaType_Release(t);
    printf("  set float: %#lx\n", hr);
    t = NULL;
    IMFSourceReader_GetCurrentMediaType(r, FIRST_AUDIO, &t);
    if (t) {
        audio_type("current", t);
        IMFMediaType_Release(t);
    }
    DWORD bytes = 0, reads = 0, flags = 0;
    float peak = 0;
    LONGLONG first = -1;
    while (reads < 1000) {
        IMFSample *s = NULL;
        DWORD idx = 0;
        LONGLONG ts = 0;
        hr = IMFSourceReader_ReadSample(r, FIRST_AUDIO, 0, &idx, &flags, &ts, &s);
        if (FAILED(hr)) {
            printf("  audio read failed %#lx\n", hr);
            break;
        }
        if (s) {
            if (first < 0) first = ts;
            IMFMediaBuffer *b = NULL;
            IMFSample_ConvertToContiguousBuffer(s, &b);
            BYTE *p = NULL;
            DWORD len = 0;
            IMFMediaBuffer_Lock(b, &p, NULL, &len);
            for (DWORD i = 0; i + 4 <= len; i += 4) {
                float v = *(float *)(p + i);
                if (v < 0) v = -v;
                if (v > peak) peak = v;
            }
            bytes += len;
            IMFMediaBuffer_Unlock(b);
            IMFMediaBuffer_Release(b);
            IMFSample_Release(s);
            reads++;
        }
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
    }
    /* (frames to 50 ms: decoders' delays differ) */
    printf("  audio: first at %ld0 ms, ~%lu x 50 ms of stereo, tone %d, end flags %#lx\n", (long)(first / 100000), (bytes / 8 + 1102) / 2205, peak > 0.05f && peak < 0.2f, flags);
    IMFSourceReader_Release(r);
}

int main(void) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if (FAILED(hr)) {
        printf("skip no Media Foundation (%#lx)\n", hr);
        return 0;
    }
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfwmv-probe.wmv", dir);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, file_wmv, sizeof file_wmv, &w, NULL);
    CloseHandle(f);
    resolver(path);
    url(path);
    DeleteFileW(path);
    MFShutdown();
    return 0;
}
