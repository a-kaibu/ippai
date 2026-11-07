/* Media Foundation's Media Engine in frame-server mode, as AdvHD (x64) plays its movies:
 * MFStartup, the class factory's CreateInstance with MF_MEDIA_ENGINE_CALLBACK,
 * MF_MEDIA_ENGINE_DXGI_MANAGER (a D3D11 device's) and B8G8R8A8 output, SetSource, the events
 * the notify callback gets, GetReadyState / GetNativeVideoSize / GetDuration, OnVideoStreamTick
 * and TransferVideoFrame into a D3D11 texture (read back: which frame it is), seeking, Play to
 * the end (IsEnded), Pause, and calls after Shutdown. The movie is the probe's own uncompressed
 * AVI (64x48, 10 fps, 20 frames, each frame's halves two grays; as tests/winref/probes/dshow.c)
 * with the `.vnv` ippai pack would make of it next to it. mingw-w64 has no mfmediaengine.h:
 * the interfaces are called by vtable index.
 * run: browser
 * libs: -lole32 -lmfplat -lmfuuid -ld3d11 -luuid -loleaut32 */
#include "../probe.h"
#include <initguid.h>
#include <d3d11.h>
#include <mfapi.h>
#include <math.h>

#define W 64
#define H 48
#define FRAMES 20
#define FPS 10

static int gray_y(int k, int bottom) { return 16 + 8 * k + (bottom ? 40 : 0); }
static int gray(int y) { return ((y - 16) * 255 * 2 + 219) / (2 * 219); }
static void put32le(FILE *f, unsigned v) { fwrite(&v, 4, 1, f); }
static void put16le(FILE *f, unsigned v) { unsigned short s = v; fwrite(&s, 2, 1, f); }
static void fourcc(FILE *f, const char *s) { fwrite(s, 4, 1, f); }

static void write_avi(const char *path) {
    FILE *f = fopen(path, "wb");
    int frame = W * 3 * H;
    int hdrl = 4 + (8 + 56) + (8 + 4 + (8 + 56) + (8 + 40));
    int movi = 4 + FRAMES * (8 + frame);
    int idx = FRAMES * 16;
    fourcc(f, "RIFF"); put32le(f, 4 + (8 + hdrl) + (8 + movi) + (8 + idx)); fourcc(f, "AVI ");
    fourcc(f, "LIST"); put32le(f, hdrl); fourcc(f, "hdrl");
    fourcc(f, "avih"); put32le(f, 56);
    unsigned avih[14] = { 1000000 / FPS, frame * FPS, 0, 0x10, FRAMES, 0, 1, frame, W, H, 0, 0, 0, 0 };
    fwrite(avih, 4, 14, f);
    fourcc(f, "LIST"); put32le(f, 4 + (8 + 56) + (8 + 40)); fourcc(f, "strl");
    fourcc(f, "strh"); put32le(f, 56);
    fourcc(f, "vids"); put32le(f, 0);
    unsigned strh[10] = { 0, 0, 0, 1, FPS, 0, FRAMES, frame, 0xffffffff, 0 };
    fwrite(strh, 4, 10, f);
    put16le(f, 0); put16le(f, 0); put16le(f, W); put16le(f, H);
    fourcc(f, "strf"); put32le(f, 40);
    BITMAPINFOHEADER bi = { 40, W, H, 1, 24, BI_RGB, frame, 0, 0, 0, 0 };
    fwrite(&bi, 40, 1, f);
    fourcc(f, "LIST"); put32le(f, movi); fourcc(f, "movi");
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W * 3, gray(gray_y(k, H - 1 - y >= H / 2)), W * 3);
        fourcc(f, "00db"); put32le(f, frame); fwrite(px, frame, 1, f);
    }
    fourcc(f, "idx1"); put32le(f, idx);
    for (int k = 0; k < FRAMES; k++) { fourcc(f, "00db"); put32le(f, 0x10); put32le(f, 4 + k * (8 + frame)); put32le(f, frame); }
    fclose(f);
    free(px);
}

