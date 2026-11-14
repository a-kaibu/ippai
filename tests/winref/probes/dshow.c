/* DirectShow's filter graph playing a movie file, as games do (RenderFile, IVideoWindow on a
 * child of the game's window, Pause/Run/Stop, IMediaSeeking and IMediaPosition, EC_COMPLETE
 * through GetEvent, the event handle and SetNotifyWindow, IBasicVideo's sizes and
 * GetCurrentImage); then graphs rendering into a windowless VMR-7 / VMR-9 added by hand, as
 * Artemis does (IVMRFilterConfig, IVMRWindowlessControl). The probe writes its own movie: an uncompressed AVI (64x48, 10 fps, 20
 * frames, each frame's top and bottom halves two grays), which Windows plays with its AVI
 * splitter, and next to it the `.vnv` masu pack would make of it (uncompressed I420, the same
 * pictures in video range), which sake plays. Times are printed rounded to what a loaded
 * runner keeps to.
 * libs: -lole32 -lstrmiids -loleaut32 -luuid -lgdi32 -luser32 */
#include "../probe.h"
#include <dshow.h>
#include <d3d9.h>
#include <vmr9.h>

#define W 64
#define H 48
#define FRAMES 20
#define FPS 10

/* frame k's grays (top, bottom): video range Y that turns back into exactly these */
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
    /* avih: usec/frame, max bytes/s, padding, flags (HASINDEX), frames, initial, streams, buffer, w, h, reserved */
    fourcc(f, "avih"); put32le(f, 56);
    unsigned avih[14] = { 1000000 / FPS, frame * FPS, 0, 0x10, FRAMES, 0, 1, frame, W, H, 0, 0, 0, 0 };
    fwrite(avih, 4, 14, f);
    fourcc(f, "LIST"); put32le(f, 4 + (8 + 56) + (8 + 40)); fourcc(f, "strl");
    fourcc(f, "strh"); put32le(f, 56);
    fourcc(f, "vids"); put32le(f, 0); /* handler: none (DIB) */
    unsigned strh[10] = { 0, 0, 0, 1, FPS, 0, FRAMES, frame, 0xffffffff, 0 }; /* flags prio/lang init scale rate start length bufsize quality samplesize */
    fwrite(strh, 4, 10, f);
    put16le(f, 0); put16le(f, 0); put16le(f, W); put16le(f, H);
    fourcc(f, "strf"); put32le(f, 40);
    BITMAPINFOHEADER bi = { 40, W, H, 1, 24, BI_RGB, frame, 0, 0, 0, 0 };
    fwrite(&bi, 40, 1, f);
    fourcc(f, "LIST"); put32le(f, movi); fourcc(f, "movi");
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        /* bottom-up rows: the picture's bottom half first */
        for (int y = 0; y < H; y++) {
            int g = gray(gray_y(k, H - 1 - y >= H / 2));
            memset(px + y * W * 3, g, W * 3);
        }
        fourcc(f, "00db"); put32le(f, frame); fwrite(px, frame, 1, f);
    }
    fourcc(f, "idx1"); put32le(f, idx);
    for (int k = 0; k < FRAMES; k++) {
        fourcc(f, "00db"); put32le(f, 0x10); put32le(f, 4 + k * (8 + frame)); put32le(f, frame);
    }
    fclose(f);
    free(px);
}

/* crates/kura/src/vnv.rs: "VNV1" w h tracks, the track (codec, frames, table), the frames,
 * then the trailer (rate, length, no sound) */
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

static double secs(LONGLONG t) { return t / 1e7; }

