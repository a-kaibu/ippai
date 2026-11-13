/* DirectShow's filter graph playing a sound-only file (a WAV here; games render their mp3
 * voices and music the same way): RenderFile, the duration, Run, the position as it plays,
 * EC_COMPLETE at its end, a seek and a second end, IBasicAudio's volume, Stop. sake played
 * nothing for such a file (no `.vnv`: EC_COMPLETE half a second after Run, duration 0, #85).
 * Times are printed rounded to what a loaded runner keeps to.
 * libs: -lole32 -lstrmiids -loleaut32 -luuid */
#include "../probe.h"
#include <dshow.h>

#define RATE 22050
#define SECS 1.5

static void put32le(FILE *f, unsigned v) { fwrite(&v, 4, 1, f); }
static void put16le(FILE *f, unsigned v) { unsigned short s = v; fwrite(&s, 2, 1, f); }

static void write_wav(const char *path) {
    FILE *f = fopen(path, "wb");
    unsigned n = (unsigned)(RATE * SECS);
    fwrite("RIFF", 4, 1, f);
    put32le(f, 36 + 2 * n);
    fwrite("WAVEfmt ", 8, 1, f);
    put32le(f, 16);
    put16le(f, 1);
    put16le(f, 1);
    put32le(f, RATE);
    put32le(f, RATE * 2);
    put16le(f, 2);
    put16le(f, 16);
    fwrite("data", 4, 1, f);
    put32le(f, 2 * n);
    for (unsigned i = 0; i < n; i++) put16le(f, (i / 50) % 2 ? 3000 : (unsigned)-3000);
    fclose(f);
}

/* waits up to `ms` for EC_COMPLETE; the milliseconds it took, or -1 */
static long wait_complete(IMediaEventEx *me, DWORD ms) {
    DWORD t0 = GetTickCount();
    for (;;) {
        long code;
        LONG_PTR p1, p2;
        DWORD left = ms - (GetTickCount() - t0);
        if ((long)left <= 0) return -1;
        if (me->lpVtbl->GetEvent(me, &code, &p1, &p2, left) != S_OK) return -1;
        me->lpVtbl->FreeEventParams(me, code, p1, p2);
        if (code == EC_COMPLETE) return (long)(GetTickCount() - t0);
    }
}

int main(void) {
    CoInitialize(NULL);
    write_wav("probe.wav");
    IGraphBuilder *gb = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb);
    printf("create %08lx\n", hr);
    if (!gb) return 0;
    hr = gb->lpVtbl->RenderFile(gb, L"probe.wav", NULL);
    printf("RenderFile %08lx\n", hr);
    if (hr != S_OK) return 0;
    IMediaControl *mc;
    IMediaSeeking *ms;
    IMediaEventEx *me;
    IMediaPosition *mp;
    IBasicAudio *ba;
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaControl, (void **)&mc);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaSeeking, (void **)&ms);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaEventEx, (void **)&me);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaPosition, (void **)&mp);
    gb->lpVtbl->QueryInterface(gb, &IID_IBasicAudio, (void **)&ba);
    LONGLONG d = 0, cur = -1;
    hr = ms->lpVtbl->GetDuration(ms, &d);
    printf("GetDuration %08lx %.2f\n", hr, d / 1e7);
    REFTIME rd = 0;
    mp->lpVtbl->get_Duration(mp, &rd);
    printf("get_Duration %.2f\n", rd);
    hr = ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("GetCurrentPosition %08lx %.2f\n", hr, cur / 1e7);
    long vol = 1;
    hr = ba->lpVtbl->put_Volume(ba, -1000);
    HRESULT hr2 = ba->lpVtbl->get_Volume(ba, &vol);
    printf("put_Volume %08lx get_Volume %08lx %ld\n", hr, hr2, vol);
    printf("put_Volume out of range %08lx\n", ba->lpVtbl->put_Volume(ba, 1));

    hr = mc->lpVtbl->Run(mc);
    printf("Run %s\n", SUCCEEDED(hr) ? "ok" : "failed");
    Sleep(500);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position after 0.5 s within 0.25 %d\n", cur > 2500000 && cur < 7500000);
    long t = wait_complete(me, 3000);
    printf("EC_COMPLETE %d after the rest (0.5..1.5 s) %d\n", t >= 0, t >= 500 && t < 1500);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position at the end %.2f\n", cur / 1e7);
    OAFilterState st = 9;
    mc->lpVtbl->GetState(mc, 100, &st);
    printf("state at the end %ld\n", st);

    LONGLONG to = 10000000;
    hr = ms->lpVtbl->SetPositions(ms, &to, AM_SEEKING_AbsolutePositioning, NULL, AM_SEEKING_NoPositioning);
    printf("SetPositions 1.0 %08lx\n", hr);
    mc->lpVtbl->Run(mc);
    t = wait_complete(me, 3000);
    printf("second EC_COMPLETE %d after about 0.5 s (0.25..1.2 s) %d\n", t >= 0, t >= 250 && t < 1200);

    hr = mc->lpVtbl->Stop(mc);
    mc->lpVtbl->GetState(mc, 100, &st);
    printf("Stop %08lx state %ld\n", hr, st);
    to = 0;
    ms->lpVtbl->SetPositions(ms, &to, AM_SEEKING_AbsolutePositioning, NULL, AM_SEEKING_NoPositioning);
    mc->lpVtbl->Pause(mc);
    Sleep(300);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("paused: position stays at 0 %d\n", cur < 500000);
    mc->lpVtbl->Stop(mc);
    ba->lpVtbl->Release(ba);
    mp->lpVtbl->Release(mp);
    me->lpVtbl->Release(me);
    ms->lpVtbl->Release(ms);
    mc->lpVtbl->Release(mc);
    gb->lpVtbl->Release(gb);
    DeleteFileA("probe.wav");
    CoUninitialize();
    return 0;
}