static void write_vnv(const char *path) {
    FILE *f = fopen(path, "wb");
    const char *codec = "raw-i420";
    int frame = W * H + 2 * (W / 2) * (H / 2);
    int table = 8 + (int)strlen(codec) + 12 * FRAMES;
    fourcc(f, "VNV1"); put32le(f, W); put32le(f, H); put32le(f, 1);
    put32le(f, strlen(codec)); fwrite(codec, strlen(codec), 1, f); put32le(f, FRAMES);
    for (int k = 0; k < FRAMES; k++) { put32le(f, 16 + table + k * frame); put32le(f, frame); put32le(f, 1); }
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W, gray_y(k, y >= H / 2), W);
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
DEFINE_GUID(IID_Notify, 0xfee7c112, 0xe776, 0x42b5, 0x9b, 0xbf, 0x00, 0x48, 0x52, 0x4e, 0x2b, 0xd5);
DEFINE_GUID(IID_EngineEx, 0x83015ead, 0xb1e6, 0x40d0, 0xa9, 0x8a, 0x37, 0x14, 0x5f, 0xfe, 0x1a, 0xd1);
DEFINE_GUID(IID_Engine, 0x98a1b0bb, 0x03eb, 0x4935, 0xae, 0x7c, 0x93, 0xc1, 0xfa, 0x0e, 0x1c, 0x93);
DEFINE_GUID(ATTR_CALLBACK, 0xc60381b8, 0x83a4, 0x41f8, 0xa3, 0xd0, 0xde, 0x05, 0x07, 0x68, 0x49, 0xa9);
DEFINE_GUID(ATTR_DXGI, 0x065702da, 0x1094, 0x486d, 0x86, 0x17, 0xee, 0x7c, 0xc4, 0xee, 0x46, 0x48);
DEFINE_GUID(ATTR_FORMAT, 0x5066893c, 0x8cf9, 0x42bc, 0x8b, 0x8a, 0x47, 0x22, 0x12, 0xe5, 0x27, 0x26);

/* a method of COM object `o` by vtable index */
#define M(o, i, T) ((T)((void **)*(void **)(o))[i])
typedef HRESULT (WINAPI *F0)(void *);
typedef HRESULT (WINAPI *Fp)(void *, void *);
typedef HRESULT (WINAPI *Fpp)(void *, void *, void *);
typedef HRESULT (WINAPI *Fu)(void *, UINT);
typedef double (WINAPI *Fd)(void *);
typedef HRESULT (WINAPI *Fsd)(void *, double);
typedef USHORT (WINAPI *Fus)(void *);
typedef BOOL (WINAPI *Fb)(void *);
/* IMFMediaEngine */
enum { GetError = 3, SetSource = 6, GetNetworkState = 8, GetReadyState = 14, IsSeeking = 15, GetCurrentTime_ = 16, SetCurrentTime = 17,
       GetDuration = 19, IsPaused = 20, GetPlaybackRate = 23, IsEnded = 27, GetAutoPlay = 28, SetAutoPlay = 29, GetLoop = 30, SetLoop = 31,
       Play = 32, Pause = 33, GetMuted = 34, GetVolume = 36, SetVolume = 37, HasVideo = 38, HasAudio = 39, GetNativeVideoSize = 40,
       GetVideoAspectRatio = 41, Shutdown = 42, TransferVideoFrame = 43, OnVideoStreamTick = 44 };

/* the notify callback: the events it got (in order) */
static volatile LONG nevents;
static int events[256];
static HRESULT WINAPI n_qi(void *t, REFIID r, void **o) {
    if (IsEqualGUID(r, &IID_IUnknown) || IsEqualGUID(r, &IID_Notify)) { *o = t; return S_OK; }
    *o = NULL;
    return E_NOINTERFACE;
}
static ULONG WINAPI n_ref(void *t) { return 2; }
static HRESULT WINAPI n_event(void *t, DWORD e, DWORD_PTR p1, DWORD p2) {
    LONG i = InterlockedIncrement(&nevents) - 1;
    if (i < 256) events[i] = e;
    if (e == 1008) SetEvent((HANDLE)p1); /* NOTIFYSTABLESTATE */
    return S_OK;
}
static void *notify_vtbl[] = { n_qi, n_ref, n_ref, n_event };
static void *notify = notify_vtbl;