/* the shown frame's two halves through GetCurrentImage (which frame it is: by its gray) */
static void image(IBasicVideo *bv, const char *l) {
    long size = 0;
    HRESULT hr = bv->lpVtbl->GetCurrentImage(bv, &size, NULL);
    printf("%s: GetCurrentImage size %08lx", l, hr);
    if (FAILED(hr)) { printf("\n"); return; }
    long *buf = calloc(1, size + 16);
    hr = bv->lpVtbl->GetCurrentImage(bv, &size, buf);
    BITMAPINFOHEADER *bi = (BITMAPINFOHEADER *)buf;
    printf(" get %08lx %ldx%ld", hr, bi->biWidth, bi->biHeight);
    if (SUCCEEDED(hr) && (bi->biBitCount == 32 || bi->biBitCount == 24)) {
        int bpp = bi->biBitCount / 8, stride = (bi->biWidth * bpp + 3) & ~3;
        unsigned char *bits = (unsigned char *)bi + bi->biSize + (bi->biCompression == BI_BITFIELDS ? 12 : 0);
        for (int half = 0; half < 2; half++) {
            int y = half ? bi->biHeight - 3 : 2; /* top-down y */
            int row = bi->biHeight > 0 ? bi->biHeight - 1 - y : y;
            unsigned char *p = bits + row * stride + 5 * bpp;
            /* which frame: the gray of the top half */
            int g = p[1], frame = -1;
            for (int k = 0; k < FRAMES; k++)
                if (abs(gray(gray_y(k, half)) - g) <= 2) frame = k;
            printf(" %s %02x%02x%02x frame %d", half ? "bottom" : "top", p[2], p[1], p[0], frame);
        }
    }
    printf("\n");
    free(buf);
}

static IMediaEventEx *ev;
static DWORD t0;
/* the events until EC_COMPLETE (or ms): their codes, and when it came */
static void events(const char *l, int ms, int lo, int hi) {
    DWORD start = GetTickCount();
    long code, p1, p2;
    int got = 0, n = 0;
    while ((int)(GetTickCount() - start) < ms) {
        LONG_PTR a, b;
        HRESULT hr = ev->lpVtbl->GetEvent(ev, &code, &a, &b, 10);
        if (hr == S_OK) {
            ev->lpVtbl->FreeEventParams(ev, code, a, b);
            if (code == EC_COMPLETE) {
                int t = GetTickCount() - start;
                printf("%s: EC_COMPLETE p1 %08lx after %s\n", l, (long)a, t >= lo && t <= hi ? "the expected time" : "an unexpected time");
                if (t < lo || t > hi) fprintf(stderr, "%s: %d ms (%d..%d)\n", l, t, lo, hi);
                got = 1;
                break;
            }
            if (n++ < 8 && code != 0x0d && code != 0x0e && code != 0x53 /* EC_CLOCK_CHANGED, EC_PAUSED, EC_VMR_RENDERDEVICE_SET */) printf("%s: event %lx\n", l, code);
        }
    }
    if (!got) printf("%s: no EC_COMPLETE in %d ms\n", l, ms);
}

/* a VMR (7 or 9: the same calls) in windowless mode on window `win`, as Artemis builds it (the
 * VMR-7): AddFilter, the rendering mode, the clipping window, then RenderFile (which renders
 * into it) */
