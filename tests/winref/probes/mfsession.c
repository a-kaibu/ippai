/* The Media Session as games play a movie with it (#31): topologies and their nodes
 * (MFCreateTopology, MFCreateTopologyNode, connections, collections, clones), collections and
 * events (MFCreateCollection, MFCreateMediaEvent), then a session on mfvideo's movie from the
 * source resolver to the EVR's activate: SetTopology, the events to its end (GetEvent and
 * BeginGetEvent), the video display control through MFGetService, the clock, the volume, Pause,
 * Stop, Close, Shutdown and the calls after it, and calls made in the wrong state.
 * run: browser (sake decodes H.264 only with a page)
 * libs: -lmf -lmfplat -lmfuuid -lole32 -luuid -lstrmiids -lgdi32 -luser32 */
#define COBJMACROS
#include "../probe.h"
#include <initguid.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mferror.h>
#include <evr.h>
#include "mfvideo_data.h"

static const char *name_of(MediaEventType t) {
    switch (t) {
    case MEError: return "Error";
    case MESessionTopologySet: return "TopologySet";
    case MESessionTopologiesCleared: return "TopologiesCleared";
    case MESessionStarted: return "Started";
    case MESessionPaused: return "Paused";
    case MESessionStopped: return "Stopped";
    case MESessionClosed: return "Closed";
    case MESessionEnded: return "Ended";
    case MESessionCapabilitiesChanged: return "CapabilitiesChanged";
    case MESessionTopologyStatus: return "TopologyStatus";
    case MEEndOfPresentation: return "EndOfPresentation";
    default: return NULL;
    }
}

/* one line of an event: its type, status, the attributes sessions give, the value's type */
static void show(IMFMediaEvent *e) {
    MediaEventType t = 0;
    HRESULT st = 0;
    IMFMediaEvent_GetType(e, &t);
    IMFMediaEvent_GetStatus(e, &st);
    const char *n = name_of(t);
    if (n) printf("  event %s %#lx", n, st);
    else printf("  event %lu %#lx", t, st);
    UINT32 v = 0;
    if (SUCCEEDED(IMFMediaEvent_GetUINT32(e, &MF_EVENT_TOPOLOGY_STATUS, &v))) printf(" status %u", v);
    if (SUCCEEDED(IMFMediaEvent_GetUINT32(e, &MF_EVENT_SESSIONCAPS, &v))) printf(" caps %#x", v);
    if (SUCCEEDED(IMFMediaEvent_GetUINT32(e, &MF_EVENT_SESSIONCAPS_DELTA, &v))) printf(" delta %#x", v);
    PROPVARIANT pv;
    PropVariantInit(&pv);
    if (SUCCEEDED(IMFMediaEvent_GetValue(e, &pv))) printf(" vt %u", pv.vt);
    if (pv.vt == VT_UNKNOWN && pv.punkVal) {
        IMFTopology *t2 = NULL;
        printf(" topology %d", SUCCEEDED(IUnknown_QueryInterface(pv.punkVal, &IID_IMFTopology, (void **)&t2)));
        if (t2) IMFTopology_Release(t2);
    }
    PropVariantClear(&pv);
    printf("\n");
}

/* Events (GetEvent, no wait) until one of type `until` (or `ms` pass): each shown. */
static int wait_for(IMFMediaSession *s, MediaEventType until, DWORD ms) {
    DWORD t0 = GetTickCount();
    while (GetTickCount() - t0 < ms) {
        IMFMediaEvent *e = NULL;
        HRESULT hr = IMFMediaSession_GetEvent(s, MF_EVENT_FLAG_NO_WAIT, &e);
        if (hr == MF_E_NO_EVENTS_AVAILABLE) {
            Sleep(5);
            continue;
        }
        if (FAILED(hr)) {
            printf("  GetEvent %#lx\n", hr);
            return 0;
        }
        MediaEventType t = 0;
        IMFMediaEvent_GetType(e, &t);
        show(e);
        IMFMediaEvent_Release(e);
        if (t == until) return 1;
    }
    printf("  no %s in %lu ms\n", name_of(until), ms);
    return 0;
}

