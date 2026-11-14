/* The Source Reader on the sound files Media Foundation's other sources open (#85): FLAC
 * (Windows 10's FLAC source), WMA (the ASF source) and Ogg Vorbis (no source on a plain Windows:
 * MF_E_UNSUPPORTED_BYTESTREAM_TYPE there; sake opens it, tests/winref/known.txt). From the file's
 * URL: the native type, float PCM, the duration, every sample to the end (times back to back,
 * the frame count), the tone per channel, a seek to 250 ms.
 * run: browser (sake decodes FLAC only with a page)
 * libs: -lmfplat -lmfreadwrite -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <math.h>
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include "mfmore_data.h"

#define FIRST_AUDIO MF_SOURCE_READER_FIRST_AUDIO_STREAM

static void tone(const char *name, const float *v, DWORD frames, int ch, DWORD rate) {
    for (int c = 0; c < ch; c++) {
        int cross = 0;
        DWORD from = frames / 4, to = frames * 3 / 4;
        double sq = 0;
        for (DWORD i = from; i < to; i++) {
            if (i > from && (v[(i - 1) * ch + c] < 0) != (v[i * ch + c] < 0)) cross++;
            sq += (double)v[i * ch + c] * v[i * ch + c];
        }
        double hz = to > from ? cross / 2.0 * rate / (to - from) : 0, rms = to > from ? sqrt(sq / (to - from)) : 0;
        printf("  %s ch %d: %ld Hz, rms %ld/20\n", name, c, lround(hz / 10) * 10, lround(rms * 20));
    }
}

/* to the end: samples, frames (to the nearest 1/100 s), whether times run on, the tone */
static void read_all(const char *name, IMFSourceReader *r, int ch, DWORD rate, int show) {
    float *all = malloc(16 << 20);
    DWORD total = 0, n = 0, gaps = 0;
    LONGLONG first = -1, next = -1;
    for (;;) {
        IMFSample *s = NULL;
        DWORD idx, flags = 0;
        LONGLONG t = 0, d = 0;
        HRESULT hr = IMFSourceReader_ReadSample(r, FIRST_AUDIO, 0, &idx, &flags, &t, &s);
        if (FAILED(hr)) {
            printf("  %s read %#lx\n", name, hr);
            break;
        }
        if (!s) {
            if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
            continue;
        }
        IMFSample_GetSampleDuration(s, &d);
        if (first < 0) first = t;
        if (next >= 0 && llabs(t - next) > 10000) gaps++;
        next = t + d;
        IMFMediaBuffer *b;
        if (SUCCEEDED(IMFSample_ConvertToContiguousBuffer(s, &b))) {
            BYTE *p;
            DWORD len;
            IMFMediaBuffer_Lock(b, &p, NULL, &len);
            if (total * 4 + len <= (16u << 20)) memcpy((BYTE *)all + total * 4, p, len);
            total += len / 4;
            IMFMediaBuffer_Unlock(b);
            IMFMediaBuffer_Release(b);
        }
        IMFSample_Release(s);
        n++;
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
    }
    DWORD frames = total / ch;
    printf("  %s first at %ld ms, %ld0 ms of sound, any samples %d, times not back to back %lu\n", name, (long)(first / 10000), lround(frames * 100.0 / rate), n > 0, gaps);
    if (show) tone(name, all, frames, ch, rate);
    free(all);
}

static void one(const char *name, const unsigned char *d, DWORD len) {
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfmore-probe.%hs", dir, name);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, d, len, &w, NULL);
    CloseHandle(f);
    IMFSourceReader *r = NULL;
    HRESULT hr = MFCreateSourceReaderFromURL(path, NULL, &r);
    printf("%s: %#lx\n", name, hr);
    if (r) {
        IMFMediaType *t = NULL;
        hr = IMFSourceReader_GetNativeMediaType(r, FIRST_AUDIO, 0, &t);
        UINT32 ch = 0, rate = 0;
        if (t) {
            GUID sub = { 0 };
            BOOL comp = 0;
            IMFMediaType_GetGUID(t, &MF_MT_SUBTYPE, &sub);
            IMFMediaType_IsCompressedFormat(t, &comp);
            IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_NUM_CHANNELS, &ch);
            IMFMediaType_GetUINT32(t, &MF_MT_AUDIO_SAMPLES_PER_SECOND, &rate);
            printf("  %s native: subtype %08lx-%04x compressed %d ch %u rate %u\n", name, sub.Data1, sub.Data2, comp, ch, rate);
            IMFMediaType_Release(t);
        }
        MFCreateMediaType(&t);
        IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
        IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFAudioFormat_Float);
        hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_AUDIO, NULL, t);
        IMFMediaType_Release(t);
        printf("  %s set float: %#lx\n", name, hr);
        PROPVARIANT v;
        PropVariantInit(&v);
        hr = IMFSourceReader_GetPresentationAttribute(r, MF_SOURCE_READER_MEDIASOURCE, &MF_PD_DURATION, &v);
        printf("  %s duration %#lx %ld0 ms\n", name, hr, lround(v.uhVal.QuadPart / 1e5));
        if (ch && rate) {
            read_all(name, r, ch, rate, 1);
            PropVariantInit(&v);
            v.vt = VT_I8;
            v.hVal.QuadPart = 2500000;
            hr = IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &v);
            printf("  %s seek to 250 ms: %#lx\n", name, hr);
            read_all(name, r, ch, rate, 0);
        }
        IMFSourceReader_Release(r);
    }
    DeleteFileW(path);
}

int main(void) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if (FAILED(hr)) {
        printf("skip no Media Foundation (%#lx)\n", hr);
        return 0;
    }
    one("flac", file_flac, sizeof file_flac);
    one("wma", file_wma, sizeof file_wma);
    one("ogg", file_ogg, sizeof file_ogg);
    MFShutdown();
    return 0;
}