static void vmr(HWND win, const char *L, REFCLSID clsid, REFIID config, REFIID windowless) {
    IGraphBuilder *gb;
    IBaseFilter *vmr;
    HRESULT hr = CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb);
    hr = CoCreateInstance(clsid, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&vmr);
    printf("%s: create %08lx\n", L, hr);
    if (FAILED(hr)) return;
    printf("%s: AddFilter %08lx\n", L, gb->lpVtbl->AddFilter(gb, vmr, L"Video Mixing Renderer"));
    IVMRWindowlessControl9 *wc = NULL;
    hr = vmr->lpVtbl->QueryInterface(vmr, windowless, (void **)&wc);
    printf("%s: windowless control before the mode %08lx\n", L, hr);
    if (wc) wc->lpVtbl->Release(wc), wc = NULL;
    IVMRFilterConfig9 *fc;
    hr = vmr->lpVtbl->QueryInterface(vmr, config, (void **)&fc);
    printf("%s: filter config %08lx\n", L, hr);
    if (FAILED(hr)) return;
    DWORD mode = 0;
    hr = fc->lpVtbl->GetRenderingMode(fc, &mode);
    printf("%s: GetRenderingMode %08lx %lu\n", L, hr, mode);
    printf("%s: SetRenderingMode %08lx\n", L, fc->lpVtbl->SetRenderingMode(fc, VMR9Mode_Windowless));
    hr = fc->lpVtbl->GetRenderingMode(fc, &mode);
    printf("%s: GetRenderingMode now %08lx %lu\n", L, hr, mode);
    fc->lpVtbl->Release(fc);
    hr = vmr->lpVtbl->QueryInterface(vmr, windowless, (void **)&wc);
    printf("%s: windowless control %08lx\n", L, hr);
    if (FAILED(hr)) return;
    printf("%s: SetVideoClippingWindow %08lx\n", L, wc->lpVtbl->SetVideoClippingWindow(wc, win));
    long w = -1, h = -1, aw = -1, ah = -1;
    hr = wc->lpVtbl->GetNativeVideoSize(wc, &w, &h, &aw, &ah);
    printf("%s: GetNativeVideoSize before %08lx %ldx%ld %ld:%ld\n", L, hr, w, h, aw, ah);
    hr = gb->lpVtbl->RenderFile(gb, L"probe.avi", NULL);
    printf("%s: RenderFile %08lx\n", L, hr);
    if (FAILED(hr)) return;
    IEnumFilters *ef;
    IBaseFilter *f;
    int filters = 0, ours = 0;
    gb->lpVtbl->EnumFilters(gb, &ef);
    while (ef->lpVtbl->Next(ef, 1, &f, NULL) == S_OK) filters++, ours |= f == vmr, f->lpVtbl->Release(f);
    ef->lpVtbl->Release(ef);
    printf("%s: our renderer among the filters %d\n", L, ours);
    hr = wc->lpVtbl->GetNativeVideoSize(wc, &w, &h, &aw, &ah);
    printf("%s: GetNativeVideoSize %08lx %ldx%ld %ld:%ld\n", L, hr, w, h, aw, ah);
    hr = wc->lpVtbl->GetMinIdealVideoSize(wc, &w, &h);
    printf("%s: GetMinIdealVideoSize %08lx %ldx%ld\n", L, hr, w, h);
    hr = wc->lpVtbl->GetMaxIdealVideoSize(wc, &w, &h);
    printf("%s: GetMaxIdealVideoSize %08lx %ldx%ld\n", L, hr, w, h);
    DWORD ar = 99;
    hr = wc->lpVtbl->GetAspectRatioMode(wc, &ar);
    printf("%s: GetAspectRatioMode %08lx %lu\n", L, hr, ar);
    RECT s = { -1, -1, -1, -1 }, d = { -1, -1, -1, -1 };
    hr = wc->lpVtbl->GetVideoPosition(wc, &s, &d);
    printf("%s: GetVideoPosition %08lx src %ld %ld %ld %ld dst %ld %ld %ld %ld\n", L, hr, s.left, s.top, s.right, s.bottom, d.left, d.top, d.right, d.bottom);
    RECT nd = { 8, 4, 136, 100 };
    printf("%s: SetVideoPosition dst %08lx\n", L, wc->lpVtbl->SetVideoPosition(wc, NULL, &nd));
    RECT ns = { 0, 0, 32, 24 };
    printf("%s: SetVideoPosition src %08lx\n", L, wc->lpVtbl->SetVideoPosition(wc, &ns, NULL));
    RECT bad = { 0, 0, 128, 24 };
    printf("%s: SetVideoPosition src too big %08lx\n", L, wc->lpVtbl->SetVideoPosition(wc, &bad, NULL));
    printf("%s: SetVideoPosition none %08lx\n", L, wc->lpVtbl->SetVideoPosition(wc, NULL, NULL));
    hr = wc->lpVtbl->GetVideoPosition(wc, &s, &d);
    printf("%s: GetVideoPosition now %08lx src %ld %ld %ld %ld dst %ld %ld %ld %ld\n", L, hr, s.left, s.top, s.right, s.bottom, d.left, d.top, d.right, d.bottom);
    printf("%s: SetAspectRatioMode %08lx\n", L, wc->lpVtbl->SetAspectRatioMode(wc, VMR9ARMode_LetterBox));
    hr = wc->lpVtbl->GetAspectRatioMode(wc, &ar);
    printf("%s: GetAspectRatioMode now %08lx %lu\n", L, hr, ar);
    /* the graph's own video window and basic video: no filter of it has a window */
    IVideoWindow *vw;
    IBasicVideo *bv;
    gb->lpVtbl->QueryInterface(gb, &IID_IVideoWindow, (void **)&vw);
    gb->lpVtbl->QueryInterface(gb, &IID_IBasicVideo, (void **)&bv);
    long v = 0;
    printf("%s: get_Visible %08lx\n", L, vw->lpVtbl->get_Visible(vw, &v));
    hr = bv->lpVtbl->GetVideoSize(bv, &w, &h);
    printf("%s: GetVideoSize %08lx\n", L, hr);
    vw->lpVtbl->Release(vw);
    bv->lpVtbl->Release(bv);

    IMediaControl *mc;
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaControl, (void **)&mc);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaEventEx, (void **)&ev);
    OAFilterState st;
    mc->lpVtbl->Pause(mc);
    mc->lpVtbl->GetState(mc, 5000, &st);
    BYTE *dib = NULL;
    hr = wc->lpVtbl->GetCurrentImage(wc, &dib);
    printf("%s: GetCurrentImage %08lx", L, hr);
    if (SUCCEEDED(hr) && dib) {
        BITMAPINFOHEADER *bi = (BITMAPINFOHEADER *)dib;
        printf(" %ldx%ld %d bits", bi->biWidth, bi->biHeight, bi->biBitCount);
        if (bi->biBitCount == 32) {
            unsigned char *bits = dib + bi->biSize;
            int row = bi->biHeight > 0 ? bi->biHeight - 1 - 2 : 2;
            unsigned char *p = bits + row * bi->biWidth * 4 + 5 * 4;
            int frame = -1;
            for (int k = 0; k < FRAMES; k++)
                if (abs(gray(gray_y(k, 0)) - p[1]) <= 2) frame = k;
            printf(" top frame %d", frame);
        }
        CoTaskMemFree(dib);
    }
    printf("\n");
    HDC dc = GetDC(win);
    printf("%s: RepaintVideo %08lx\n", L, wc->lpVtbl->RepaintVideo(wc, win, dc));
    ReleaseDC(win, dc);
    printf("%s: DisplayModeChanged %08lx\n", L, wc->lpVtbl->DisplayModeChanged(wc));
    printf("%s: SetBorderColor %08lx\n", L, wc->lpVtbl->SetBorderColor(wc, 0x123456));
    COLORREF bc = 0;
    hr = wc->lpVtbl->GetBorderColor(wc, &bc);
    printf("%s: GetBorderColor %08lx %06lx\n", L, hr, bc);
    printf("%s: Run %08lx\n", L, mc->lpVtbl->Run(mc));
    char l[32];
    snprintf(l, sizeof l, "%s run", L);
    events(l, 4000, 0, 3000);
    mc->lpVtbl->Stop(mc);
    ev->lpVtbl->Release(ev);
    mc->lpVtbl->Release(mc);
    wc->lpVtbl->Release(wc);
    vmr->lpVtbl->Release(vmr);
    gb->lpVtbl->Release(gb);
    printf("%s: released\n", L);
}