static void nodes(void) {
    IMFTopology *t = NULL;
    printf("MFCreateTopology %#lx\n", MFCreateTopology(&t));
    IMFTopologyNode *n[5] = { 0 };
    for (int k = 0; k < 4; k++) {
        HRESULT hr = MFCreateTopologyNode((MF_TOPOLOGY_TYPE)k, &n[k]);
        MF_TOPOLOGY_TYPE ty = 9;
        IMFTopologyNode_GetNodeType(n[k], &ty);
        DWORD ins = 9, outs = 9;
        IMFTopologyNode_GetInputCount(n[k], &ins);
        IMFTopologyNode_GetOutputCount(n[k], &outs);
        TOPOID id = 0;
        IMFTopologyNode_GetTopoNodeID(n[k], &id);
        printf("node %d: %#lx type %d inputs %lu outputs %lu id %d\n", k, hr, ty, ins, outs, id != 0);
    }
    printf("node type 4: %#lx\n", MFCreateTopologyNode((MF_TOPOLOGY_TYPE)4, &n[4]));
    if (n[4]) IMFTopologyNode_Release(n[4]);
    IUnknown *o = (IUnknown *)1;
    printf("GetObject of none: %#lx %d\n", IMFTopologyNode_GetObject(n[0], &o), o == NULL);
    IMFTopologyNode *x = NULL;
    printf("QI topology as node: %#lx\n", IMFTopology_QueryInterface(t, &IID_IMFTopologyNode, (void **)&x));
    if (x) IMFTopologyNode_Release(x);
    /* source -> transform -> tee -> output, then what may not be connected */
    printf("connect source-transform %#lx\n", IMFTopologyNode_ConnectOutput(n[1], 0, n[2], 0));
    printf("connect transform-tee %#lx\n", IMFTopologyNode_ConnectOutput(n[2], 0, n[3], 0));
    printf("connect tee:1-output %#lx\n", IMFTopologyNode_ConnectOutput(n[3], 1, n[0], 0));
    printf("connect output-tee %#lx\n", IMFTopologyNode_ConnectOutput(n[0], 0, n[3], 1));
    printf("connect transform-source %#lx\n", IMFTopologyNode_ConnectOutput(n[2], 1, n[1], 0));
    DWORD c1 = 9, c2 = 9;
    IMFTopologyNode_GetOutputCount(n[3], &c1);
    IMFTopologyNode_GetInputCount(n[0], &c2);
    printf("tee outputs %lu, output inputs %lu\n", c1, c2);
    DWORD idx = 9;
    x = NULL;
    HRESULT hr = IMFTopologyNode_GetOutput(n[3], 0, &x, &idx);
    printf("tee output 0 (none): %#lx\n", hr);
    if (x) IMFTopologyNode_Release(x);
    x = NULL;
    hr = IMFTopologyNode_GetOutput(n[3], 1, &x, &idx);
    printf("tee output 1: %#lx is the output %d at %lu\n", hr, x == n[0], idx);
    if (x) IMFTopologyNode_Release(x);
    x = NULL;
    hr = IMFTopologyNode_GetInput(n[0], 0, &x, &idx);
    printf("output's input: %#lx is the tee %d at %lu\n", hr, x == n[3], idx);
    if (x) IMFTopologyNode_Release(x);
    printf("GetOutput 5: %#lx\n", IMFTopologyNode_GetOutput(n[3], 5, &x, &idx));
    for (int k = 0; k < 4; k++) printf("AddNode %d %#lx\n", k, IMFTopology_AddNode(t, n[k]));
    printf("AddNode again %#lx\n", IMFTopology_AddNode(t, n[0]));
    WORD count = 0;
    IMFTopology_GetNodeCount(t, &count);
    printf("nodes %u\n", count);
    x = NULL;
    printf("GetNode 4: %#lx\n", IMFTopology_GetNode(t, 4, &x));
    TOPOID id = 0;
    IMFTopologyNode_GetTopoNodeID(n[2], &id);
    x = NULL;
    hr = IMFTopology_GetNodeByID(t, id, &x);
    printf("by id: %#lx %d\n", hr, x == n[2]);
    if (x) IMFTopologyNode_Release(x);
    printf("by id 12345: %#lx\n", IMFTopology_GetNodeByID(t, 12345, &x));
    IMFCollection *col = NULL;
    DWORD k = 9;
    IMFTopology_GetSourceNodeCollection(t, &col);
    IMFCollection_GetElementCount(col, &k);
    printf("source nodes %lu\n", k);
    IMFCollection_Release(col);
    IMFTopology_GetOutputNodeCollection(t, &col);
    IMFCollection_GetElementCount(col, &k);
    IUnknown *u = NULL;
    IMFCollection_GetElement(col, 0, &u);
    printf("output nodes %lu, the output %d\n", k, u == (IUnknown *)n[0]);
    if (u) IUnknown_Release(u);
    IMFCollection_Release(col);
    /* a clone: new nodes, the same ids, connected as they were */
    IMFTopology *t2 = NULL;
    MFCreateTopology(&t2);
    IMFTopologyNode_SetUINT32(n[1], &MF_TOPONODE_STREAMID, 7);
    hr = IMFTopology_CloneFrom(t2, t);
    count = 0;
    IMFTopology_GetNodeCount(t2, &count);
    x = NULL;
    IMFTopology_GetNodeByID(t2, id, &x);
    printf("clone %#lx: nodes %u, node by id new %d\n", hr, count, x != NULL && x != n[2]);
    if (x) {
        IMFTopologyNode *d = NULL;
        hr = IMFTopologyNode_GetOutput(x, 0, &d, &idx);
        MF_TOPOLOGY_TYPE ty = 9;
        if (d) IMFTopologyNode_GetNodeType(d, &ty);
        printf("clone's transform output %#lx type %d\n", hr, ty);
        if (d) IMFTopologyNode_Release(d);
        IMFTopologyNode_Release(x);
    }
    IMFTopologyNode_GetTopoNodeID(n[1], &id);
    x = NULL;
    IMFTopology_GetNodeByID(t2, id, &x);
    UINT32 sid = 0;
    if (x) {
        IMFTopologyNode_GetUINT32(x, &MF_TOPONODE_STREAMID, &sid);
        IMFTopologyNode_Release(x);
    }
    printf("clone's source stream id %u\n", sid);
    IMFTopology_Release(t2);
    /* (one call per statement: C evaluates arguments in any order) */
    hr = IMFTopologyNode_DisconnectOutput(n[3], 1);
    printf("disconnect %#lx", hr);
    printf(" again %#lx\n", IMFTopologyNode_DisconnectOutput(n[3], 1));
    hr = IMFTopology_RemoveNode(t, n[3]);
    printf("RemoveNode %#lx", hr);
    printf(" again %#lx\n", IMFTopology_RemoveNode(t, n[3]));
    printf("Clear %#lx\n", IMFTopology_Clear(t));
    IMFTopology_GetNodeCount(t, &count);
    printf("nodes %u\n", count);
    for (int j = 0; j < 4; j++) IMFTopologyNode_Release(n[j]);
    IMFTopology_Release(t);

    /* (mingw's mfplat has no MFCreateCollection to link with) */
    HRESULT (WINAPI *create_collection)(IMFCollection **) = (void *)GetProcAddress(GetModuleHandleW(L"mfplat.dll"), "MFCreateCollection");
    col = NULL;
    printf("MFCreateCollection %#lx\n", create_collection ? create_collection(&col) : E_NOTIMPL);
    if (!col) return;
    IMFCollection_AddElement(col, NULL);
    IMFCollection_GetElementCount(col, &k);
    u = (IUnknown *)1;
    hr = IMFCollection_GetElement(col, 0, &u);
    printf("collection of NULL: %lu, %#lx %d", k, hr, u == NULL);
    printf("; element 3 %#lx\n", IMFCollection_GetElement(col, 3, &u));
    IMFCollection_Release(col);

    IMFMediaEvent *e = NULL;
    PROPVARIANT pv;
    PropVariantInit(&pv);
    pv.vt = VT_UI4;
    pv.ulVal = 5;
    printf("MFCreateMediaEvent %#lx\n", MFCreateMediaEvent(MESessionStarted, &GUID_NULL, 0x80004005, &pv, &e));
    show(e);
    IMFMediaEvent_Release(e);
}

