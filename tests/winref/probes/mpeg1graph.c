/* DirectShow's MPEG-1 filters in a graph built by hand, as KiriKiri's krmovie does (#138): the
 * game's own source filter (an IAsyncReader over the stream, here the probe's resource) and
 * its own video renderer (RGB32 samples into a buffer, no clock of its own) added, then
 * CLSID_MPEG1Splitter, CLSID_CMpegVideoCodec, CLSID_CMpegAudioCodec and CLSID_DSoundRender
 * created and connected pin to pin with IGraphBuilder::Connect (krmovie's ConnectFilters:
 * the first free output to the first free input). Prints the pins, the type the renderer is
 * given, the duration, the samples (times, sizes, a picture), EC_COMPLETE, and the same once
 * more after a rewind.
 * libs: -lole32 -lstrmiids -loleaut32 -luuid -luser32 */
#include "../probe.h"
#include <stddef.h>
#include <wchar.h>
#include <dshow.h>

#define W 64
#define H 48

static void wcopy(WCHAR *d, const WCHAR *s) { while ((*d++ = *s++)) {} }

static const unsigned char *data;
static DWORD data_len;

static void guid_name(const char *l, const GUID *g) {
    const char *n = IsEqualGUID(g, &MEDIATYPE_Video) ? "video" : IsEqualGUID(g, &MEDIATYPE_Audio) ? "audio" : IsEqualGUID(g, &MEDIASUBTYPE_RGB32) ? "rgb32" : IsEqualGUID(g, &FORMAT_VideoInfo) ? "videoinfo" : 0;
    if (n)
        printf("%s %s\n", l, n);
    else
        printf("%s {%08lx-...}\n", l, g->Data1);
}

/* ------------------------------------------------------------------------------------------ */
/* an enumerator of one pin */

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

/* ------------------------------------------------------------------------------------------ */
/* a filter of the probe: IBaseFilter with one pin, the pin's interfaces in the same object */

typedef struct Filter {
    IBaseFilter bf;
    IMediaSeeking ms;
    IPin pin;
    IAsyncReader ar;  /* the source's */
    IMemInputPin mi;  /* the renderer's */
    LONG refs;
    int out;          /* the pin is an output */
    const WCHAR *name, *pin_name;
    IFilterGraph *graph;
    IMediaEventSink *sink;
    FILTER_STATE state;
    IPin *peer;
    AM_MEDIA_TYPE mt;
    /* the source's queued requests */
    IMediaSample *req[64];
    DWORD_PTR user[64];
    int nreq;
    int flushing;
    /* what the renderer got */
    int samples, eos, flushes, segments;
    REFERENCE_TIME t0[40], t1[40];
    long lens[40];
    unsigned char pic[W * H * 4];
} Filter;

#define OF(p, m) ((Filter *)((char *)(p) - offsetof(Filter, m)))

static ULONG add(Filter *f) { return InterlockedIncrement(&f->refs); }
static ULONG rel(Filter *f) { return InterlockedDecrement(&f->refs); }

static HRESULT qi(Filter *f, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IPersist) || IsEqualIID(iid, &IID_IMediaFilter) || IsEqualIID(iid, &IID_IBaseFilter))
        *out = &f->bf;
    else if (IsEqualIID(iid, &IID_IMediaSeeking) && !f->out)
        *out = &f->ms;
    else { *out = 0; return E_NOINTERFACE; }
    add(f);
    return S_OK;
}
static HRESULT WINAPI bf_qi(IBaseFilter *b, REFIID iid, void **out) { return qi(OF(b, bf), iid, out); }
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
    Filter *f = OF(b, bf);
    if (wcscmp(id, f->pin_name)) { *p = 0; return VFW_E_NOT_FOUND; }
    *p = &f->pin; add(f); return S_OK;
}
static HRESULT WINAPI bf_info(IBaseFilter *b, FILTER_INFO *i) {
    Filter *f = OF(b, bf);
    wcopy(i->achName, f->name);
    i->pGraph = f->graph;
    if (f->graph) f->graph->lpVtbl->AddRef(f->graph);
    return S_OK;
}
static HRESULT WINAPI bf_join(IBaseFilter *b, IFilterGraph *g, LPCWSTR name) {
    Filter *f = OF(b, bf);
    f->graph = g;
    f->sink = 0;
    /* (as the base classes: the sink not held) */
    if (g && SUCCEEDED(g->lpVtbl->QueryInterface(g, &IID_IMediaEventSink, (void **)&f->sink))) f->sink->lpVtbl->Release(f->sink);
    return S_OK;
}
static HRESULT WINAPI bf_vendor(IBaseFilter *b, LPWSTR *v) { return E_NOTIMPL; }
static IBaseFilterVtbl bf_vt = { bf_qi, bf_add, bf_rel, bf_class, bf_stop, bf_pause, bf_run, bf_state, bf_set_clock, bf_get_clock, bf_pins, bf_find, bf_info, bf_join, bf_vendor };

