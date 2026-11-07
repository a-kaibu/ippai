/* Media Foundation's Source Reader as games decode music and voices with it (Microsoft's "decoding
 * audio" tutorial): MFCreateSourceReaderFromURL on an M4A (AAC), an MP3 and a WAVE written to the
 * temp folder, and FromByteStream on an IMFByteStream of the probe's own over the MP3's bytes;
 * stream selection, the native type, a PCM (then float) current type, the WAVEFORMATEX of it,
 * the duration and seeking, ReadSample to the end of the stream. The decoders' samples are not
 * equal to the bit, so the sound is checked by what it is: per channel the tone's frequency and
 * loudness, coarsely, and lengths to the decoder's frame.
 * run: browser (sake decodes AAC and MP3 only with a page)
 * libs: -lmfplat -lmfreadwrite -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <math.h>
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include "mfreader_data.h"

#define FIRST_AUDIO MF_SOURCE_READER_FIRST_AUDIO_STREAM

static void shape(const char *name, const BYTE *b, DWORD bytes, int ch, int bits, int fl, DWORD rate) {
    int frames = bytes / (ch * bits / 8);
    for (int c = 0; c < ch; c++) {
        int cross = 0, from = frames / 4, to = frames * 3 / 4;
        double sq = 0, prev = 0;
        for (int i = from; i < to; i++) {
            double v = fl ? ((const float *)b)[i * ch + c] : ((const short *)b)[i * ch + c] / 32768.0;
            if (i > from && (prev < 0) != (v < 0)) cross++;
            prev = v;
            sq += v * v;
        }
        double hz = to > from ? cross / 2.0 * rate / (to - from) : 0, rms = to > from ? sqrt(sq / (to - from)) : 0;
        printf("  %s ch %d: %ld Hz, rms %ld/20\n", name, c, lround(hz / 10) * 10, lround(rms * 20));
    }
}

static void attr(IMFMediaType *t, const char *name, REFGUID k) {
    UINT32 v;
    HRESULT hr = IMFMediaType_GetUINT32(t, k, &v);
    if (SUCCEEDED(hr)) printf(" %s %u", name, v);
    else printf(" %s -", name);
}

static void type(const char *what, IMFMediaType *t, int avg) {
    GUID major, sub;
    BOOL comp = 0;
    UINT32 n = 0;
    IMFMediaType_GetMajorType(t, &major);
    IMFMediaType_GetGUID(t, &MF_MT_SUBTYPE, &sub);
    IMFMediaType_IsCompressedFormat(t, &comp);
    printf("  %s: audio %d subtype %#lx compressed %d", what, IsEqualGUID(&major, &MFMediaType_Audio), sub.Data1, comp);
    attr(t, "ch", &MF_MT_AUDIO_NUM_CHANNELS);
    attr(t, "rate", &MF_MT_AUDIO_SAMPLES_PER_SECOND);
    attr(t, "bits", &MF_MT_AUDIO_BITS_PER_SAMPLE);
    attr(t, "align", &MF_MT_AUDIO_BLOCK_ALIGNMENT);
    if (avg) attr(t, "avg", &MF_MT_AUDIO_AVG_BYTES_PER_SECOND);
    attr(t, "independent", &MF_MT_ALL_SAMPLES_INDEPENDENT);
    puts("");
}

/* everything from the current position to the end: samples, frames, the sound */
static void read_all(const char *name, IMFSourceReader *r, int ch, int bits, int fl, DWORD rate, DWORD per) {
    BYTE *all = malloc(16 << 20);
    DWORD total = 0, n = 0, flags = 0, idx = 0;
    LONGLONG first = -1, last = 0, t;
    DWORD first_len = 0;
    for (;;) {
        IMFSample *s = NULL;
        HRESULT hr = IMFSourceReader_ReadSample(r, FIRST_AUDIO, 0, &idx, &flags, &t, &s);
        if (FAILED(hr)) { printf("  read %#lx\n", hr); break; }
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) {
            printf("  end of stream: flags %#lx sample %d\n", flags, s != NULL);
            if (s) IMFSample_Release(s);
            break;
        }
        if (!s) { printf("  no sample, flags %#lx\n", flags); continue; }
        if (first < 0) first = t;
        last = t;
        IMFMediaBuffer *b;
        if (SUCCEEDED(IMFSample_ConvertToContiguousBuffer(s, &b))) {
            BYTE *p;
            DWORD len;
            IMFMediaBuffer_Lock(b, &p, NULL, &len);
            if (total + len <= (16u << 20)) memcpy(all + total, p, len);
            if (!n) first_len = len;
            total += len;
            IMFMediaBuffer_Unlock(b);
            IMFMediaBuffer_Release(b);
        }
        IMFSample_Release(s);
        n++;
    }
    int frames = total / (ch * bits / 8);
    printf("  %s: first at %ld ms, %ld frames of %lu, last sample before %ld ms\n", name, (long)(first / 10000), lround((double)frames / per), per, lround((last + 1) / 1e4 / 50) * 50);
    /* (how the sound comes in samples: their count, the first one's length in frames of the codec) */
    printf("  %s: %lu samples, the first %ld frames of %lu\n", name, n, lround((double)first_len / (ch * bits / 8) / per), per);
    shape(name, all, total, ch, bits, fl, rate);
    free(all);
}