/* an IMFAsyncCallback for BeginGetEvent: each Invoke takes the event and asks for the next */
typedef struct {
    IMFAsyncCallback iface;
    IMFMediaSession *s;
    HANDLE ended;
    DWORD main_thread;
    int other_thread;
    char types[256];
} Cb;
static HRESULT STDMETHODCALLTYPE cb_qi(IMFAsyncCallback *t, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IMFAsyncCallback)) {
        *out = t;
        return S_OK;
    }
    *out = NULL;
    return E_NOINTERFACE;
}
static ULONG STDMETHODCALLTYPE cb_ref(IMFAsyncCallback *t) { return 2; }
static HRESULT STDMETHODCALLTYPE cb_params(IMFAsyncCallback *t, DWORD *f, DWORD *q) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE cb_invoke(IMFAsyncCallback *t, IMFAsyncResult *r) {
    Cb *cb = (Cb *)t;
    if (GetCurrentThreadId() != cb->main_thread) cb->other_thread = 1;
    IMFMediaEvent *e = NULL;
    HRESULT hr = IMFMediaSession_EndGetEvent(cb->s, r, &e);
    if (FAILED(hr)) {
        SetEvent(cb->ended);
        return S_OK;
    }
    MediaEventType ty = 0;
    IMFMediaEvent_GetType(e, &ty);
    IMFMediaEvent_Release(e);
    const char *n = name_of(ty);
    char buf[32];
    if (!n) {
        sprintf(buf, "%lu", ty);
        n = buf;
    }
    if (strlen(cb->types) + strlen(n) + 2 < sizeof cb->types) {
        strcat(cb->types, " ");
        strcat(cb->types, n);
    }
    if (ty == MESessionClosed) SetEvent(cb->ended);
    else IMFMediaSession_BeginGetEvent(cb->s, t, NULL);
    return S_OK;
}
static IMFAsyncCallbackVtbl cb_vtbl = { cb_qi, cb_ref, cb_ref, cb_params, cb_invoke };

