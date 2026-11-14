/* IGraphBuilder::AddSourceFilter and a game's own video renderer in a graph that renders a
 * file, as WillPlus' AdvHD plays its movies (#200, #215): AddSourceFilter of a file that is not
 * there; of an MPEG-1 system stream and an AVI with a ".dat" / ".avi" name (the File Source
 * (Async): its class, its one pin "Output", the type and the file its IFileSourceFilter tells,
 * its pin's IAsyncReader reads, the name it joins the graph by); then the probe's renderer
 * (one input pin taking MEDIATYPE_Video / RGB24 / VIDEOINFOHEADER only, as AdvHD's) added and
 * RenderFile of an uncompressed AVI (64x48, 10 fps, 20 frames, each frame's top and bottom
 * halves two grays, as tests/winref/probes/dshow.c writes it, with the `.vnv` ippai pack would
 * make of it next to it): the type the renderer is given, the graph's IVideoWindow (no video
 * window: E_NOINTERFACE), the samples it gets once the graph runs (their times and the gray of
 * the first ones' halves), the end of the stream and EC_COMPLETE.
 * (The WM ASF Reader AdvHD's ASF movies get is not here: the probe can't write an ASF file
 * Windows' reader opens. crates/sake/src/dshow_mpeg.rs tests it.)
 * libs: -lole32 -lstrmiids -loleaut32 -luuid -luser32 */
#include "../probe.h"
#include <stddef.h>
#include <wchar.h>
#include <dshow.h>

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

/* crates/kura/src/vnv.rs: uncompressed I420, the same pictures in video range */
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

static void write_bytes(const char *path, const void *b, int n) {
    FILE *f = fopen(path, "wb");
    fwrite(b, n, 1, f);
    fclose(f);
}

static void wcopy(WCHAR *d, const WCHAR *s) { while ((*d++ = *s++)) {} }

static const char *guid_name(const GUID *g) {
    static char buf[64];
    if (IsEqualGUID(g, &MEDIATYPE_Stream)) return "stream";
    if (IsEqualGUID(g, &MEDIATYPE_Video)) return "video";
    if (IsEqualGUID(g, &MEDIASUBTYPE_MPEG1System)) return "mpeg1system";
    if (IsEqualGUID(g, &MEDIASUBTYPE_Avi)) return "avi";
    if (IsEqualGUID(g, &MEDIASUBTYPE_RGB24)) return "rgb24";
    if (IsEqualGUID(g, &MEDIASUBTYPE_RGB32)) return "rgb32";
    if (IsEqualGUID(g, &FORMAT_VideoInfo)) return "videoinfo";
    if (IsEqualGUID(g, &GUID_NULL)) return "null";
    if (IsEqualGUID(g, &CLSID_AsyncReader)) return "File Source (Async)";
    snprintf(buf, sizeof buf, "{%08lx-%04x-%04x-...}", g->Data1, g->Data2, g->Data3);
    return buf;
}

/* ------------------------------------------------------------------------------------------ */
/* the probe's renderer: one input pin, RGB24 only, every sample taken at once */

typedef struct { IEnumPins e; LONG refs; IPin *pin; int at; } Enum;
static HRESULT WINAPI en_qi(IEnumPins *e, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IEnumPins)) { *out = e; e->lpVtbl->AddRef(e); return S_OK; }
    *out = 0; return E_NOINTERFACE;
}
static ULONG WINAPI en_add(IEnumPins *e) { return InterlockedIncrement(&((Enum *)e)->refs); }
static ULONG WINAPI en_rel(IEnumPins *e) { LONG n = InterlockedDecrement(&((Enum *)e)->refs); if (!n) free(e); return n; }
static HRESULT WINAPI en_next(IEnumPins *e, ULONG n, IPin **out, ULONG *got) {
    Enum *x = (Enum *)e;
    ULONG k = 0;
    if (n && !x->at) { out[0] = x->pin; x->pin->lpVtbl->AddRef(x->pin); x->at = 1; k = 1; }
    if (got) *got = k;
    return k == n ? S_OK : S_FALSE;
}
static HRESULT WINAPI en_skip(IEnumPins *e, ULONG n) { Enum *x = (Enum *)e; if (n) { int was = x->at; x->at = 1; return was || n > 1 ? S_FALSE : S_OK; } return S_OK; }
static HRESULT WINAPI en_reset(IEnumPins *e) { ((Enum *)e)->at = 0; return S_OK; }
static HRESULT WINAPI en_clone(IEnumPins *e, IEnumPins **out);
static IEnumPinsVtbl en_vt = { en_qi, en_add, en_rel, en_next, en_skip, en_reset, en_clone };
static IEnumPins *enum_of(IPin *p) {
    Enum *x = calloc(1, sizeof *x);
    x->e.lpVtbl = &en_vt; x->refs = 1; x->pin = p;
    return &x->e;
}
static HRESULT WINAPI en_clone(IEnumPins *e, IEnumPins **out) { *out = enum_of(((Enum *)e)->pin); ((Enum *)*out)->at = ((Enum *)e)->at; return S_OK; }