static void open_one(const char *name, IMFSourceReader *r, DWORD per) {
    BOOL sel = 0;
    HRESULT hr = IMFSourceReader_SetStreamSelection(r, MF_SOURCE_READER_ALL_STREAMS, FALSE);
    hr |= IMFSourceReader_SetStreamSelection(r, FIRST_AUDIO, TRUE);
    IMFSourceReader_GetStreamSelection(r, FIRST_AUDIO, &sel);
    printf("%s: select %#lx, selected %d; stream 5 %#lx\n", name, hr, sel, IMFSourceReader_GetStreamSelection(r, 5, &sel));

    IMFMediaType *nt = NULL, *t = NULL, *cur = NULL;
    hr = IMFSourceReader_GetNativeMediaType(r, FIRST_AUDIO, 0, &nt);
    printf("  native %#lx\n", hr);
    if (nt) {
        type("native", nt, 0);
        IMFMediaType_Release(nt);
    }
    printf("  native 1: %#lx\n", IMFSourceReader_GetNativeMediaType(r, FIRST_AUDIO, 1, &nt));

    PROPVARIANT v;
    PropVariantInit(&v);
    hr = IMFSourceReader_GetPresentationAttribute(r, MF_SOURCE_READER_MEDIASOURCE, &MF_PD_DURATION, &v);
    printf("  duration %#lx vt %u %ld ms\n", hr, v.vt, (long)lround(v.uhVal.QuadPart / 1e4 / 10) * 10);

    /* 16-bit PCM */
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFAudioFormat_PCM);
    hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_AUDIO, NULL, t);
    printf("  set PCM %#lx\n", hr);
    IMFMediaType_Release(t);
    hr = IMFSourceReader_GetCurrentMediaType(r, FIRST_AUDIO, &cur);
    if (FAILED(hr)) { printf("  current %#lx\n", hr); return; }
    type("current", cur, 1);
    WAVEFORMATEX *w = NULL;
    UINT32 wsize = 0;
    hr = MFCreateWaveFormatExFromMFMediaType(cur, &w, &wsize, 0);
    if (SUCCEEDED(hr)) printf("  wave: size %u tag %#x ch %u rate %lu avg %lu align %u bits %u extra %u\n", wsize, w->wFormatTag, w->nChannels, w->nSamplesPerSec, w->nAvgBytesPerSec, w->nBlockAlign, w->wBitsPerSample, w->cbSize);
    else printf("  wave %#lx\n", hr);
    int ch = w ? w->nChannels : 1;
    DWORD rate = w ? w->nSamplesPerSec : 1;
    CoTaskMemFree(w);
    IMFMediaType_Release(cur);
    read_all("pcm", r, ch, 16, 0, rate, per);

    /* a rate it does not have */
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFAudioFormat_PCM);
    IMFMediaType_SetUINT32(t, &MF_MT_AUDIO_SAMPLES_PER_SECOND, rate == 8000 ? 11025 : 8000);
    printf("  set PCM at another rate: failed %d\n", FAILED(IMFSourceReader_SetCurrentMediaType(r, FIRST_AUDIO, NULL, t)));
    IMFMediaType_Release(t);

    /* float, from 0.25 s */
    MFCreateMediaType(&t);
    IMFMediaType_SetGUID(t, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
    IMFMediaType_SetGUID(t, &MF_MT_SUBTYPE, &MFAudioFormat_Float);
    hr = IMFSourceReader_SetCurrentMediaType(r, FIRST_AUDIO, NULL, t);
    printf("  set float %#lx\n", hr);
    IMFMediaType_Release(t);
    if (SUCCEEDED(IMFSourceReader_GetCurrentMediaType(r, FIRST_AUDIO, &cur))) {
        type("current", cur, 1);
        IMFMediaType_Release(cur);
    }
    PropVariantClear(&v);
    v.vt = VT_I8;
    v.hVal.QuadPart = 2500000;
    hr = IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &v);
    printf("  seek %#lx\n", hr);
    DWORD idx, flags;
    LONGLONG ts;
    IMFSample *s = NULL;
    hr = IMFSourceReader_ReadSample(r, FIRST_AUDIO, 0, &idx, &flags, &ts, &s);
    printf("  after the seek: %#lx flags %#lx, at or before 250 ms: %d, within 100 ms of it: %d\n", hr, flags, ts <= 2500000, ts > 1500000);
    if (s) IMFSample_Release(s);
}