/* A topology of the movie's streams from the source to the EVR's activate for `hwnd` */
static IMFTopology *movie_topology(IMFMediaSource *src, HWND hwnd) {
    IMFPresentationDescriptor *pd = NULL;
    IMFMediaSource_CreatePresentationDescriptor(src, &pd);
    IMFTopology *t = NULL;
    MFCreateTopology(&t);
    DWORD n = 0;
    IMFPresentationDescriptor_GetStreamDescriptorCount(pd, &n);
    for (DWORD i = 0; i < n; i++) {
        BOOL sel = FALSE;
        IMFStreamDescriptor *sd = NULL;
        IMFPresentationDescriptor_GetStreamDescriptorByIndex(pd, i, &sel, &sd);
        IMFMediaTypeHandler *h = NULL;
        GUID major = GUID_NULL;
        IMFStreamDescriptor_GetMediaTypeHandler(sd, &h);
        IMFMediaTypeHandler_GetMajorType(h, &major);
        IMFMediaTypeHandler_Release(h);
        IMFActivate *a = NULL;
        if (IsEqualGUID(&major, &MFMediaType_Video)) MFCreateVideoRendererActivate(hwnd, &a);
        else MFCreateAudioRendererActivate(&a);
        IMFTopologyNode *sn = NULL, *on = NULL;
        MFCreateTopologyNode(MF_TOPOLOGY_SOURCESTREAM_NODE, &sn);
        IMFTopologyNode_SetUnknown(sn, &MF_TOPONODE_SOURCE, (IUnknown *)src);
        IMFTopologyNode_SetUnknown(sn, &MF_TOPONODE_PRESENTATION_DESCRIPTOR, (IUnknown *)pd);
        IMFTopologyNode_SetUnknown(sn, &MF_TOPONODE_STREAM_DESCRIPTOR, (IUnknown *)sd);
        MFCreateTopologyNode(MF_TOPOLOGY_OUTPUT_NODE, &on);
        IMFTopologyNode_SetObject(on, (IUnknown *)a);
        IMFTopology_AddNode(t, sn);
        IMFTopology_AddNode(t, on);
        IMFTopologyNode_ConnectOutput(sn, 0, on, 0);
        printf("stream %lu: video %d selected %d\n", i, IsEqualGUID(&major, &MFMediaType_Video), sel);
        IMFTopologyNode_Release(sn);
        IMFTopologyNode_Release(on);
        IMFActivate_Release(a);
        IMFStreamDescriptor_Release(sd);
    }
    IMFPresentationDescriptor_Release(pd);
    return t;
}

static IMFMediaSource *open_source(const WCHAR *path) {
    IMFSourceResolver *res = NULL;
    MFCreateSourceResolver(&res);
    MF_OBJECT_TYPE ot = MF_OBJECT_INVALID;
    IUnknown *obj = NULL;
    HRESULT hr = IMFSourceResolver_CreateObjectFromURL(res, path, MF_RESOLUTION_MEDIASOURCE, NULL, &ot, &obj);
    printf("source %#lx\n", hr);
    IMFSourceResolver_Release(res);
    IMFMediaSource *src = NULL;
    if (obj) {
        IUnknown_QueryInterface(obj, &IID_IMFMediaSource, (void **)&src);
        IUnknown_Release(obj);
    }
    return src;
}