typedef struct Filter {
    IBaseFilter bf;
    IPin pin;
    IMemInputPin mi;
    LONG refs;
    IFilterGraph *graph;
    IMediaEventSink *sink;
    FILTER_STATE state;
    IPin *peer;
    AM_MEDIA_TYPE mt;
    int offered, refused;
    volatile LONG samples, eos;
    REFERENCE_TIME t0[FRAMES + 4];
    int top[FRAMES + 4], bottom[FRAMES + 4];
    long len0;
} Filter;

#define OF(p, m) ((Filter *)((char *)(p) - offsetof(Filter, m)))
static ULONG add(Filter *f) { return InterlockedIncrement(&f->refs); }
static ULONG rel(Filter *f) { return InterlockedDecrement(&f->refs); }

static HRESULT WINAPI bf_qi(IBaseFilter *b, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IPersist) || IsEqualIID(iid, &IID_IMediaFilter) || IsEqualIID(iid, &IID_IBaseFilter)) { *out = b; add(OF(b, bf)); return S_OK; }
    *out = 0; return E_NOINTERFACE;
}
static ULONG WINAPI bf_add(IBaseFilter *b) { return add(OF(b, bf)); }
static ULONG WINAPI bf_rel(IBaseFilter *b) { return rel(OF(b, bf)); }
static HRESULT WINAPI bf_class(IBaseFilter *b, CLSID *c) { *c = GUID_NULL; return S_OK; }
static HRESULT WINAPI bf_stop(IBaseFilter *b) { OF(b, bf)->state = State_Stopped; return S_OK; }
static HRESULT WINAPI bf_pause(IBaseFilter *b) { OF(b, bf)->state = State_Paused; return S_OK; }
static HRESULT WINAPI bf_run(IBaseFilter *b, REFERENCE_TIME t) { OF(b, bf)->state = State_Running; return S_OK; }
static HRESULT WINAPI bf_state(IBaseFilter *b, DWORD ms, FILTER_STATE *s) { *s = OF(b, bf)->state; return S_OK; }
static HRESULT WINAPI bf_set_clock(IBaseFilter *b, IReferenceClock *c) { return S_OK; }
static HRESULT WINAPI bf_get_clock(IBaseFilter *b, IReferenceClock **c) { *c = 0; return S_OK; }
static HRESULT WINAPI bf_pins(IBaseFilter *b, IEnumPins **e) { *e = enum_of(&OF(b, bf)->pin); return S_OK; }
static HRESULT WINAPI bf_find(IBaseFilter *b, LPCWSTR id, IPin **p) {
    if (wcscmp(id, L"In")) { *p = 0; return VFW_E_NOT_FOUND; }
    *p = &OF(b, bf)->pin; add(OF(b, bf)); return S_OK;
}
static HRESULT WINAPI bf_info(IBaseFilter *b, FILTER_INFO *i) {
    Filter *f = OF(b, bf);
    wcopy(i->achName, L"renderer");
    i->pGraph = f->graph;
    if (f->graph) f->graph->lpVtbl->AddRef(f->graph);
    return S_OK;
}
static HRESULT WINAPI bf_join(IBaseFilter *b, IFilterGraph *g, LPCWSTR name) {
    Filter *f = OF(b, bf);
    f->graph = g;
    f->sink = 0;
    if (g && SUCCEEDED(g->lpVtbl->QueryInterface(g, &IID_IMediaEventSink, (void **)&f->sink))) f->sink->lpVtbl->Release(f->sink);
    return S_OK;
}
static HRESULT WINAPI bf_vendor(IBaseFilter *b, LPWSTR *v) { return E_NOTIMPL; }
static IBaseFilterVtbl bf_vt = { bf_qi, bf_add, bf_rel, bf_class, bf_stop, bf_pause, bf_run, bf_state, bf_set_clock, bf_get_clock, bf_pins, bf_find, bf_info, bf_join, bf_vendor };

