/* Media Foundation without a GPU (sake's node build): IMFAttributes in depth (every type, the
 * PROPVARIANTs, short buffers, Compare, CopyAllItems, the references it holds), the DXGI device
 * manager's tokens and references, and the Media Engine on the probe's movie (as
 * tests/winref/probes/mfengine.c, whose Windows answers check the rest): its events in order,
 * the clock (seek, play to the end, looping, play again from the end), OnVideoStreamTick, the
 * notify callback's references, and calls after Shutdown.
 * libs: -lole32 -lmfplat -lmfuuid -luuid -loleaut32 */
#include <stdio.h>
#include <string.h>
#include <math.h>
#include <windows.h>
#include <initguid.h>
#include <mfapi.h>

#define W 64
#define H 48
#define FRAMES 20
#define FPS 10

static void put32le(FILE *f, unsigned v) { fwrite(&v, 4, 1, f); }
static void fourcc(FILE *f, const char *s) { fwrite(s, 4, 1, f); }

/* the movie's .vnv (uncompressed I420; the .avi beside it only needs to be there) */
static void write_movie(const char *path) {
    char vnv[MAX_PATH];
    FILE *f = fopen(path, "wb");
    fputs("RIFF", f);
    fclose(f);
    snprintf(vnv, sizeof vnv, "%s.vnv", path);
    f = fopen(vnv, "wb");
    const char *codec = "raw-i420";
    int frame = W * H + 2 * (W / 2) * (H / 2);
    int table = 8 + (int)strlen(codec) + 12 * FRAMES;
    fourcc(f, "VNV1"); put32le(f, W); put32le(f, H); put32le(f, 1);
    put32le(f, strlen(codec)); fwrite(codec, strlen(codec), 1, f); put32le(f, FRAMES);
    for (int k = 0; k < FRAMES; k++) { put32le(f, 16 + table + k * frame); put32le(f, frame); put32le(f, 1); }
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        memset(px, 16 + 8 * k, W * H);
        memset(px + W * H, 128, 2 * (W / 2) * (H / 2));
        fwrite(px, frame, 1, f);
    }
    unsigned long long dur = 10000000ull * FRAMES / FPS;
    put32le(f, FPS); put32le(f, 1); put32le(f, (unsigned)dur); put32le(f, (unsigned)(dur >> 32));
    put32le(f, 0); put32le(f, 0); fourcc(f, "VNVt");
    fclose(f);
    free(px);
}

DEFINE_GUID(CLSID_Factory, 0xb44392da, 0x499b, 0x446b, 0xa4, 0xcb, 0x00, 0x5f, 0xea, 0xd0, 0xe6, 0xd5);
DEFINE_GUID(IID_Factory, 0x4d645ace, 0x26aa, 0x4688, 0x9b, 0xe1, 0xdf, 0x35, 0x16, 0x99, 0x0b, 0x93);
DEFINE_GUID(IID_EngineEx, 0x83015ead, 0xb1e6, 0x40d0, 0xa9, 0x8a, 0x37, 0x14, 0x5f, 0xfe, 0x1a, 0xd1);
DEFINE_GUID(IID_Engine, 0x98a1b0bb, 0x03eb, 0x4935, 0xae, 0x7c, 0x93, 0xc1, 0xfa, 0x0e, 0x1c, 0x93);
DEFINE_GUID(ATTR_CALLBACK, 0xc60381b8, 0x83a4, 0x41f8, 0xa3, 0xd0, 0xde, 0x05, 0x07, 0x68, 0x49, 0xa9);
DEFINE_GUID(ATTR_DXGI, 0x065702da, 0x1094, 0x486d, 0x86, 0x17, 0xee, 0x7c, 0xc4, 0xee, 0x46, 0x48);
DEFINE_GUID(ATTR_FORMAT, 0x5066893c, 0x8cf9, 0x42bc, 0x8b, 0x8a, 0x47, 0x22, 0x12, 0xe5, 0x27, 0x26);
DEFINE_GUID(K1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1);
DEFINE_GUID(K2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2);
DEFINE_GUID(K3, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3);
DEFINE_GUID(K4, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4);
DEFINE_GUID(K5, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5);
DEFINE_GUID(K6, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6);
DEFINE_GUID(K7, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7);