static int seen(int e) {
    for (int i = 0; i < nevents && i < 256; i++)
        if (events[i] == e) return i + 1;
    return 0;
}
/* waits for event e (up to ms); whether it came */
static int wait_event(int e, int ms) {
    for (int t = 0; t < ms; t += 10) {
        if (seen(e)) return 1;
        Sleep(10);
    }
    return seen(e) != 0;
}

static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11Texture2D *tex, *staging;

/* the frame the texture shows (by the gray of its top half) and its bottom half's */
static void frame(void *engine, const char *l) {
    RECT r = { 0, 0, W, H };
    MFARGB border = { 0, 0, 0, 255 };
    typedef HRESULT (WINAPI *T)(void *, IUnknown *, const void *, const RECT *, const MFARGB *);
    HRESULT hr = M(engine, TransferVideoFrame, T)(engine, (IUnknown *)tex, NULL, &r, &border);
    printf("%s: TransferVideoFrame %08lx", l, hr);
    if (SUCCEEDED(hr)) {
        ctx->lpVtbl->CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)tex);
        D3D11_MAPPED_SUBRESOURCE m;
        if (SUCCEEDED(ctx->lpVtbl->Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &m))) {
            for (int half = 0; half < 2; half++) {
                unsigned char *p = (unsigned char *)m.pData + (half ? H - 3 : 2) * m.RowPitch + 5 * 4;
                int f = -1;
                for (int k = 0; k < FRAMES; k++)
                    if (abs(gray(gray_y(k, half)) - p[1]) <= 3) f = k;
                printf(" %s frame %d", half ? "bottom" : "top", f);
            }
            ctx->lpVtbl->Unmap(ctx, (ID3D11Resource *)staging, 0);
        }
    }
    printf("\n");
}

static void state(void *e, const char *l) {
    double t = M(e, GetCurrentTime_, Fd)(e);
    printf("%s: ready %u paused %d ended %d seeking %d time %.1f\n", l, M(e, GetReadyState, Fus)(e), M(e, IsPaused, Fb)(e), M(e, IsEnded, Fb)(e),
           M(e, IsSeeking, Fb)(e), t);
}

