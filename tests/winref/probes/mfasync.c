/* The Source Reader in asynchronous mode (MF_SOURCE_READER_ASYNC_CALLBACK), as games stream music
 * with it: ReadSample answered on the callback's OnReadSample on another thread, in order, to the
 * end of the stream; the out pointers refused; SetCurrentPosition while a read is pending; Flush
 * and OnFlush, with reads queued behind one whose callback has not returned. A WAVE file (PCM:
 * no decoder). sake refused this mode before (#85).
 * libs: -lmfplat -lmfreadwrite -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>

#define RATE 22050
#define FRAMES 66150 /* 3 s, mono 16-bit */

typedef struct {
    IMFSourceReaderCallback iface;
    LONG refs;
    HANDLE done, flushed, gate;
    volatile LONG reads, flushes, other_thread, gated;
    HRESULT hr;
    DWORD index, flags, last_flush_stream;
    LONGLONG time;
    DWORD bytes, samples;
    DWORD main_thread;
} Cb;

static HRESULT WINAPI cb_qi(IMFSourceReaderCallback *t, REFIID iid, void **o) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IMFSourceReaderCallback)) {
        *o = t;
        IMFSourceReaderCallback_AddRef(t);
        return S_OK;
    }
    *o = NULL;
    return E_NOINTERFACE;
}
static ULONG WINAPI cb_add(IMFSourceReaderCallback *t) { return InterlockedIncrement(&((Cb *)t)->refs); }
static ULONG WINAPI cb_rel(IMFSourceReaderCallback *t) { return InterlockedDecrement(&((Cb *)t)->refs); }
static HRESULT WINAPI cb_read(IMFSourceReaderCallback *t, HRESULT hr, DWORD index, DWORD flags, LONGLONG time, IMFSample *s) {
    Cb *c = (Cb *)t;
    if (GetCurrentThreadId() != c->main_thread) InterlockedIncrement(&c->other_thread);
    c->hr = hr;
    c->index = index;
    c->flags = flags;
    c->time = time;
    if (s) {
        DWORD n = 0;
        IMFSample_GetTotalLength(s, &n);
        c->bytes += n;
        c->samples++;
    }
    InterlockedIncrement(&c->reads);
    if (c->gated) WaitForSingleObject(c->gate, 5000);
    SetEvent(c->done);
    return S_OK;
}
static HRESULT WINAPI cb_flush(IMFSourceReaderCallback *t, DWORD index) {
    Cb *c = (Cb *)t;
    c->last_flush_stream = index;
    InterlockedIncrement(&c->flushes);
    SetEvent(c->flushed);
    return S_OK;
}
static HRESULT WINAPI cb_event(IMFSourceReaderCallback *t, DWORD index, IMFMediaEvent *e) { return S_OK; }
static IMFSourceReaderCallbackVtbl cb_vt = { cb_qi, cb_add, cb_rel, cb_read, cb_flush, cb_event };

static void put(HANDLE f, const void *p, DWORD n) {
    DWORD w;
    WriteFile(f, p, n, &w, NULL);
}