/* An IMFByteStream over bytes in memory (as games wrap their archives) */
typedef struct { IMFByteStream b; LONG refs; const BYTE *d; QWORD len, pos; ULONG got; } Mem;
static HRESULT WINAPI m_qi(IMFByteStream *t, REFIID iid, void **o) {
    if (IsEqualGUID(iid, &IID_IUnknown) || IsEqualGUID(iid, &IID_IMFByteStream)) { *o = t; IMFByteStream_AddRef(t); return S_OK; }
    *o = NULL;
    return E_NOINTERFACE;
}
static ULONG WINAPI m_add(IMFByteStream *t) { return InterlockedIncrement(&((Mem *)t)->refs); }
static ULONG WINAPI m_rel(IMFByteStream *t) { return InterlockedDecrement(&((Mem *)t)->refs); }
static HRESULT WINAPI m_caps(IMFByteStream *t, DWORD *c) { *c = MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_SEEKABLE; return S_OK; }
static HRESULT WINAPI m_len(IMFByteStream *t, QWORD *l) { *l = ((Mem *)t)->len; return S_OK; }
static HRESULT WINAPI m_setlen(IMFByteStream *t, QWORD l) { return E_NOTIMPL; }
static HRESULT WINAPI m_getpos(IMFByteStream *t, QWORD *p) { *p = ((Mem *)t)->pos; return S_OK; }
static HRESULT WINAPI m_setpos(IMFByteStream *t, QWORD p) { ((Mem *)t)->pos = p; return S_OK; }
static HRESULT WINAPI m_eos(IMFByteStream *t, BOOL *e) { *e = ((Mem *)t)->pos >= ((Mem *)t)->len; return S_OK; }
static HRESULT WINAPI m_read(IMFByteStream *t, BYTE *b, ULONG n, ULONG *got) {
    Mem *m = (Mem *)t;
    ULONG k = m->pos >= m->len ? 0 : (ULONG)min((QWORD)n, m->len - m->pos);
    memcpy(b, m->d + m->pos, k);
    m->pos += k;
    *got = k;
    return S_OK;
}
static HRESULT WINAPI m_bread(IMFByteStream *t, BYTE *b, ULONG n, IMFAsyncCallback *cb, IUnknown *st) {
    /* (Windows' sources read asynchronously: done at once, the callback told; the count
     * kept for EndRead, the result holding no object) */
    m_read(t, b, n, &((Mem *)t)->got);
    IMFAsyncResult *r;
    MFCreateAsyncResult(NULL, cb, st, &r);
    MFInvokeCallback(r);
    IMFAsyncResult_Release(r);
    return S_OK;
}
static HRESULT WINAPI m_eread(IMFByteStream *t, IMFAsyncResult *r, ULONG *got) {
    *got = ((Mem *)t)->got;
    return S_OK;
}
static HRESULT WINAPI m_write(IMFByteStream *t, const BYTE *b, ULONG n, ULONG *w) { return E_NOTIMPL; }
static HRESULT WINAPI m_bwrite(IMFByteStream *t, const BYTE *b, ULONG n, IMFAsyncCallback *c, IUnknown *s) { return E_NOTIMPL; }
static HRESULT WINAPI m_ewrite(IMFByteStream *t, IMFAsyncResult *r, ULONG *w) { return E_NOTIMPL; }
static HRESULT WINAPI m_seek(IMFByteStream *t, MFBYTESTREAM_SEEK_ORIGIN o, LONGLONG off, DWORD f, QWORD *p) {
    Mem *m = (Mem *)t;
    m->pos = (o == msoBegin ? 0 : m->pos) + off;
    if (p) *p = m->pos;
    return S_OK;
}
static HRESULT WINAPI m_ok(IMFByteStream *t) { return S_OK; }
static IMFByteStreamVtbl mem_vt = { m_qi, m_add, m_rel, m_caps, m_len, m_setlen, m_getpos, m_setpos, m_eos, m_read, m_bread, m_eread, m_write, m_bwrite, m_ewrite, m_seek, m_ok, m_ok };

static void url(const char *name, const unsigned char *d, DWORD len, DWORD per) {
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfreader-probe.%hs", dir, name);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, d, len, &w, NULL);
    CloseHandle(f);
    IMFSourceReader *r = NULL;
    HRESULT hr = MFCreateSourceReaderFromURL(path, NULL, &r);
    printf("%s from its file: %#lx\n", name, hr);
    if (r) {
        open_one(name, r, per);
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
    IMFSourceReader *r = NULL;
    printf("missing file: %#lx\n", MFCreateSourceReaderFromURL(L"C:\\no-such-dir\\none.mp3", NULL, &r));
    url("m4a", file_m4a, sizeof file_m4a, 1024);
    url("mp3", file_mp3, sizeof file_mp3, 1152);
    url("wav", file_wav, sizeof file_wav, 1024);
    Mem m = { { &mem_vt }, 1, file_mp3, sizeof file_mp3, 0 };
    r = NULL;
    hr = MFCreateSourceReaderFromByteStream(&m.b, NULL, &r);
    printf("mp3 from a byte stream: %#lx\n", hr);
    if (r) {
        open_one("mp3 stream", r, 1152);
        IMFSourceReader_Release(r);
    }
    /* not a sound file */
    static const BYTE junk[4096] = { 1, 2, 3 };
    Mem j = { { &mem_vt }, 1, junk, sizeof junk, 0 };
    r = NULL;
    printf("junk: failed %d\n", FAILED(MFCreateSourceReaderFromByteStream(&j.b, NULL, &r)));
    if (r) IMFSourceReader_Release(r);
    MFShutdown();
    return 0;
}