#define M(o, i, T) ((T)((void **)*(void **)(o))[i])
typedef HRESULT (WINAPI *F0)(void *);
typedef HRESULT (WINAPI *Fp)(void *, void *);
typedef HRESULT (WINAPI *Fpp)(void *, void *, void *);
typedef HRESULT (WINAPI *Fu)(void *, UINT);
typedef double (WINAPI *Fd)(void *);
typedef HRESULT (WINAPI *Fsd)(void *, double);
typedef USHORT (WINAPI *Fus)(void *);
typedef BOOL (WINAPI *Fb)(void *);
enum { GetError = 3, SetSource = 6, GetCurrentSource = 7, GetNetworkState = 8, Load = 12, GetReadyState = 14, IsSeeking = 15,
       GetCurrentTime_ = 16, SetCurrentTime = 17, GetDuration = 19, IsPaused = 20, SetPlaybackRate = 24, IsEnded = 27, SetAutoPlay = 29,
       SetLoop = 31, Play = 32, Pause = 33, SetMuted = 35, SetVolume = 37, HasVideo = 38, HasAudio = 39, GetNativeVideoSize = 40,
       GetVideoAspectRatio = 41, Shutdown = 42, TransferVideoFrame = 43, OnVideoStreamTick = 44, GetResourceCharacteristics = 52,
       GetNumberOfStreams = 54 };

/* an object that counts its references (a device, a callback) */
typedef struct { void **vtbl; LONG refs; } Counted;
static HRESULT WINAPI c_qi(void *t, REFIID r, void **o) { *o = t; ((Counted *)t)->refs++; return S_OK; }
static ULONG WINAPI c_ref(void *t) { return InterlockedIncrement(&((Counted *)t)->refs); }
static ULONG WINAPI c_rel(void *t) { return InterlockedDecrement(&((Counted *)t)->refs); }
static void *counted_vtbl[] = { c_qi, c_ref, c_rel };

/* the notify callback: its events in order, and its references */
static volatile LONG nevents;
static volatile DWORD_PTR error_params[2];
static int events[512];
static HRESULT WINAPI n_event(void *t, DWORD e, DWORD_PTR p1, DWORD p2) {
    LONG i = InterlockedIncrement(&nevents) - 1;
    if (i < 512) events[i] = e;
    if (e == 5) error_params[0] = p1, error_params[1] = p2;
    return S_OK;
}
static void *notify_vtbl[] = { c_qi, c_ref, c_rel, n_event };
static Counted notify = { notify_vtbl, 0 };

static int seen(int e) {
    for (int i = 0; i < nevents && i < 512; i++)
        if (events[i] == e) return 1;
    return 0;
}
static int wait_event(int e, int ms) {
    for (int t = 0; t < ms; t += 5) {
        if (seen(e)) return 1;
        Sleep(5);
    }
    return seen(e);
}
/* the events so far, then cleared */
static void list(const char *l) {
    printf("%s events:", l);
    for (int i = 0; i < nevents && i < 512; i++) printf(" %d", events[i]);
    printf("\n");
    nevents = 0;
}