/* the renderer's IMediaSeeking: the upstream pin's (as the base classes' CRendererPosPassThru) */
static IMediaSeeking *up(IMediaSeeking *m) {
    Filter *f = OF(m, ms);
    IMediaSeeking *s = 0;
    if (f->peer) f->peer->lpVtbl->QueryInterface(f->peer, &IID_IMediaSeeking, (void **)&s);
    if (s) s->lpVtbl->Release(s);
    return s;
}
#define PASS(name, args, call) static HRESULT WINAPI ms_##name args { IMediaSeeking *s = up(m); return s ? s->lpVtbl->name call : E_NOTIMPL; }
static HRESULT WINAPI ms_qi(IMediaSeeking *m, REFIID iid, void **out) { return qi(OF(m, ms), iid, out); }
static ULONG WINAPI ms_add(IMediaSeeking *m) { return add(OF(m, ms)); }
static ULONG WINAPI ms_rel(IMediaSeeking *m) { return rel(OF(m, ms)); }
PASS(GetCapabilities, (IMediaSeeking *m, DWORD *a), (s, a))
PASS(CheckCapabilities, (IMediaSeeking *m, DWORD *a), (s, a))
PASS(IsFormatSupported, (IMediaSeeking *m, const GUID *a), (s, a))
PASS(QueryPreferredFormat, (IMediaSeeking *m, GUID *a), (s, a))
PASS(GetTimeFormat, (IMediaSeeking *m, GUID *a), (s, a))
PASS(IsUsingTimeFormat, (IMediaSeeking *m, const GUID *a), (s, a))
PASS(SetTimeFormat, (IMediaSeeking *m, const GUID *a), (s, a))
PASS(GetDuration, (IMediaSeeking *m, LONGLONG *a), (s, a))
PASS(GetStopPosition, (IMediaSeeking *m, LONGLONG *a), (s, a))
PASS(GetCurrentPosition, (IMediaSeeking *m, LONGLONG *a), (s, a))
PASS(ConvertTimeFormat, (IMediaSeeking *m, LONGLONG *a, const GUID *b, LONGLONG c, const GUID *d), (s, a, b, c, d))
PASS(SetPositions, (IMediaSeeking *m, LONGLONG *a, DWORD b, LONGLONG *c, DWORD d), (s, a, b, c, d))
PASS(GetPositions, (IMediaSeeking *m, LONGLONG *a, LONGLONG *b), (s, a, b))
PASS(GetAvailable, (IMediaSeeking *m, LONGLONG *a, LONGLONG *b), (s, a, b))
PASS(SetRate, (IMediaSeeking *m, double a), (s, a))
PASS(GetRate, (IMediaSeeking *m, double *a), (s, a))
PASS(GetPreroll, (IMediaSeeking *m, LONGLONG *a), (s, a))
static IMediaSeekingVtbl ms_vt = { ms_qi, ms_add, ms_rel, ms_GetCapabilities, ms_CheckCapabilities, ms_IsFormatSupported, ms_QueryPreferredFormat, ms_GetTimeFormat, ms_IsUsingTimeFormat, ms_SetTimeFormat, ms_GetDuration, ms_GetStopPosition, ms_GetCurrentPosition, ms_ConvertTimeFormat, ms_SetPositions, ms_GetPositions, ms_GetAvailable, ms_SetRate, ms_GetRate, ms_GetPreroll };