static LRESULT CALLBACK wndproc(HWND h, UINT m, WPARAM w, LPARAM l) { return DefWindowProcA(h, m, w, l); }

int main(void) {
    write_avi("probe.avi");
    write_vnv("probe.avi.vnv");
    CoInitialize(NULL);
    IGraphBuilder *gb;
    HRESULT hr = CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb);
    printf("create %08lx\n", hr);
    if (FAILED(hr)) return 1;
    printf("RenderFile missing %08lx\n", gb->lpVtbl->RenderFile(gb, L"nothere.avi", NULL));
    hr = gb->lpVtbl->RenderFile(gb, L"probe.avi", NULL);
    printf("RenderFile %08lx\n", hr);
    if (FAILED(hr)) return 1;
    IMediaControl *mc; IMediaSeeking *ms; IMediaPosition *mp; IVideoWindow *vw; IBasicVideo *bv;
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaControl, (void **)&mc);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaEventEx, (void **)&ev);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaSeeking, (void **)&ms);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaPosition, (void **)&mp);
    gb->lpVtbl->QueryInterface(gb, &IID_IVideoWindow, (void **)&vw);
    gb->lpVtbl->QueryInterface(gb, &IID_IBasicVideo, (void **)&bv);

    /* seeking */
    LONGLONG d = -1, cur = -1, stop = -1;
    DWORD caps = 0;
    /* (each call before its printf: arguments are evaluated in no set order) */
    hr = ms->lpVtbl->GetDuration(ms, &d);
    printf("GetDuration %08lx %lld\n", hr, d);
    hr = ms->lpVtbl->GetStopPosition(ms, &stop);
    printf("GetStopPosition %08lx %lld\n", hr, stop);
    hr = ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("GetCurrentPosition %08lx %lld\n", hr, cur);
    ms->lpVtbl->GetCapabilities(ms, &caps);
    printf("caps absolute %d forwards %d backwards %d current %d stop %d duration %d\n", !!(caps & AM_SEEKING_CanSeekAbsolute), !!(caps & AM_SEEKING_CanSeekForwards),
           !!(caps & AM_SEEKING_CanSeekBackwards), !!(caps & AM_SEEKING_CanGetCurrentPos), !!(caps & AM_SEEKING_CanGetStopPos), !!(caps & AM_SEEKING_CanGetDuration));
    GUID tf;
    ms->lpVtbl->GetTimeFormat(ms, &tf);
    printf("time format media %d, IsFormatSupported media %08lx\n", IsEqualGUID(&tf, &TIME_FORMAT_MEDIA_TIME), ms->lpVtbl->IsFormatSupported(ms, &TIME_FORMAT_MEDIA_TIME));
    REFTIME rt = -1;
    mp->lpVtbl->get_Duration(mp, &rt);
    printf("get_Duration %.3f\n", rt);

    /* the picture */
    long vwid = 0, vhei = 0;
    hr = bv->lpVtbl->GetVideoSize(bv, &vwid, &vhei);
    printf("GetVideoSize %08lx %ldx%ld\n", hr, vwid, vhei);
    REFTIME avg = 0;
    hr = bv->lpVtbl->get_AvgTimePerFrame(bv, &avg);
    printf("get_AvgTimePerFrame %08lx %.4f\n", hr, avg);
    long l, t, w, h;
    bv->lpVtbl->GetSourcePosition(bv, &l, &t, &w, &h);
    hr = bv->lpVtbl->IsUsingDefaultSource(bv);
    printf("source %ld %ld %ld %ld default %08lx\n", l, t, w, h, hr);
    bv->lpVtbl->GetDestinationPosition(bv, &l, &t, &w, &h);
    hr = bv->lpVtbl->IsUsingDefaultDestination(bv);
    printf("destination %ld %ld %ld %ld default %08lx\n", l, t, w, h, hr);
    long v = 0;
    vw->lpVtbl->get_AutoShow(vw, &v);
    printf("AutoShow %ld\n", v);
    vw->lpVtbl->get_Visible(vw, &v);
    printf("Visible %ld\n", v);
    vw->lpVtbl->get_WindowStyle(vw, &v);
    printf("WindowStyle child %d caption %d\n", !!(v & WS_CHILD), (v & WS_CAPTION) == WS_CAPTION);

    /* a game's window as the owner */
    WNDCLASSA wc = { 0, wndproc, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "probewin" };
    RegisterClassA(&wc);
    HWND win = CreateWindowA("probewin", "probe", WS_POPUP | WS_VISIBLE, 0, 0, 200, 150, 0, 0, wc.hInstance, 0);
    printf("put_Owner %08lx\n", vw->lpVtbl->put_Owner(vw, (OAHWND)win));
    printf("put_WindowStyle %08lx\n", vw->lpVtbl->put_WindowStyle(vw, WS_CHILD | WS_CLIPSIBLINGS));
    printf("SetWindowPosition %08lx\n", vw->lpVtbl->SetWindowPosition(vw, 10, 10, 128, 96));
    vw->lpVtbl->GetWindowPosition(vw, &l, &t, &w, &h);
    printf("window %ld %ld %ld %ld\n", l, t, w, h);
    bv->lpVtbl->GetDestinationPosition(bv, &l, &t, &w, &h);
    printf("destination now %ld %ld %ld %ld\n", l, t, w, h);
    OAHWND o = 0;
    vw->lpVtbl->get_Owner(vw, &o);
    printf("owner is ours %d\n", (HWND)o == win);
    printf("SetNotifyWindow %08lx\n", ev->lpVtbl->SetNotifyWindow(ev, (OAHWND)win, WM_APP + 1, 0x1234));

    /* paused at the start: the first frame */
    OAFilterState st = -1;
    printf("Pause %08lx\n", mc->lpVtbl->Pause(mc));
    hr = mc->lpVtbl->GetState(mc, 5000, &st);
    printf("GetState %08lx %ld\n", hr, st);
    vw->lpVtbl->get_Visible(vw, &v);
    printf("Visible after pause %ld\n", v);
    image(bv, "paused at 0");
    /* seek while paused */
    cur = 7500000;
    hr = ms->lpVtbl->SetPositions(ms, &cur, AM_SEEKING_AbsolutePositioning | AM_SEEKING_ReturnTime, NULL, AM_SEEKING_NoPositioning);
    printf("SetPositions %08lx returned %lld\n", hr, cur);
    mc->lpVtbl->GetState(mc, 5000, &st);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position %lld\n", cur);
    image(bv, "paused at 0.75");

    /* run to the end: 1.25 s */
    HANDLE eh = 0;
    ev->lpVtbl->GetEventHandle(ev, (OAEVENT *)&eh);
    printf("event set before %lu\n", WaitForSingleObject(eh, 0));
    printf("Run %08lx\n", mc->lpVtbl->Run(mc));
    mc->lpVtbl->GetState(mc, 5000, &st);
    printf("state %ld\n", st);
    Sleep(500);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position after 0.5 s within 0.25 %d\n", cur > 10000000 && cur < 15000000);
    printf("WaitForSingleObject ended %d\n", WaitForSingleObject(eh, 3000) == WAIT_OBJECT_0);
    events("run", 3000, 0, 1500);
    printf("event set after %lu\n", WaitForSingleObject(eh, 0));
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position at the end %lld\n", cur);
    mc->lpVtbl->GetState(mc, 5000, &st);
    printf("state at the end %ld\n", st);
    /* (how many: one per event or fewer, by timing: Windows posted 2 on x86, 3 on x64) */
    MSG m;
    int notes = 0, ours = 1;
    while (PeekMessageA(&m, win, WM_APP + 1, WM_APP + 1, PM_REMOVE)) notes++, ours &= m.lParam == 0x1234;
    printf("notified %d, every lParam ours %d\n", notes > 0, ours);
    long code = 0;
    hr = ev->lpVtbl->WaitForCompletion(ev, 0, &code);
    printf("WaitForCompletion running %08lx %ld\n", hr, code);

    /* stopped: where is it, and does it end again when it runs? */
    printf("Stop %08lx\n", mc->lpVtbl->Stop(mc));
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position after stop %lld\n", cur);
    printf("WaitForCompletion stopped %08lx\n", ev->lpVtbl->WaitForCompletion(ev, 0, &code));
    printf("Run %08lx\n", mc->lpVtbl->Run(mc));
    events("run after stop", 1000, 0, 400);
    mc->lpVtbl->Stop(mc);

    /* from 1.0 s at twice the speed: 0.5 s */
    printf("put_CurrentPosition %08lx\n", mp->lpVtbl->put_CurrentPosition(mp, 1.0));
    mp->lpVtbl->get_CurrentPosition(mp, &rt);
    printf("get_CurrentPosition %.3f\n", rt);
    printf("SetRate %08lx\n", ms->lpVtbl->SetRate(ms, 2.0));
    double r = 0;
    ms->lpVtbl->GetRate(ms, &r);
    printf("GetRate %.2f\n", r);
    mc->lpVtbl->Run(mc);
    events("twice the speed", 3000, 250, 900);
    mc->lpVtbl->Stop(mc);
    ms->lpVtbl->SetRate(ms, 1.0);

    /* a stop position: from 0 to 0.5 s */
    cur = 0, stop = 5000000;
    hr = ms->lpVtbl->SetPositions(ms, &cur, AM_SEEKING_AbsolutePositioning, &stop, AM_SEEKING_AbsolutePositioning);
    printf("SetPositions with stop %08lx\n", hr);
    ms->lpVtbl->GetStopPosition(ms, &stop);
    printf("GetStopPosition %lld\n", stop);
    mc->lpVtbl->Run(mc);
    events("to the stop", 3000, 250, 1000);
    ms->lpVtbl->GetCurrentPosition(ms, &cur);
    printf("position at the stop %lld\n", cur);
    mc->lpVtbl->Stop(mc);

    printf("put_Visible %08lx\n", vw->lpVtbl->put_Visible(vw, OAFALSE));
    printf("put_Owner 0 %08lx\n", vw->lpVtbl->put_Owner(vw, 0));
    bv->lpVtbl->Release(bv);
    vw->lpVtbl->Release(vw);
    mp->lpVtbl->Release(mp);
    ms->lpVtbl->Release(ms);
    ev->lpVtbl->Release(ev);
    mc->lpVtbl->Release(mc);
    gb->lpVtbl->Release(gb);
    printf("released\n");
    vmr(win, "vmr7", &CLSID_VideoMixingRenderer, &IID_IVMRFilterConfig, &IID_IVMRWindowlessControl);
    vmr(win, "vmr9", &CLSID_VideoMixingRenderer9, &IID_IVMRFilterConfig9, &IID_IVMRWindowlessControl9);
    DestroyWindow(win);
    CoUninitialize();
    return 0;
}