static void play(const WCHAR *path, HWND hwnd) {
    IMFMediaSession *s = NULL;
    printf("MFCreateMediaSession %#lx\n", MFCreateMediaSession(NULL, &s));
    DWORD caps = 0;
    IMFMediaSession_GetSessionCapabilities(s, &caps);
    printf("caps %#lx\n", caps);
    IMFMediaEvent *e = NULL;
    printf("GetEvent of none %#lx\n", IMFMediaSession_GetEvent(s, MF_EVENT_FLAG_NO_WAIT, &e));
    printf("Pause before a topology %#lx\n", IMFMediaSession_Pause(s));
    wait_for(s, MESessionPaused, 3000);
    IMFMediaSource *src = open_source(path);
    if (!src) return;
    IMFTopology *t = movie_topology(src, hwnd);
    printf("SetTopology %#lx\n", IMFMediaSession_SetTopology(s, 0, t));
    wait_for(s, MESessionTopologyStatus, 5000);
    IMFMediaSession_GetSessionCapabilities(s, &caps);
    printf("caps %#lx\n", caps);
    IMFVideoDisplayControl *dc = NULL;
    HRESULT hr = MFGetService((IUnknown *)s, &MR_VIDEO_RENDER_SERVICE, &IID_IMFVideoDisplayControl, (void **)&dc);
    printf("display control %#lx\n", hr);
    if (dc) {
        HWND w = NULL;
        IMFVideoDisplayControl_GetVideoWindow(dc, &w);
        SIZE sz = { 0 }, ar = { 0 };
        hr = IMFVideoDisplayControl_GetNativeVideoSize(dc, &sz, &ar);
        printf("  window ours %d, native %#lx %ldx%ld\n", w == hwnd, hr, sz.cx, sz.cy);
        RECT r = { 0, 0, 160, 120 };
        printf("  SetVideoPosition %#lx\n", IMFVideoDisplayControl_SetVideoPosition(dc, NULL, &r));
    }
    IMFSimpleAudioVolume *vol = NULL;
    hr = MFGetService((IUnknown *)s, &MR_POLICY_VOLUME_SERVICE, &IID_IMFSimpleAudioVolume, (void **)&vol);
    printf("volume %#lx\n", hr);
    if (vol) {
        float v = 0;
        IMFSimpleAudioVolume_GetMasterVolume(vol, &v);
        printf("  master %d", (int)(v * 100));
        printf("; set 0.5 %#lx", IMFSimpleAudioVolume_SetMasterVolume(vol, 0.5f));
        printf(", 2 %#lx\n", IMFSimpleAudioVolume_SetMasterVolume(vol, 2.0f));
        IMFSimpleAudioVolume_GetMasterVolume(vol, &v);
        printf("  master %d\n", (int)(v * 100 + 0.5));
        IMFSimpleAudioVolume_Release(vol);
    }
    void *junk = NULL;
    printf("unknown service %#lx\n", MFGetService((IUnknown *)s, &MR_VIDEO_MIXER_SERVICE, &IID_IMFTopology, &junk));
    IMFClock *clk = NULL;
    printf("GetClock %#lx\n", IMFMediaSession_GetClock(s, &clk));
    IMFPresentationClock *pc = NULL;
    if (clk) {
        IMFClock_QueryInterface(clk, &IID_IMFPresentationClock, (void **)&pc);
        MFCLOCK_STATE cs = 9;
        IMFClock_GetState(clk, 0, &cs);
        printf("  presentation clock %d, state %d\n", pc != NULL, cs);
    }
    PROPVARIANT pv;
    PropVariantInit(&pv);
    DWORD t0 = GetTickCount();
    printf("Start %#lx\n", IMFMediaSession_Start(s, &GUID_NULL, &pv));
    wait_for(s, MESessionStarted, 5000);
    if (clk) {
        MFCLOCK_STATE cs = 9;
        IMFClock_GetState(clk, 0, &cs);
        printf("  clock state %d\n", cs);
    }
    wait_for(s, MESessionEnded, 10000);
    DWORD took = GetTickCount() - t0;
    printf("played for at least 600 ms: %d, less than 5 s: %d\n", took >= 600, took < 5000);
    if (pc) {
        MFTIME tm = 0;
        IMFPresentationClock_GetTime(pc, &tm);
        printf("  time at the end near 750 ms: %d\n", tm > 6000000 && tm < 8000000);
    }
    /* again from 500 ms, paused, stopped */
    pv.vt = VT_I8;
    pv.hVal.QuadPart = 5000000;
    printf("Start at 500 ms %#lx\n", IMFMediaSession_Start(s, &GUID_NULL, &pv));
    wait_for(s, MESessionStarted, 5000);
    printf("Pause %#lx\n", IMFMediaSession_Pause(s));
    wait_for(s, MESessionPaused, 5000);
    if (pc) {
        MFTIME tm = 0;
        IMFPresentationClock_GetTime(pc, &tm);
        printf("  paused at or past 500 ms: %d\n", tm >= 5000000);
    }
    printf("Pause again %#lx\n", IMFMediaSession_Pause(s));
    wait_for(s, MESessionPaused, 5000);
    printf("Stop %#lx\n", IMFMediaSession_Stop(s));
    wait_for(s, MESessionStopped, 5000);
    if (pc) {
        MFTIME tm = 9;
        IMFPresentationClock_GetTime(pc, &tm);
        printf("  stopped at 0: %d\n", tm == 0);
    }
    printf("Close %#lx\n", IMFMediaSession_Close(s));
    wait_for(s, MESessionClosed, 5000);
    printf("Start after Close %#lx\n", IMFMediaSession_Start(s, &GUID_NULL, &pv));
    wait_for(s, MESessionStarted, 2000);
    printf("Shutdown %#lx\n", IMFMediaSession_Shutdown(s));
    printf("after it: Start %#lx", IMFMediaSession_Start(s, &GUID_NULL, &pv));
    printf(", GetEvent %#lx", IMFMediaSession_GetEvent(s, MF_EVENT_FLAG_NO_WAIT, &e));
    printf(", caps %#lx", IMFMediaSession_GetSessionCapabilities(s, &caps));
    printf(", Shutdown %#lx\n", IMFMediaSession_Shutdown(s));
    if (dc) IMFVideoDisplayControl_Release(dc);
    if (pc) IMFPresentationClock_Release(pc);
    if (clk) IMFClock_Release(clk);
    IMFMediaSource_Shutdown(src);
    IMFMediaSource_Release(src);
    IMFTopology_Release(t);
    printf("released %lu\n", IMFMediaSession_Release(s));
}