static HRESULT WINAPI pin_qi(IPin *p, REFIID iid, void **out) {
    Filter *f = OF(p, pin);
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IPin)) *out = &f->pin;
    else if (IsEqualIID(iid, &IID_IMemInputPin)) *out = &f->mi;
    else { *out = 0; return E_NOINTERFACE; }
    add(f);
    return S_OK;
}
static ULONG WINAPI pin_add(IPin *p) { return add(OF(p, pin)); }
static ULONG WINAPI pin_rel(IPin *p) { return rel(OF(p, pin)); }
static HRESULT WINAPI pin_connect(IPin *p, IPin *to, const AM_MEDIA_TYPE *want) { return E_UNEXPECTED; }
/* RGB24 VIDEOINFOHEADER only (AdvHD's renderer: its CheckMediaType) */
static HRESULT WINAPI pin_receive(IPin *p, IPin *from, const AM_MEDIA_TYPE *mt) {
    Filter *f = OF(p, pin);
    if (f->peer) return VFW_E_ALREADY_CONNECTED;
    f->offered++;
    if (!IsEqualGUID(&mt->majortype, &MEDIATYPE_Video) || !IsEqualGUID(&mt->subtype, &MEDIASUBTYPE_RGB24) || !IsEqualGUID(&mt->formattype, &FORMAT_VideoInfo) || mt->cbFormat < sizeof(VIDEOINFOHEADER)) {
        f->refused++;
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    f->mt = *mt;
    f->mt.pbFormat = malloc(mt->cbFormat);
    memcpy(f->mt.pbFormat, mt->pbFormat, mt->cbFormat);
    f->peer = from;
    from->lpVtbl->AddRef(from);
    return S_OK;
}
static HRESULT WINAPI pin_disconnect(IPin *p) {
    Filter *f = OF(p, pin);
    if (!f->peer) return S_FALSE;
    f->peer->lpVtbl->Release(f->peer);
    f->peer = 0;
    return S_OK;
}
static HRESULT WINAPI pin_connected(IPin *p, IPin **out) {
    Filter *f = OF(p, pin);
    *out = f->peer;
    if (!f->peer) return VFW_E_NOT_CONNECTED;
    f->peer->lpVtbl->AddRef(f->peer);
    return S_OK;
}
static HRESULT WINAPI pin_mt(IPin *p, AM_MEDIA_TYPE *mt) {
    Filter *f = OF(p, pin);
    if (!f->peer) return VFW_E_NOT_CONNECTED;
    *mt = f->mt;
    if (f->mt.cbFormat) { mt->pbFormat = CoTaskMemAlloc(f->mt.cbFormat); memcpy(mt->pbFormat, f->mt.pbFormat, f->mt.cbFormat); }
    return S_OK;
}
static HRESULT WINAPI pin_info(IPin *p, PIN_INFO *i) {
    Filter *f = OF(p, pin);
    i->pFilter = &f->bf; add(f);
    i->dir = PINDIR_INPUT;
    wcopy(i->achName, L"In");
    return S_OK;
}
static HRESULT WINAPI pin_dir(IPin *p, PIN_DIRECTION *d) { *d = PINDIR_INPUT; return S_OK; }
static HRESULT WINAPI pin_id(IPin *p, LPWSTR *id) { *id = CoTaskMemAlloc(6); wcopy(*id, L"In"); return S_OK; }
static HRESULT WINAPI pin_accept(IPin *p, const AM_MEDIA_TYPE *mt) { return IsEqualGUID(&mt->subtype, &MEDIASUBTYPE_RGB24) ? S_OK : S_FALSE; }
static HRESULT WINAPI pin_types(IPin *p, IEnumMediaTypes **e) { *e = 0; return E_NOTIMPL; }
static HRESULT WINAPI pin_internal(IPin *p, IPin **a, ULONG *n) { return E_NOTIMPL; }
static HRESULT WINAPI pin_eos(IPin *p) {
    Filter *f = OF(p, pin);
    InterlockedIncrement(&f->eos);
    if (f->sink) f->sink->lpVtbl->Notify(f->sink, EC_COMPLETE, S_OK, (LONG_PTR)&f->bf);
    return S_OK;
}
static HRESULT WINAPI pin_flush(IPin *p) { return S_OK; }
static HRESULT WINAPI pin_segment(IPin *p, REFERENCE_TIME a, REFERENCE_TIME b, double r) { return S_OK; }
static IPinVtbl pin_vt = { pin_qi, pin_add, pin_rel, pin_connect, pin_receive, pin_disconnect, pin_connected, pin_mt, pin_info, pin_dir, pin_id, pin_accept, pin_types, pin_internal, pin_eos, pin_flush, pin_flush, pin_segment };

static HRESULT WINAPI mi_qi(IMemInputPin *m, REFIID iid, void **out) { return pin_qi(&OF(m, mi)->pin, iid, out); }
static ULONG WINAPI mi_add(IMemInputPin *m) { return add(OF(m, mi)); }
static ULONG WINAPI mi_rel(IMemInputPin *m) { return rel(OF(m, mi)); }
static HRESULT WINAPI mi_get_alloc(IMemInputPin *m, IMemAllocator **a) { *a = 0; return VFW_E_NO_ALLOCATOR; }
static HRESULT WINAPI mi_notify_alloc(IMemInputPin *m, IMemAllocator *a, BOOL ro) { return S_OK; }
static HRESULT WINAPI mi_reqs(IMemInputPin *m, ALLOCATOR_PROPERTIES *p) { return E_NOTIMPL; }
/* a sample's picture: the gray at (5, 2) and (5, H - 3) (top-down), bottom-up RGB24 rows */
static HRESULT WINAPI mi_receive(IMemInputPin *m, IMediaSample *s) {
    Filter *f = OF(m, mi);
    LONG k = f->samples;
    if (k < FRAMES + 4) {
        REFERENCE_TIME a = -1, b = -1;
        BYTE *p = 0;
        s->lpVtbl->GetTime(s, &a, &b);
        s->lpVtbl->GetPointer(s, &p);
        if (!k) f->len0 = s->lpVtbl->GetActualDataLength(s);
        int stride = (W * 3 + 3) & ~3;
        f->t0[k] = a;
        f->top[k] = p[(H - 1 - 2) * stride + 5 * 3 + 1];
        f->bottom[k] = p[2 * stride + 5 * 3 + 1];
    }
    InterlockedIncrement(&f->samples);
    return S_OK;
}
static HRESULT WINAPI mi_receive_multiple(IMemInputPin *m, IMediaSample **s, long n, long *done) {
    for (long i = 0; i < n; i++) mi_receive(m, s[i]);
    *done = n;
    return S_OK;
}
static HRESULT WINAPI mi_can_block(IMemInputPin *m) { return S_FALSE; }
static IMemInputPinVtbl mi_vt = { mi_qi, mi_add, mi_rel, mi_get_alloc, mi_notify_alloc, mi_reqs, mi_receive, mi_receive_multiple, mi_can_block };

/* ------------------------------------------------------------------------------------------ */

static IGraphBuilder *new_graph(void) {
    IGraphBuilder *g = 0;
    CoCreateInstance(&CLSID_FilterGraph, 0, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&g);
    return g;
}

/* a source AddSourceFilter gave: class, pins, file, reads */
static void source(const char *l, IGraphBuilder *g, const WCHAR *file, const WCHAR *name, LONGLONG size) {
    IBaseFilter *f = (IBaseFilter *)0x1;
    HRESULT hr = g->lpVtbl->AddSourceFilter(g, file, name, &f);
    printf("%s: AddSourceFilter %08lx filter %s\n", l, hr, f ? "set" : "NULL");
    if (FAILED(hr) || !f) return;
    CLSID c;
    f->lpVtbl->GetClassID(f, &c);
    printf("%s: class %s\n", l, guid_name(&c));
    IEnumPins *e = 0;
    IPin *p = 0;
    int n = 0;
    if (SUCCEEDED(f->lpVtbl->EnumPins(f, &e))) {
        while (e->lpVtbl->Next(e, 1, &p, 0) == S_OK) {
            PIN_INFO i;
            p->lpVtbl->QueryPinInfo(p, &i);
            if (i.pFilter) i.pFilter->lpVtbl->Release(i.pFilter);
            printf("%s: pin %d %ls %s\n", l, n++, i.achName, i.dir == PINDIR_OUTPUT ? "out" : "in");
            p->lpVtbl->Release(p);
        }
        e->lpVtbl->Release(e);
    }
    p = 0;
    hr = f->lpVtbl->FindPin(f, L"Output", &p);
    printf("%s: FindPin Output %08lx\n", l, hr);
    IFileSourceFilter *fs = 0;
    hr = f->lpVtbl->QueryInterface(f, &IID_IFileSourceFilter, (void **)&fs);
    printf("%s: IFileSourceFilter %08lx\n", l, hr);
    if (fs) {
        LPOLESTR cur = 0;
        AM_MEDIA_TYPE mt;
        memset(&mt, 0xcc, sizeof mt);
        hr = fs->lpVtbl->GetCurFile(fs, &cur, &mt);
        printf("%s: GetCurFile %08lx %ls major %s sub %s\n", l, hr, cur ? cur : L"(none)", guid_name(&mt.majortype), guid_name(&mt.subtype));
        if (cur) CoTaskMemFree(cur);
        if (SUCCEEDED(hr) && mt.cbFormat && mt.pbFormat) CoTaskMemFree(mt.pbFormat);
        hr = fs->lpVtbl->Load(fs, file, 0);
        printf("%s: Load again %s\n", l, SUCCEEDED(hr) ? "ok" : "fails");
        fs->lpVtbl->Release(fs);
    }
    IAsyncReader *r = 0;
    if (p && SUCCEEDED(p->lpVtbl->QueryInterface(p, &IID_IAsyncReader, (void **)&r))) {
        LONGLONG total = -1, avail = -1;
        hr = r->lpVtbl->Length(r, &total, &avail);
        printf("%s: Length %08lx %s\n", l, hr, total == size && avail == size ? "the file's size" : "other");
        BYTE buf[8] = { 0 };
        hr = r->lpVtbl->SyncRead(r, 0, 4, buf);
        printf("%s: SyncRead 0 4 %08lx %02x%02x%02x%02x\n", l, hr, buf[0], buf[1], buf[2], buf[3]);
        hr = r->lpVtbl->SyncRead(r, size - 2, 4, buf);
        printf("%s: SyncRead end-2 4 %08lx\n", l, hr);
        r->lpVtbl->Release(r);
    } else if (p)
        printf("%s: no IAsyncReader\n", l);
    if (p) p->lpVtbl->Release(p);
    IBaseFilter *byname = 0;
    hr = g->lpVtbl->FindFilterByName(g, name ? name : file, &byname);
    printf("%s: FindFilterByName %08lx %s\n", l, hr, byname == f ? "the source" : byname ? "another" : "none");
    if (byname) byname->lpVtbl->Release(byname);
    f->lpVtbl->Release(f);
}

int main(void) {
    CoInitialize(0);
    static const unsigned char mpeg[] = { 0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0x80, 0x00, 0x01, 0x00, 0x00, 0x01, 0xb9 };
    write_bytes("movie.dat", mpeg, sizeof mpeg);
    write_avi("clip.avi");
    write_vnv("clip.avi.vnv");
    LONGLONG avi_size = 0;
    {
        HANDLE h = CreateFileA("clip.avi", GENERIC_READ, FILE_SHARE_READ, 0, OPEN_EXISTING, 0, 0);
        LARGE_INTEGER s;
        GetFileSizeEx(h, &s);
        avi_size = s.QuadPart;
        CloseHandle(h);
    }

    IGraphBuilder *g = new_graph();
    if (!g) { printf("skip no filter graph\n"); return 0; }
    IBaseFilter *f = (IBaseFilter *)0x1;
    HRESULT hr = g->lpVtbl->AddSourceFilter(g, L"nothere.dat", L"x", &f);
    printf("missing: AddSourceFilter %08lx filter %s\n", hr, f ? "set" : "NULL");
    hr = g->lpVtbl->AddSourceFilter(g, L"movie.dat", L"x", 0);
    printf("null out: %08lx\n", hr);
    source("mpeg", g, L"movie.dat", L"Movie Source", sizeof mpeg);
    source("avi", g, L"clip.avi", 0, avi_size);
    g->lpVtbl->Release(g);

    /* RenderFile into the probe's renderer */
    g = new_graph();
    static Filter r;
    r.bf.lpVtbl = &bf_vt; r.pin.lpVtbl = &pin_vt; r.mi.lpVtbl = &mi_vt; r.refs = 1;
    hr = g->lpVtbl->AddFilter(g, &r.bf, L"renderer");
    printf("render: AddFilter %08lx\n", hr);
    hr = g->lpVtbl->RenderFile(g, L"clip.avi", 0);
    printf("render: RenderFile %s, the renderer %s\n", SUCCEEDED(hr) ? "ok" : "fails", r.peer ? "connected" : "not connected");
    if (r.peer) {
        VIDEOINFOHEADER *vi = (VIDEOINFOHEADER *)r.mt.pbFormat;
        printf("render: type %s %s %s fixed %d sample %ld\n", guid_name(&r.mt.majortype), guid_name(&r.mt.subtype), guid_name(&r.mt.formattype), r.mt.bFixedSizeSamples, r.mt.lSampleSize);
        printf("render: %ldx%ld %d bits compression %ld size %ld frame %ld\n", vi->bmiHeader.biWidth, vi->bmiHeader.biHeight, vi->bmiHeader.biBitCount, vi->bmiHeader.biCompression, vi->bmiHeader.biSizeImage, (long)vi->AvgTimePerFrame);
    }
    IVideoWindow *vw = 0;
    hr = g->lpVtbl->QueryInterface(g, &IID_IVideoWindow, (void **)&vw);
    if (vw) {
        long v = 0;
        hr = vw->lpVtbl->get_Visible(vw, &v);
        printf("render: IVideoWindow get_Visible %08lx\n", hr);
        vw->lpVtbl->Release(vw);
    }
    IMediaControl *mc = 0;
    IMediaEvent *me = 0;
    g->lpVtbl->QueryInterface(g, &IID_IMediaControl, (void **)&mc);
    g->lpVtbl->QueryInterface(g, &IID_IMediaEvent, (void **)&me);
    hr = mc->lpVtbl->Run(mc);
    printf("render: Run %s\n", SUCCEEDED(hr) ? "ok" : "fails");
    long code = 0;
    hr = me->lpVtbl->WaitForCompletion(me, 8000, &code);
    printf("render: WaitForCompletion %08lx code %lx\n", hr, code);
    /* (the end of the stream reaches the renderer by then, or a moment later) */
    for (int i = 0; i < 300 && !r.eos; i++) Sleep(10);
    printf("render: eos %ld samples %s\n", r.eos, r.samples >= 2 ? "some" : "none");
    if (r.samples) printf("render: first sample length %ld\n", r.len0);
    int ordered = 1, frames_ok = 1;
    LONG n = r.samples < FRAMES + 4 ? r.samples : FRAMES + 4;
    for (LONG k = 0; k < n; k++) {
        if (r.t0[k] % 1000000 != 0 || (k && r.t0[k] <= r.t0[k - 1])) ordered = 0;
        int fr = (int)(r.t0[k] / 1000000);
        if (fr < 0 || fr >= FRAMES || abs(r.top[k] - gray(gray_y(fr, 0))) > 2 || abs(r.bottom[k] - gray(gray_y(fr, 1))) > 2) frames_ok = 0;
    }
    printf("render: times whole frames in order %s, each picture its time's %s\n", ordered ? "yes" : "no", frames_ok ? "yes" : "no");
    if (n) printf("render: first sample time %ld top %02x bottom %02x\n", (long)r.t0[0], r.top[0], r.bottom[0]);
    mc->lpVtbl->Stop(mc);
    me->lpVtbl->Release(me);
    mc->lpVtbl->Release(mc);
    g->lpVtbl->Release(g);
    printf("done\n");
    return 0;
}