static void attributes(void) {
    IMFAttributes *a, *b;
    printf("MFCreateAttributes %08lx\n", MFCreateAttributes(&a, 4));
    MFCreateAttributes(&b, 0);
    Counted unk = { counted_vtbl, 1 };
    a->lpVtbl->SetUINT32(a, &K1, 7);
    a->lpVtbl->SetUINT64(a, &K2, 0x123456789abcdefull);
    a->lpVtbl->SetDouble(a, &K3, 2.5);
    a->lpVtbl->SetGUID(a, &K4, &IID_Engine);
    a->lpVtbl->SetString(a, &K5, L"hello");
    BYTE blob[3] = { 1, 2, 3 };
    a->lpVtbl->SetBlob(a, &K6, blob, 3);
    a->lpVtbl->SetUnknown(a, &K7, (IUnknown *)&unk);
    printf("SetUnknown took a reference %d\n", unk.refs == 2);
    UINT32 n = 0, u = 0;
    a->lpVtbl->GetCount(a, &n);
    printf("count %u\n", n);
    for (UINT32 i = 0; i < n; i++) {
        GUID k;
        PROPVARIANT v;
        PropVariantInit(&v);
        HRESULT hr = a->lpVtbl->GetItemByIndex(a, i, &k, &v);
        MF_ATTRIBUTE_TYPE t;
        a->lpVtbl->GetItemType(a, &k, &t);
        printf("item %u: %08lx key %lu vt %u type %u", i, hr, k.Data1, v.vt, t);
        if (v.vt == VT_UI4) printf(" %lu", v.ulVal);
        if (v.vt == VT_UI8) printf(" %llx", v.uhVal.QuadPart);
        if (v.vt == VT_R8) printf(" %.2f", v.dblVal);
        if (v.vt == VT_CLSID) printf(" %08lx", v.puuid->Data1);
        if (v.vt == VT_LPWSTR) printf(" %ls", v.pwszVal);
        if (v.vt == (VT_VECTOR | VT_UI1)) printf(" %lu bytes, last %u", v.caub.cElems, v.caub.pElems[2]);
        if (v.vt == VT_UNKNOWN) printf(" same %d", v.punkVal == (IUnknown *)&unk);
        BOOL same = FALSE;
        a->lpVtbl->CompareItem(a, &k, &v, &same);
        printf(", compares %d\n", same);
        PropVariantClear(&v);
    }
    printf("GetItemByIndex past the end %08lx\n", a->lpVtbl->GetItemByIndex(a, n, NULL, NULL));
    printf("unknown references after the PROPVARIANTs went %ld\n", unk.refs);
    HRESULT hr = a->lpVtbl->GetUINT32(a, &K1, &u);
    printf("GetUINT32 %08lx %u\n", hr, u);
    UINT64 q;
    printf("GetUINT64 of a UINT32 %08lx, missing %08lx\n", a->lpVtbl->GetUINT64(a, &K1, &q), a->lpVtbl->GetUINT64(a, &IID_Factory, &q));
    double d;
    hr = a->lpVtbl->GetDouble(a, &K3, &d);
    printf("GetDouble %08lx %.2f\n", hr, d);
    GUID g;
    hr = a->lpVtbl->GetGUID(a, &K4, &g);
    printf("GetGUID %08lx %d\n", hr, IsEqualGUID(&g, &IID_Engine));
    WCHAR s[8];
    UINT32 len = 0;
    printf("GetStringLength %08lx ", a->lpVtbl->GetStringLength(a, &K5, &len));
    printf("%u\n", len);
    printf("GetString short %08lx, ", a->lpVtbl->GetString(a, &K5, s, 5, &len));
    hr = a->lpVtbl->GetString(a, &K5, s, 6, &len);
    printf("fits %08lx %ls %u\n", hr, s, len);
    WCHAR *as;
    hr = a->lpVtbl->GetAllocatedString(a, &K5, &as, &len);
    printf("GetAllocatedString %08lx %ls %u\n", hr, as, len);
    CoTaskMemFree(as);
    BYTE got[4];
    printf("GetBlob short %08lx, ", a->lpVtbl->GetBlob(a, &K6, got, 2, &len));
    hr = a->lpVtbl->GetBlob(a, &K6, got, 4, &len);
    printf("fits %08lx %u %u\n", hr, len, got[1]);
    BYTE *ab;
    hr = a->lpVtbl->GetAllocatedBlob(a, &K6, &ab, &len);
    printf("GetAllocatedBlob %08lx %u %u\n", hr, len, ab[2]);
    CoTaskMemFree(ab);
    IUnknown *o = NULL;
    hr = a->lpVtbl->GetUnknown(a, &K7, &IID_IUnknown, (void **)&o);
    printf("GetUnknown %08lx same %d\n", hr, o == (IUnknown *)&unk);
    o->lpVtbl->Release(o);
    printf("GetString of a UINT32 %08lx\n", a->lpVtbl->GetString(a, &K1, s, 8, &len));

    /* Compare */
    b->lpVtbl->SetUINT32(b, &K1, 7);
    BOOL r[5];
    for (int t = 0; t < 5; t++) a->lpVtbl->Compare(a, b, t, &r[t]);
    printf("compare (b within a): ours %d theirs %d all %d intersection %d smaller %d\n", r[0], r[1], r[2], r[3], r[4]);
    b->lpVtbl->SetUINT32(b, &K1, 8);
    a->lpVtbl->Compare(a, b, MF_ATTRIBUTES_MATCH_INTERSECTION, &r[0]);
    printf("intersection with another value %d\n", r[0]);

    /* replacing in place keeps the order; SetItem; deleting */
    a->lpVtbl->SetUINT32(a, &K3, 9);
    GUID k;
    a->lpVtbl->GetItemByIndex(a, 2, &k, NULL);
    printf("replaced in place %d\n", IsEqualGUID(&k, &K3));
    PROPVARIANT v;
    PropVariantInit(&v);
    v.vt = VT_UI4, v.ulVal = 11;
    printf("SetItem %08lx\n", a->lpVtbl->SetItem(a, &K1, &v));
    a->lpVtbl->GetUINT32(a, &K1, &u);
    printf("SetItem's value %u\n", u);
    v.vt = VT_I2;
    printf("SetItem VT_I2 %08lx\n", a->lpVtbl->SetItem(a, &K1, &v));

    /* CopyAllItems: the unknown's reference once more; what b had goes */
    printf("CopyAllItems %08lx\n", a->lpVtbl->CopyAllItems(a, b));
    b->lpVtbl->GetCount(b, &n);
    printf("copied %u, unknown references %ld\n", n, unk.refs);
    a->lpVtbl->DeleteItem(a, &K7);
    printf("DeleteItem released it %ld\n", unk.refs);
    a->lpVtbl->DeleteAllItems(a);
    a->lpVtbl->GetCount(a, &n);
    printf("DeleteAllItems %u\n", n);
    printf("released %lu\n", a->lpVtbl->Release(a));
    ULONG left = b->lpVtbl->Release(b);
    printf("released the copy %lu, unknown references %ld\n", left, unk.refs);
}