/* the pin */
static HRESULT WINAPI pin_qi(IPin *p, REFIID iid, void **out) {
    Filter *f = OF(p, pin);
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IPin)) *out = &f->pin;
    else if (f->out && IsEqualIID(iid, &IID_IAsyncReader)) *out = &f->ar;
    else if (!f->out && IsEqualIID(iid, &IID_IMemInputPin)) *out = &f->mi;
    else { *out = 0; return E_NOINTERFACE; }
    add(f);
    return S_OK;
}
static ULONG WINAPI pin_add(IPin *p) { return add(OF(p, pin)); }
static ULONG WINAPI pin_rel(IPin *p) { return rel(OF(p, pin)); }
/* the source's Connect: MEDIATYPE_Stream / MPEG1System, as krmovie's reader offers for .mpg */
static HRESULT WINAPI pin_connect(IPin *p, IPin *to, const AM_MEDIA_TYPE *want) {
    Filter *f = OF(p, pin);
    PIN_DIRECTION d;
    if (!f->out) return E_UNEXPECTED;
    if (f->peer) return VFW_E_ALREADY_CONNECTED;
    if (FAILED(to->lpVtbl->QueryDirection(to, &d)) || d != PINDIR_INPUT) return VFW_E_INVALID_DIRECTION;
    memset(&f->mt, 0, sizeof f->mt);
    f->mt.majortype = MEDIATYPE_Stream;
    f->mt.subtype = MEDIASUBTYPE_MPEG1System;
    f->mt.bFixedSizeSamples = TRUE;
    f->mt.lSampleSize = 1;
    f->mt.formattype = GUID_NULL;
    HRESULT hr = to->lpVtbl->ReceiveConnection(to, p, &f->mt);
    if (FAILED(hr)) return hr;
    f->peer = to;
    to->lpVtbl->AddRef(to);
    return S_OK;
}
/* the renderer's: RGB32 VIDEOINFOHEADER only */
static HRESULT WINAPI pin_receive(IPin *p, IPin *from, const AM_MEDIA_TYPE *mt) {
    Filter *f = OF(p, pin);
    if (f->out) return E_UNEXPECTED;
    if (f->peer) return VFW_E_ALREADY_CONNECTED;
    if (!IsEqualGUID(&mt->majortype, &MEDIATYPE_Video) || !IsEqualGUID(&mt->subtype, &MEDIASUBTYPE_RGB32) || !IsEqualGUID(&mt->formattype, &FORMAT_VideoInfo) || mt->cbFormat < sizeof(VIDEOINFOHEADER))
        return VFW_E_TYPE_NOT_ACCEPTED;
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
    i->dir = f->out ? PINDIR_OUTPUT : PINDIR_INPUT;
    wcopy(i->achName, f->pin_name);
    return S_OK;
}
static HRESULT WINAPI pin_dir(IPin *p, PIN_DIRECTION *d) { *d = OF(p, pin)->out ? PINDIR_OUTPUT : PINDIR_INPUT; return S_OK; }
static HRESULT WINAPI pin_id(IPin *p, LPWSTR *id) {
    Filter *f = OF(p, pin);
    *id = CoTaskMemAlloc(2 * (wcslen(f->pin_name) + 1));
    wcopy(*id, f->pin_name);
    return S_OK;
}
static HRESULT WINAPI pin_accept(IPin *p, const AM_MEDIA_TYPE *mt) { return S_FALSE; }
static HRESULT WINAPI pin_types(IPin *p, IEnumMediaTypes **e) { *e = 0; return E_NOTIMPL; }
static HRESULT WINAPI pin_internal(IPin *p, IPin **a, ULONG *n) { return E_NOTIMPL; }
static HRESULT WINAPI pin_eos(IPin *p) {
    Filter *f = OF(p, pin);
    f->eos++;
    if (f->sink) f->sink->lpVtbl->Notify(f->sink, EC_COMPLETE, S_OK, (LONG_PTR)&f->bf);
    return S_OK;
}
static HRESULT WINAPI pin_begin_flush(IPin *p) { Filter *f = OF(p, pin); f->flushes++; f->flushing = 1; return S_OK; }
static HRESULT WINAPI pin_end_flush(IPin *p) { Filter *f = OF(p, pin); f->flushing = 0; f->eos = 0; return S_OK; }
static HRESULT WINAPI pin_segment(IPin *p, REFERENCE_TIME a, REFERENCE_TIME b, double r) { OF(p, pin)->segments++; return S_OK; }
static IPinVtbl pin_vt = { pin_qi, pin_add, pin_rel, pin_connect, pin_receive, pin_disconnect, pin_connected, pin_mt, pin_info, pin_dir, pin_id, pin_accept, pin_types, pin_internal, pin_eos, pin_begin_flush, pin_end_flush, pin_segment };