/* the same movie with BeginGetEvent: the events on another thread, to Closed */
static void play_async(const WCHAR *path, HWND hwnd) {
    IMFMediaSession *s = NULL;
    MFCreateMediaSession(NULL, &s);
    Cb cb = { { &cb_vtbl }, s, CreateEventW(NULL, TRUE, FALSE, NULL), GetCurrentThreadId(), 0, "" };
    printf("BeginGetEvent %#lx", IMFMediaSession_BeginGetEvent(s, &cb.iface, NULL));
    printf(", again %#lx\n", IMFMediaSession_BeginGetEvent(s, &cb.iface, NULL));
    IMFMediaEvent *e = NULL;
    printf("GetEvent while waiting %#lx\n", IMFMediaSession_GetEvent(s, MF_EVENT_FLAG_NO_WAIT, &e));
    IMFMediaSource *src = open_source(path);
    if (!src) return;
    IMFTopology *t = movie_topology(src, hwnd);
    IMFMediaSession_SetTopology(s, 0, t);
    PROPVARIANT pv;
    PropVariantInit(&pv);
    IMFMediaSession_Start(s, &GUID_NULL, &pv);
    Sleep(2500);
    IMFMediaSession_Close(s);
    DWORD w = WaitForSingleObject(cb.ended, 5000);
    printf("async: closed %d, on another thread %d\n", w == WAIT_OBJECT_0, cb.other_thread);
    printf("async events:%s\n", cb.types);
    IMFMediaSession_Shutdown(s);
    IMFMediaSource_Shutdown(src);
    IMFMediaSource_Release(src);
    IMFTopology_Release(t);
    IMFMediaSession_Release(s);
    CloseHandle(cb.ended);
}

int main(void) {
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    printf("MFStartup %#lx\n", MFStartup(MF_VERSION, MFSTARTUP_FULL));
    nodes();
    WCHAR dir[MAX_PATH], path[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    swprintf(path, MAX_PATH, L"%lsmfsession-probe.mp4", dir);
    HANDLE f = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD wr = 0;
    WriteFile(f, file_mp4, sizeof file_mp4, &wr, NULL);
    CloseHandle(f);
    HWND hwnd = CreateWindowExW(0, L"STATIC", L"mfsession", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 320, 240, NULL, NULL, NULL, NULL);
    play(path, hwnd);
    play_async(path, hwnd);
    DestroyWindow(hwnd);
    DeleteFileW(path);
    MFShutdown();
    return 0;
}