/* (the time only while paused: running, it moves) */
static void state(void *e, const char *l) {
    BOOL paused = M(e, IsPaused, Fb)(e);
    printf("%s: ready %u network %u paused %d ended %d seeking %d", l, M(e, GetReadyState, Fus)(e), M(e, GetNetworkState, Fus)(e), paused,
           M(e, IsEnded, Fb)(e), M(e, IsSeeking, Fb)(e));
    if (paused) printf(" time %.2f", M(e, GetCurrentTime_, Fd)(e));
    printf("\n");
}
static void tick(void *e, const char *l) {
    LONGLONG pts = -1;
    HRESULT hr = M(e, OnVideoStreamTick, Fp)(e, &pts);
    printf("%s: tick %08lx pts %lld\n", l, hr, hr == S_OK ? pts : -1);
}

int main(void) {
    printf("MFStartup %08lx\n", MFStartup(MF_VERSION, MFSTARTUP_FULL));
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    attributes();

    /* the device manager */
    Counted dev = { counted_vtbl, 1 };
    UINT token = 0;
    IMFDXGIDeviceManager *mgr;
    HRESULT hr = MFCreateDXGIDeviceManager(&token, &mgr);
    printf("MFCreateDXGIDeviceManager %08lx token %d\n", hr, token != 0);
    HANDLE h;
    printf("OpenDeviceHandle before a device %08lx\n", mgr->lpVtbl->OpenDeviceHandle(mgr, &h));
    printf("ResetDevice wrong token %08lx\n", mgr->lpVtbl->ResetDevice(mgr, (IUnknown *)&dev, token + 1));
    hr = mgr->lpVtbl->ResetDevice(mgr, (IUnknown *)&dev, token);
    printf("ResetDevice %08lx device references %ld\n", hr, dev.refs);
    printf("OpenDeviceHandle %08lx\n", mgr->lpVtbl->OpenDeviceHandle(mgr, &h));
    void *locked = NULL;
    hr = mgr->lpVtbl->LockDevice(mgr, h, &IID_IUnknown, &locked, TRUE);
    printf("LockDevice %08lx same %d\n", hr, locked == &dev);
    ((IUnknown *)locked)->lpVtbl->Release(locked);
    mgr->lpVtbl->UnlockDevice(mgr, h, FALSE);
    mgr->lpVtbl->CloseDeviceHandle(mgr, h);

    void *factory;
    printf("factory %08lx\n", CoCreateInstance(&CLSID_Factory, NULL, CLSCTX_INPROC_SERVER, &IID_Factory, &factory));
    void *nofactory = NULL;
    printf("factory as IMFMediaEngine %08lx\n", CoCreateInstance(&CLSID_Factory, NULL, CLSCTX_INPROC_SERVER, &IID_Engine, &nofactory));
    IMFAttributes *attr;
    MFCreateAttributes(&attr, 3);
    typedef HRESULT (WINAPI *Create)(void *, DWORD, IMFAttributes *, void **);
    void *e = (void *)1;
    hr = M(factory, 3, Create)(factory, 0, attr, &e);
    printf("CreateInstance without a callback %08lx out cleared %d\n", hr, e == NULL);
    attr->lpVtbl->SetUnknown(attr, &ATTR_CALLBACK, (IUnknown *)&notify);
    attr->lpVtbl->SetUnknown(attr, &ATTR_DXGI, (IUnknown *)mgr);
    attr->lpVtbl->SetUINT32(attr, &ATTR_FORMAT, 87);
    printf("CreateInstance %08lx\n", M(factory, 3, Create)(factory, 2, attr, &e));
    attr->lpVtbl->Release(attr);
    printf("notify held %d\n", notify.refs > 0);
    void *ex = NULL;
    hr = M(e, 0, Fpp)(e, (void *)&IID_EngineEx, &ex);
    printf("QueryInterface IMFMediaEngineEx %08lx same %d\n", hr, ex == e);
    M(ex, 2, F0)(ex);

    state(e, "empty");
    printf("empty: duration nan %d, video %d\n", isnan(M(e, GetDuration, Fd)(e)) != 0, M(e, HasVideo, Fb)(e));
    DWORD cx = 0, cy = 0;
    printf("empty: GetNativeVideoSize %08lx\n", M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy));
    tick(e, "empty");
    printf("empty: SetCurrentTime %08lx\n", M(e, SetCurrentTime, Fsd)(e, 1.0));

    /* a missing file */
    BSTR missing = SysAllocString(L"nothere.avi");
    printf("SetSource missing %08lx\n", M(e, SetSource, Fp)(e, missing));
    wait_event(5, 3000);
    list("missing");
    printf("missing: error event's code %lu, %08lx\n", (unsigned long)error_params[0], (unsigned long)error_params[1]);
    void *err = NULL;
    printf("GetError %08lx", M(e, GetError, Fp)(e, &err));
    if (err) printf(" code %u hr %08lx", M(err, 3, Fus)(err), M(err, 4, F0)(err)), M(err, 2, F0)(err);
    printf("\n");
    state(e, "missing");

    /* the movie */
    write_movie("mfmovie.avi");
    WCHAR full[MAX_PATH];
    GetFullPathNameW(L"mfmovie.avi", MAX_PATH, full, NULL);
    BSTR src = SysAllocString(full);
    printf("SetSource %08lx\n", M(e, SetSource, Fp)(e, src));
    wait_event(15, 3000);
    list("loaded");
    BSTR cur = NULL;
    M(e, GetCurrentSource, Fp)(e, &cur);
    printf("GetCurrentSource same %d\n", cur && !wcscmp(cur, full));
    SysFreeString(cur);
    err = (void *)1;
    M(e, GetError, Fp)(e, &err);
    printf("error cleared %d\n", err == NULL);
    state(e, "loaded");
    printf("loaded: duration %.2f, size %08lx ", M(e, GetDuration, Fd)(e), M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy));
    printf("%lux%lu, ", cx, cy);
    printf("aspect %08lx ", M(e, GetVideoAspectRatio, Fpp)(e, &cx, &cy));
    printf("%lu:%lu, video %d, audio %d\n", cx, cy, M(e, HasVideo, Fb)(e), M(e, HasAudio, Fb)(e));
    DWORD v = 0;
    printf("characteristics %08lx ", M(e, GetResourceCharacteristics, Fp)(e, &v));
    printf("%lx, ", v);
    printf("streams %08lx ", M(e, GetNumberOfStreams, Fp)(e, &v));
    printf("%lu\n", v);
    tick(e, "loaded");
    tick(e, "loaded again");
    int x = 0;
    printf("TransferVideoFrame into no texture %08lx\n", M(e, TransferVideoFrame, Fpp)(e, &x, NULL));

    printf("SetCurrentTime %08lx\n", M(e, SetCurrentTime, Fsd)(e, 0.75));
    wait_event(17, 3000);
    list("seeked");
    state(e, "seeked");
    tick(e, "seeked");
    printf("SetCurrentTime past the end %08lx\n", M(e, SetCurrentTime, Fsd)(e, 99.0));
    state(e, "past the end");
    M(e, SetCurrentTime, Fsd)(e, 1.5);
    wait_event(17, 3000);
    nevents = 0;

    DWORD t0 = GetTickCount();
    printf("Play %08lx\n", M(e, Play, F0)(e));
    int ended = wait_event(19, 4000);
    DWORD ms = GetTickCount() - t0;
    printf("ended %d in time %d\n", ended, ms >= 400 && ms <= 1500);
    if (ms < 400 || ms > 1500) fprintf(stderr, "ended after %lu ms\n", ms);
    Sleep(50);
    list("played");
    state(e, "ended");
    tick(e, "ended");

    printf("Play again %08lx\n", M(e, Play, F0)(e));
    wait_event(13, 2000);
    Sleep(50);
    list("again");
    state(e, "again");
    hr = M(e, Pause, F0)(e);
    printf("Pause %08lx, ", hr);
    printf("again %08lx\n", M(e, Pause, F0)(e));
    wait_event(9, 2000);
    Sleep(50);
    list("paused");
    double t1 = M(e, GetCurrentTime_, Fd)(e);
    Sleep(200);
    printf("paused: the clock stands %d\n", M(e, GetCurrentTime_, Fd)(e) == t1);

    /* looping from near the end: no end, it goes on */
    printf("SetLoop %08lx\n", M(e, SetLoop, Fu)(e, TRUE));
    M(e, SetCurrentTime, Fsd)(e, 1.8);
    M(e, Play, F0)(e);
    Sleep(600);
    printf("looping: ended %d, paused %d, from the start %d\n", seen(19), M(e, IsPaused, Fb)(e), M(e, GetCurrentTime_, Fd)(e) < 1.0);
    M(e, Pause, F0)(e);
    Sleep(50);
    list("looped");
    printf("SetVolume %08lx, ", M(e, SetVolume, Fsd)(e, 0.5));
    printf("SetMuted %08lx, ", M(e, SetMuted, Fu)(e, TRUE));
    printf("SetPlaybackRate %08lx, ", M(e, SetPlaybackRate, Fsd)(e, 2.0));
    printf("negative %08lx\n", M(e, SetPlaybackRate, Fsd)(e, -1.0));
    Sleep(50);
    list("volume");

    /* autoplay: the next load plays */
    M(e, SetLoop, Fu)(e, FALSE);
    M(e, SetAutoPlay, Fu)(e, TRUE);
    printf("Load %08lx\n", M(e, Load, F0)(e));
    wait_event(13, 3000);
    Sleep(50);
    list("autoplay");
    state(e, "autoplay");

    printf("Shutdown %08lx\n", M(e, Shutdown, F0)(e));
    printf("after shutdown: Play %08lx SetSource %08lx GetNativeVideoSize %08lx Shutdown %08lx tick %08lx\n", M(e, Play, F0)(e),
           M(e, SetSource, Fp)(e, src), M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy), M(e, Shutdown, F0)(e), M(e, OnVideoStreamTick, Fp)(e, &x));
    Sleep(100);
    nevents = 0;
    Sleep(100);
    printf("no events after shutdown %d\n", nevents == 0);
    printf("released %d\n", M(e, 2, F0)(e) == 0);
    printf("notify let go %d\n", notify.refs == 0);
    mgr->lpVtbl->Release(mgr);
    printf("device let go %d\n", dev.refs == 1);
    printf("MFShutdown %08lx\n", MFShutdown());
    return 0;
}