/* the source's IAsyncReader over the stream (times of samples are byte offsets * 10^7) */
static HRESULT WINAPI ar_qi(IAsyncReader *a, REFIID iid, void **out) { return pin_qi(&OF(a, ar)->pin, iid, out); }
static ULONG WINAPI ar_add(IAsyncReader *a) { return add(OF(a, ar)); }
static ULONG WINAPI ar_rel(IAsyncReader *a) { return rel(OF(a, ar)); }
static HRESULT WINAPI ar_allocator(IAsyncReader *a, IMemAllocator *pref, ALLOCATOR_PROPERTIES *props, IMemAllocator **out) {
    ALLOCATOR_PROPERTIES got;
    IMemAllocator *m = pref;
    if (m) m->lpVtbl->AddRef(m);
    else if (FAILED(CoCreateInstance(&CLSID_MemoryAllocator, 0, CLSCTX_INPROC_SERVER, &IID_IMemAllocator, (void **)&m))) return E_FAIL;
    if (props->cbAlign == 0) props->cbAlign = 1;
    HRESULT hr = m->lpVtbl->SetProperties(m, props, &got);
    if (FAILED(hr)) { m->lpVtbl->Release(m); return hr; }
    *out = m;
    return S_OK;
}
static HRESULT fill(IMediaSample *s) {
    REFERENCE_TIME t0, t1;
    BYTE *p;
    s->lpVtbl->GetTime(s, &t0, &t1);
    LONGLONG at = t0 / 10000000, end = t1 / 10000000;
    if (end > data_len) end = data_len;
    long n = at < end ? (long)(end - at) : 0;
    s->lpVtbl->GetPointer(s, &p);
    memcpy(p, data + at, n);
    s->lpVtbl->SetActualDataLength(s, n);
    return n == (t1 - t0) / 10000000 ? S_OK : S_FALSE;
}
static HRESULT WINAPI ar_request(IAsyncReader *a, IMediaSample *s, DWORD_PTR user) {
    Filter *f = OF(a, ar);
    if (f->flushing) return VFW_E_WRONG_STATE;
    if (f->nreq == 64) return E_FAIL;
    f->req[f->nreq] = s;
    f->user[f->nreq++] = user;
    return S_OK;
}
static HRESULT WINAPI ar_wait(IAsyncReader *a, DWORD ms, IMediaSample **s, DWORD_PTR *user) {
    Filter *f = OF(a, ar);
    *s = 0;
    *user = 0;
    if (!f->nreq) { if (!f->flushing) Sleep(ms > 10 ? 10 : ms); return f->flushing ? VFW_E_WRONG_STATE : VFW_E_TIMEOUT; }
    *s = f->req[0];
    *user = f->user[0];
    memmove(f->req, f->req + 1, --f->nreq * sizeof f->req[0]);
    memmove(f->user, f->user + 1, f->nreq * sizeof f->user[0]);
    if (f->flushing) return VFW_E_WRONG_STATE;
    return fill(*s);
}
static HRESULT WINAPI ar_sync_aligned(IAsyncReader *a, IMediaSample *s) { return fill(s); }
static HRESULT WINAPI ar_sync(IAsyncReader *a, LONGLONG at, LONG n, BYTE *buf) {
    if (at >= data_len) return S_FALSE;
    LONG k = at + n > data_len ? (LONG)(data_len - at) : n;
    memcpy(buf, data + at, k);
    return k == n ? S_OK : S_FALSE;
}
static HRESULT WINAPI ar_length(IAsyncReader *a, LONGLONG *total, LONGLONG *avail) { *total = *avail = data_len; return S_OK; }
static HRESULT WINAPI ar_begin_flush(IAsyncReader *a) { OF(a, ar)->flushing = 1; return S_OK; }
static HRESULT WINAPI ar_end_flush(IAsyncReader *a) { OF(a, ar)->flushing = 0; return S_OK; }
static IAsyncReaderVtbl ar_vt = { ar_qi, ar_add, ar_rel, ar_allocator, ar_request, ar_wait, ar_sync_aligned, ar_sync, ar_length, ar_begin_flush, ar_end_flush };