int main(void) {
    MFStartup(MF_VERSION, MFSTARTUP_FULL);
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfasync-probe.wav", dir);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD v;
    WORD w;
    put(f, "RIFF", 4); v = 36 + FRAMES * 2; put(f, &v, 4);
    put(f, "WAVEfmt ", 8); v = 16; put(f, &v, 4);
    w = 1; put(f, &w, 2); put(f, &w, 2);
    v = RATE; put(f, &v, 4); v = RATE * 2; put(f, &v, 4);
    w = 2; put(f, &w, 2); w = 16; put(f, &w, 2);
    put(f, "data", 4); v = FRAMES * 2; put(f, &v, 4);
    for (int i = 0; i < FRAMES; i++) {
        short s = (i / 25) % 2 ? 4000 : -4000;
        put(f, &s, 2);
    }
    CloseHandle(f);

    Cb cb = { { &cb_vt }, 1 };
    cb.done = CreateEventA(NULL, FALSE, FALSE, NULL);
    cb.flushed = CreateEventA(NULL, FALSE, FALSE, NULL);
    cb.gate = CreateEventA(NULL, TRUE, FALSE, NULL);
    cb.main_thread = GetCurrentThreadId();
    IMFAttributes *at = NULL;
    MFCreateAttributes(&at, 1);
    IMFAttributes_SetUnknown(at, &MF_SOURCE_READER_ASYNC_CALLBACK, (IUnknown *)&cb.iface);
    IMFSourceReader *r = NULL;
    HRESULT hr = MFCreateSourceReaderFromURL(path, at, &r);
    printf("create async %#lx, the callback held %d\n", hr, cb.refs > 1);
    IMFAttributes_Release(at);
    if (!r) return 0;

    DWORD sf = 0, ai = 0;
    LONGLONG ts = 0;
    IMFSample *smp = NULL;
    printf("ReadSample with out pointers %#lx\n", IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, &ai, &sf, &ts, &smp));
    printf("ReadSample with the sample pointer only %#lx\n", IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, &smp));
    printf("ReadSample of stream 5 %#lx\n", IMFSourceReader_ReadSample(r, 5, 0, NULL, NULL, NULL, NULL));

    /* to the end, a read at a time */
    int calls = 0, ok = 1, ordered = 1;
    LONGLONG prev = -1;
    for (;;) {
        hr = IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, NULL);
        if (hr != S_OK) {
            printf("ReadSample %#lx\n", hr);
            break;
        }
        if (WaitForSingleObject(cb.done, 5000) != WAIT_OBJECT_0) {
            printf("no OnReadSample\n");
            break;
        }
        calls++;
        if (cb.hr != S_OK || cb.index != 0) ok = 0;
        if (cb.flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
        if (cb.time <= prev) ordered = 0;
        prev = cb.time;
        if (calls > 1000) break;
    }
    printf("answers ok %d, on another thread %d, times rising %d\n", ok, cb.other_thread == calls, ordered);
    printf("samples %lu, bytes %lu, the end's flags %#lx, at %.2f s\n", cb.samples, cb.bytes, cb.flags, cb.time / 1e7);
    printf("one more read past the end: ");
    IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, NULL);
    WaitForSingleObject(cb.done, 5000);
    printf("flags %#lx sample bytes still %lu\n", cb.flags, cb.bytes);

    PROPVARIANT pos;
    PropVariantInit(&pos);
    pos.vt = VT_I8;
    pos.hVal.QuadPart = 0;
    printf("SetCurrentPosition 0 idle %#lx\n", IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &pos));

    /* a read whose callback is held: a seek meanwhile, then reads queued behind it and a Flush */
    cb.gated = 1;
    ResetEvent(cb.gate);
    LONG before = cb.reads;
    IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, NULL);
    for (int i = 0; i < 500 && cb.reads == before; i++) Sleep(10);
    printf("held read reached the callback %d\n", cb.reads == before + 1);
    printf("SetCurrentPosition with a read pending %#lx\n", IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &pos));
    IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, NULL);
    IMFSourceReader_ReadSample(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM, 0, NULL, NULL, NULL, NULL);
    printf("Flush %#lx\n", IMFSourceReader_Flush(r, MF_SOURCE_READER_FIRST_AUDIO_STREAM));
    cb.gated = 0;
    SetEvent(cb.gate);
    DWORD fw = WaitForSingleObject(cb.flushed, 5000);
    Sleep(300);
    printf("OnFlush %d (stream %#lx), reads answered after the held one %ld\n", fw == WAIT_OBJECT_0 && cb.flushes == 1, cb.last_flush_stream, cb.reads - before - 1);

    printf("Flush idle %#lx\n", IMFSourceReader_Flush(r, MF_SOURCE_READER_ALL_STREAMS));
    fw = WaitForSingleObject(cb.flushed, 5000);
    printf("OnFlush again %d (stream %#lx)\n", fw == WAIT_OBJECT_0, cb.last_flush_stream);
    printf("SetCurrentPosition after the flush %#lx\n", IMFSourceReader_SetCurrentPosition(r, &GUID_NULL, &pos));

    IMFSourceReader_Release(r);
    Sleep(100);
    printf("the callback let go %d\n", cb.refs == 1);
    DeleteFileW(path);
    MFShutdown();
    return 0;
}