int main(void) {
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    if (FAILED(hr)) {
        printf("skip MFStartup %08lx (no Media Foundation)\n", hr);
        return 0;
    }
    printf("MFStartup %08lx\n", hr);
    write_avi("probe.avi");
    write_vnv("probe.avi.vnv");
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    D3D_FEATURE_LEVEL fl;
    UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, flags, NULL, 0, D3D11_SDK_VERSION, &dev, &fl, &ctx);
    if (FAILED(hr)) hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_WARP, NULL, flags, NULL, 0, D3D11_SDK_VERSION, &dev, &fl, &ctx);
    printf("D3D11CreateDevice %08lx\n", hr);
    if (FAILED(hr)) return 1;
    ID3D10Multithread *mt;
    if (SUCCEEDED(dev->lpVtbl->QueryInterface(dev, &IID_ID3D10Multithread, (void **)&mt))) {
        mt->lpVtbl->SetMultithreadProtected(mt, TRUE);
        mt->lpVtbl->Release(mt);
    }
    D3D11_TEXTURE2D_DESC td = { W, H, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, { 1, 0 }, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0 };
    dev->lpVtbl->CreateTexture2D(dev, &td, NULL, &tex);
    td.Usage = D3D11_USAGE_STAGING, td.BindFlags = 0, td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    dev->lpVtbl->CreateTexture2D(dev, &td, NULL, &staging);

    UINT token = 0;
    IMFDXGIDeviceManager *mgr;
    hr = MFCreateDXGIDeviceManager(&token, &mgr);
    printf("MFCreateDXGIDeviceManager %08lx token %d\n", hr, token != 0);
    printf("ResetDevice %08lx\n", mgr->lpVtbl->ResetDevice(mgr, (IUnknown *)dev, token));
    printf("ResetDevice wrong token %08lx\n", mgr->lpVtbl->ResetDevice(mgr, (IUnknown *)dev, token + 1));

    void *factory;
    hr = CoCreateInstance(&CLSID_Factory, NULL, CLSCTX_INPROC_SERVER, &IID_Factory, &factory);
    printf("factory %08lx\n", hr);
    if (FAILED(hr)) return 1;
    IMFAttributes *attr;
    hr = MFCreateAttributes(&attr, 1);
    printf("MFCreateAttributes %08lx\n", hr);
    attr->lpVtbl->SetUnknown(attr, &ATTR_CALLBACK, (IUnknown *)&notify);
    attr->lpVtbl->SetUnknown(attr, &ATTR_DXGI, (IUnknown *)mgr);
    attr->lpVtbl->SetUINT32(attr, &ATTR_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM);
    UINT32 n = 0;
    attr->lpVtbl->GetCount(attr, &n);
    UINT32 fmt = 0;
    hr = attr->lpVtbl->GetUINT32(attr, &ATTR_FORMAT, &fmt);
    printf("attributes %u, format %08lx %u\n", n, hr, fmt);
    UINT64 u64;
    printf("GetUINT64 of a UINT32 %08lx\n", attr->lpVtbl->GetUINT64(attr, &ATTR_FORMAT, &u64));
    printf("GetUINT32 missing %08lx\n", attr->lpVtbl->GetUINT32(attr, &IID_Engine, &fmt));

    /* (no callback: refused) */
    void *e = NULL;
    IMFAttributes *bare;
    MFCreateAttributes(&bare, 1);
    typedef HRESULT (WINAPI *Create)(void *, DWORD, IMFAttributes *, void **);
    hr = M(factory, 3, Create)(factory, 0, bare, &e);
    printf("CreateInstance without a callback %08lx\n", hr);
    if (e) M(e, 2, F0)(e), e = NULL;
    hr = M(factory, 3, Create)(factory, 0, attr, &e);
    printf("CreateInstance %08lx\n", hr);
    if (FAILED(hr)) return 1;
    void *ex = NULL;
    printf("QueryInterface IMFMediaEngineEx %08lx\n", M(e, 0, Fpp)(e, (void *)&IID_EngineEx, &ex));
    if (ex) M(ex, 2, F0)(ex);

    /* nothing loaded */
    state(e, "empty");
    double d = M(e, GetDuration, Fd)(e);
    printf("empty: duration nan %d, network %u, autoplay %d, loop %d, volume %.2f, muted %d, rate %.2f, video %d, audio %d\n", isnan(d),
           M(e, GetNetworkState, Fus)(e), M(e, GetAutoPlay, Fb)(e), M(e, GetLoop, Fb)(e), M(e, GetVolume, Fd)(e), M(e, GetMuted, Fb)(e),
           M(e, GetPlaybackRate, Fd)(e), M(e, HasVideo, Fb)(e), M(e, HasAudio, Fb)(e));
    DWORD cx = 1, cy = 1;
    hr = M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy);
    printf("empty: GetNativeVideoSize %08lx %lux%lu\n", hr, cx, cy);
    LONGLONG pts = 1;
    hr = M(e, OnVideoStreamTick, Fp)(e, &pts);
    printf("empty: OnVideoStreamTick %08lx\n", hr);
    frame(e, "empty");

    /* a missing file: the error */
    BSTR missing = SysAllocString(L"nothere.avi");
    nevents = 0;
    hr = M(e, SetSource, Fp)(e, missing);
    printf("SetSource missing %08lx\n", hr);
    printf("missing: error event %d\n", wait_event(5, 5000));
    void *err = NULL;
    hr = M(e, GetError, Fp)(e, &err);
    printf("missing: GetError %08lx", hr);
    if (err) {
        typedef USHORT (WINAPI *C)(void *);
        printf(" code %u", M(err, 3, C)(err));
        M(err, 2, F0)(err);
    }
    printf("\n");
    state(e, "missing");

    /* the movie */
    WCHAR full[MAX_PATH];
    GetFullPathNameW(L"probe.avi", MAX_PATH, full, NULL);
    BSTR src = SysAllocString(full);
    nevents = 0;
    hr = M(e, SetSource, Fp)(e, src);
    printf("SetSource %08lx\n", hr);
    int ok = wait_event(15, 5000) || wait_event(14, 1);
    printf("can play %d\n", ok);
    /* (the events up to then, those that come every time: load start, duration, metadata,
     * data, can play, can play through, first frame) */
    int order[] = { 1, 21, 10, 11, 14, 15, 1009 };
    for (int i = 0; i < 7; i++) printf("event %d seen %d\n", order[i], seen(order[i]) != 0);
    printf("load start before metadata %d, metadata before data %d, data before can play %d\n", seen(1) < seen(10), seen(10) < seen(11),
           seen(11) < seen(14));
    state(e, "loaded");
    d = M(e, GetDuration, Fd)(e);
    hr = M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy);
    printf("loaded: duration %.2f, size %08lx %lux%lu, video %d, audio %d, network %u\n", d, hr, cx, cy, M(e, HasVideo, Fb)(e), M(e, HasAudio, Fb)(e),
           M(e, GetNetworkState, Fus)(e));
    hr = M(e, OnVideoStreamTick, Fp)(e, &pts);
    printf("loaded: OnVideoStreamTick %08lx pts %lld\n", hr, hr == S_OK ? pts : -1);
    frame(e, "loaded");

    /* seek while paused */
    nevents = 0;
    printf("SetCurrentTime %08lx\n", M(e, SetCurrentTime, Fsd)(e, 0.75));
    printf("seeked %d, seeking seen %d\n", wait_event(17, 3000), seen(16) != 0);
    state(e, "seeked");
    hr = M(e, OnVideoStreamTick, Fp)(e, &pts);
    printf("seeked: OnVideoStreamTick %08lx pts within a frame %d\n", hr, hr == S_OK && pts >= 6500000 && pts <= 7500000);
    frame(e, "seeked");

    /* play to the end (1.25 s) */
    nevents = 0;
    DWORD t0 = GetTickCount();
    printf("Play %08lx\n", M(e, Play, F0)(e));
    printf("play event %d\n", wait_event(8, 1000));
    printf("playing event %d\n", wait_event(13, 2000));
    Sleep(300);
    double t = M(e, GetCurrentTime_, Fd)(e);
    printf("playing: paused %d ended %d, time moved %d\n", M(e, IsPaused, Fb)(e), M(e, IsEnded, Fb)(e), t > 0.8 && t < 1.9);
    int ended = wait_event(19, 4000);
    int ms = GetTickCount() - t0;
    printf("ended event %d after the expected time %d\n", ended, ms >= 900 && ms <= 3000);
    if (ms < 900 || ms > 3000) fprintf(stderr, "ended after %d ms\n", ms);
    printf("pause event at the end %d\n", seen(9) != 0);
    state(e, "ended");
    frame(e, "ended");
    /* play again from the end: from the start */
    nevents = 0;
    printf("Play again %08lx\n", M(e, Play, F0)(e));
    wait_event(13, 2000);
    t = M(e, GetCurrentTime_, Fd)(e);
    printf("again: from the start %d, ended %d\n", t < 1.0, M(e, IsEnded, Fb)(e));
    printf("Pause %08lx\n", M(e, Pause, F0)(e));
    printf("pause event %d\n", wait_event(9, 2000));
    state(e, "paused");
    printf("Pause again %08lx\n", M(e, Pause, F0)(e));
    printf("SetVolume %08lx\n", M(e, SetVolume, Fsd)(e, 0.5));
    printf("volume %.2f\n", M(e, GetVolume, Fd)(e));
    printf("SetLoop %08lx loop %d\n", M(e, SetLoop, Fu)(e, TRUE), M(e, GetLoop, Fb)(e));
    printf("SetAutoPlay %08lx autoplay %d\n", M(e, SetAutoPlay, Fu)(e, TRUE), M(e, GetAutoPlay, Fb)(e));

    printf("Shutdown %08lx\n", M(e, Shutdown, F0)(e));
    printf("after shutdown: Play %08lx\n", M(e, Play, F0)(e));
    printf("after shutdown: SetSource %08lx\n", M(e, SetSource, Fp)(e, src));
    hr = M(e, GetNativeVideoSize, Fpp)(e, &cx, &cy);
    printf("after shutdown: GetNativeVideoSize %08lx\n", hr);
    printf("after shutdown: Shutdown %08lx\n", M(e, Shutdown, F0)(e));
    printf("released %d\n", M(e, 2, F0)(e) == 0);
    M(factory, 2, F0)(factory);
    attr->lpVtbl->Release(attr);
    bare->lpVtbl->Release(bare);
    mgr->lpVtbl->Release(mgr);
    printf("MFShutdown %08lx\n", MFShutdown());
    return 0;
}