/* the renderer's IMemInputPin: takes each sample at once (no clock), keeps their times */
static HRESULT WINAPI mi_qi(IMemInputPin *m, REFIID iid, void **out) { return pin_qi(&OF(m, mi)->pin, iid, out); }
static ULONG WINAPI mi_add(IMemInputPin *m) { return add(OF(m, mi)); }
static ULONG WINAPI mi_rel(IMemInputPin *m) { return rel(OF(m, mi)); }
static HRESULT WINAPI mi_get_alloc(IMemInputPin *m, IMemAllocator **a) { *a = 0; return VFW_E_NO_ALLOCATOR; }
static HRESULT WINAPI mi_notify_alloc(IMemInputPin *m, IMemAllocator *a, BOOL ro) { return S_OK; }
static HRESULT WINAPI mi_reqs(IMemInputPin *m, ALLOCATOR_PROPERTIES *p) { return E_NOTIMPL; }
static HRESULT WINAPI mi_receive(IMemInputPin *m, IMediaSample *s) {
    Filter *f = OF(m, mi);
    int k = f->samples++;
    if (k < 40) {
        if (FAILED(s->lpVtbl->GetTime(s, &f->t0[k], &f->t1[k]))) f->t0[k] = f->t1[k] = -1;
        f->lens[k] = s->lpVtbl->GetActualDataLength(s);
    }
    if (k == 10) {
        BYTE *p;
        s->lpVtbl->GetPointer(s, &p);
        if (s->lpVtbl->GetActualDataLength(s) >= W * H * 4) memcpy(f->pic, p, W * H * 4);
    }
    return S_OK;
}
static HRESULT WINAPI mi_receive_multiple(IMemInputPin *m, IMediaSample **s, long n, long *done) {
    for (long i = 0; i < n; i++) mi_receive(m, s[i]);
    *done = n;
    return S_OK;
}
static HRESULT WINAPI mi_can_block(IMemInputPin *m) { return S_FALSE; }
static IMemInputPinVtbl mi_vt = { mi_qi, mi_add, mi_rel, mi_get_alloc, mi_notify_alloc, mi_reqs, mi_receive, mi_receive_multiple, mi_can_block };

static Filter *filter(int out, const WCHAR *name, const WCHAR *pin) {
    Filter *f = calloc(1, sizeof *f);
    f->bf.lpVtbl = &bf_vt; f->ms.lpVtbl = &ms_vt; f->pin.lpVtbl = &pin_vt; f->ar.lpVtbl = &ar_vt; f->mi.lpVtbl = &mi_vt;
    f->refs = 1; f->out = out; f->name = name; f->pin_name = pin;
    return f;
}

/* ------------------------------------------------------------------------------------------ */

static IGraphBuilder *gb;

/* krmovie's ConnectFilters: the first free output of `a` to the first free input of `b` that
 * IGraphBuilder::Connect takes */
static HRESULT connect_filters(IBaseFilter *a, IBaseFilter *b) {
    IEnumPins *ea, *eb;
    IPin *pa, *pb, *x;
    PIN_INFO ia, ib;
    HRESULT last = E_FAIL;
    a->lpVtbl->EnumPins(a, &ea);
    while (ea->lpVtbl->Next(ea, 1, &pa, 0) == S_OK) {
        pa->lpVtbl->QueryPinInfo(pa, &ia);
        ia.pFilter->lpVtbl->Release(ia.pFilter);
        x = 0;
        pa->lpVtbl->ConnectedTo(pa, &x);
        if (x) x->lpVtbl->Release(x);
        if (ia.dir == PINDIR_OUTPUT && !x) {
            b->lpVtbl->EnumPins(b, &eb);
            while (eb->lpVtbl->Next(eb, 1, &pb, 0) == S_OK) {
                pb->lpVtbl->QueryPinInfo(pb, &ib);
                ib.pFilter->lpVtbl->Release(ib.pFilter);
                x = 0;
                pb->lpVtbl->ConnectedTo(pb, &x);
                if (x) x->lpVtbl->Release(x);
                if (ib.dir == PINDIR_INPUT && !x) {
                    last = gb->lpVtbl->Connect(gb, pa, pb);
                    if (SUCCEEDED(last)) { pb->lpVtbl->Release(pb); eb->lpVtbl->Release(eb); pa->lpVtbl->Release(pa); ea->lpVtbl->Release(ea); return last; }
                }
                pb->lpVtbl->Release(pb);
            }
            eb->lpVtbl->Release(eb);
        }
        pa->lpVtbl->Release(pa);
    }
    ea->lpVtbl->Release(ea);
    return last;
}

static IBaseFilter *create(const CLSID *c, const WCHAR *name, const char *label) {
    IBaseFilter *f = 0;
    HRESULT hr = CoCreateInstance(c, 0, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&f);
    printf("%s: create %08lx\n", label, hr);
    if (f) printf("%s: add %08lx\n", label, gb->lpVtbl->AddFilter(gb, f, name));
    return f;
}

static void pins(const char *label, IBaseFilter *f) {
    IEnumPins *e;
    IPin *p;
    PIN_INFO i;
    if (FAILED(f->lpVtbl->EnumPins(f, &e))) { printf("%s: no pins\n", label); return; }
    while (e->lpVtbl->Next(e, 1, &p, 0) == S_OK) {
        p->lpVtbl->QueryPinInfo(p, &i);
        i.pFilter->lpVtbl->Release(i.pFilter);
        printf("%s: pin %s %s\n", label, i.dir == PINDIR_INPUT ? "in" : "out", i.dir == PINDIR_INPUT ? "-" : "-");
        p->lpVtbl->Release(p);
    }
    e->lpVtbl->Release(e);
}

static void play(const char *label, Filter *r, IMediaControl *mc, IMediaEvent *me) {
    long code = 0;
    r->samples = 0;
    HRESULT hr = mc->lpVtbl->Run(mc);
    printf("%s: run %s\n", label, SUCCEEDED(hr) ? "ok" : "failed");
    hr = me->lpVtbl->WaitForCompletion(me, 10000, &code);
    printf("%s: complete %08lx code %ld\n", label, hr, code);
    int n = r->samples;
    printf("%s: samples %d\n", label, n);
    int ordered = 1, sized = 1;
    for (int k = 0; k < n && k < 40; k++) {
        if (r->lens[k] != W * H * 4) sized = 0;
        if (k && r->t0[k] != r->t1[k - 1]) ordered = 0;
    }
    printf("%s: lengths %s, each starting at the last's end %s\n", label, sized ? "ok" : "differ", ordered ? "yes" : "no");
    for (int k = 0; k < n && k < 3; k++) printf("%s: sample %d %lld..%lld\n", label, k, r->t0[k] / 10000, r->t1[k] / 10000);
    if (n > 10) probe_image(label, r->pic + (H - 1) * W * 4, W, H, -W * 4, 24);
    mc->lpVtbl->Stop(mc);
}

int main(void) {
    HRSRC rs = FindResourceA(0, MAKEINTRESOURCEA(10), (LPCSTR)RT_RCDATA);
    data = LockResource(LoadResource(0, rs));
    data_len = SizeofResource(0, rs);
    CoInitialize(0);
    if (FAILED(CoCreateInstance(&CLSID_FilterGraph, 0, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb))) { puts("skip no filter graph"); return 0; }
    Filter *src = filter(1, L"Stream Reader", L"Output"), *rend = filter(0, L"Buffer Renderer", L"Input");
    printf("source: add %08lx\n", gb->lpVtbl->AddFilter(gb, &src->bf, src->name));
    printf("renderer: add %08lx\n", gb->lpVtbl->AddFilter(gb, &rend->bf, rend->name));
    IBaseFilter *split = create(&CLSID_MPEG1Splitter, L"MPEG-I Stream Splitter", "splitter");
    if (!split) return 0;
    pins("splitter before", split);
    printf("source -> splitter: %08lx\n", connect_filters(&src->bf, split));
    pins("splitter", split);
    IBaseFilter *vdec = create(&CLSID_CMpegVideoCodec, L"MPEG Video Decoder", "video decoder");
    printf("splitter -> video decoder: %08lx\n", connect_filters(split, vdec));
    printf("video decoder -> renderer: %08lx\n", connect_filters(vdec, &rend->bf));
    if (rend->peer) {
        VIDEOINFOHEADER *v = (VIDEOINFOHEADER *)rend->mt.pbFormat;
        guid_name("renderer: major", &rend->mt.majortype);
        guid_name("renderer: sub", &rend->mt.subtype);
        guid_name("renderer: format", &rend->mt.formattype);
        printf("renderer: %ldx%ld %d bits, compression %lu, image %lu, sample size %lu fixed %d, frame %lld\n", v->bmiHeader.biWidth, v->bmiHeader.biHeight, v->bmiHeader.biBitCount, v->bmiHeader.biCompression, v->bmiHeader.biSizeImage, rend->mt.lSampleSize, rend->mt.bFixedSizeSamples, v->AvgTimePerFrame);
    }
    IBaseFilter *adec = create(&CLSID_CMpegAudioCodec, L"MPEG Audio Decoder", "audio decoder");
    printf("splitter -> audio decoder: %08lx\n", connect_filters(split, adec));
    printf("splitter -> video decoder again: %08lx\n", connect_filters(split, vdec) == S_OK ? 0 : 1);
    /* (the sound renderer: the runner may have no sound device, so its answers stay unprinted) */
    IBaseFilter *snd = 0;
    if (SUCCEEDED(CoCreateInstance(&CLSID_DSoundRender, 0, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&snd))) {
        gb->lpVtbl->AddFilter(gb, snd, L"Sound Renderer");
        if (FAILED(connect_filters(adec, snd))) gb->lpVtbl->RemoveFilter(gb, snd);
    }
    IMediaControl *mc;
    IMediaEvent *me;
    IMediaSeeking *ms;
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaControl, (void **)&mc);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaEvent, (void **)&me);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaSeeking, (void **)&ms);
    LONGLONG d = 0;
    HRESULT hr = ms->lpVtbl->GetDuration(ms, &d);
    printf("duration %08lx %.1f s\n", hr, d / 1e7);
    play("first", rend, mc, me);
    LONGLONG zero = 0;
    printf("rewind %08lx\n", ms->lpVtbl->SetPositions(ms, &zero, AM_SEEKING_AbsolutePositioning, 0, AM_SEEKING_NoPositioning));
    play("again", rend, mc, me);
    ms->lpVtbl->Release(ms);
    me->lpVtbl->Release(me);
    mc->lpVtbl->Release(mc);
    gb->lpVtbl->Release(gb);
    puts("done");
    return 0;
}
