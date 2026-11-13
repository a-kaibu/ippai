/* The WebBrowser control (CLSID_WebBrowser) embedded the way Delphi's TWebBrowser (TOleControl)
 * does it: CoCreateInstance for IOleObject, GetMiscStatus, SetClientSite, IPersistStreamInit's
 * InitNew, IOleControl, IDispatch, the events' connection point, DoVerb(INPLACEACTIVATE) into
 * a parent window, IOleInPlaceObject's window, then IWebBrowser2. Kurovadis 体験版 quit on
 * "OLE error 80040154": sake had no such class (#216). Only what holds before a page loads is
 * printed (navigation is asynchronous on Windows).
 * libs: -lole32 -loleaut32 -luuid */
#define COBJMACROS
#include "../probe.h"
#include <ole2.h>
#include <olectl.h>
#include <exdisp.h>

static int calls[16];
enum { CAN_ACTIVATE, ON_ACTIVATE, ON_DEACTIVATE, GET_WINDOW, GET_CONTEXT };
static HWND parent;

/* ---- the container's site: IOleClientSite and IOleInPlaceSite on one object ---- */
typedef struct { IOleClientSite cs; IOleInPlaceSite ips; LONG refs; } Site;
static Site site;

static HRESULT site_qi(void *iid_, void **out) {
    REFIID iid = iid_;
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IOleClientSite)) *out = &site.cs;
    else if (IsEqualIID(iid, &IID_IOleInPlaceSite) || IsEqualIID(iid, &IID_IOleWindow)) *out = &site.ips;
    else { *out = NULL; return E_NOINTERFACE; }
    site.refs++;
    return S_OK;
}
static HRESULT STDMETHODCALLTYPE cs_qi(IOleClientSite *t, REFIID iid, void **out) { return site_qi((void *)iid, out); }
static ULONG STDMETHODCALLTYPE cs_addref(IOleClientSite *t) { return ++site.refs; }
static ULONG STDMETHODCALLTYPE cs_release(IOleClientSite *t) { return --site.refs; }
static HRESULT STDMETHODCALLTYPE cs_save(IOleClientSite *t) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE cs_moniker(IOleClientSite *t, DWORD a, DWORD w, IMoniker **m) { *m = NULL; return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE cs_container(IOleClientSite *t, IOleContainer **c) { *c = NULL; return E_NOINTERFACE; }
static HRESULT STDMETHODCALLTYPE cs_show(IOleClientSite *t) { return S_OK; }
static HRESULT STDMETHODCALLTYPE cs_onshow(IOleClientSite *t, BOOL s) { return S_OK; }
static HRESULT STDMETHODCALLTYPE cs_layout(IOleClientSite *t) { return E_NOTIMPL; }
static IOleClientSiteVtbl cs_vt = { cs_qi, cs_addref, cs_release, cs_save, cs_moniker, cs_container, cs_show, cs_onshow, cs_layout };

static HRESULT STDMETHODCALLTYPE ips_qi(IOleInPlaceSite *t, REFIID iid, void **out) { return site_qi((void *)iid, out); }
static ULONG STDMETHODCALLTYPE ips_addref(IOleInPlaceSite *t) { return ++site.refs; }
static ULONG STDMETHODCALLTYPE ips_release(IOleInPlaceSite *t) { return --site.refs; }
static HRESULT STDMETHODCALLTYPE ips_getwindow(IOleInPlaceSite *t, HWND *h) { calls[GET_WINDOW]++; *h = parent; return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_help(IOleInPlaceSite *t, BOOL b) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE ips_can(IOleInPlaceSite *t) { calls[CAN_ACTIVATE]++; return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_on(IOleInPlaceSite *t) { calls[ON_ACTIVATE]++; return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_onui(IOleInPlaceSite *t) { return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_context(IOleInPlaceSite *t, IOleInPlaceFrame **f, IOleInPlaceUIWindow **d, LPRECT pos, LPRECT clip, LPOLEINPLACEFRAMEINFO fi) {
    calls[GET_CONTEXT]++;
    *f = NULL;
    *d = NULL;
    SetRect(pos, 10, 20, 330, 260);
    SetRect(clip, 10, 20, 330, 260);
    fi->fMDIApp = FALSE;
    fi->hwndFrame = parent;
    fi->haccel = NULL;
    fi->cAccelEntries = 0;
    return S_OK;
}
static HRESULT STDMETHODCALLTYPE ips_scroll(IOleInPlaceSite *t, SIZE s) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE ips_onuide(IOleInPlaceSite *t, BOOL u) { return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_onde(IOleInPlaceSite *t) { calls[ON_DEACTIVATE]++; return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_discard(IOleInPlaceSite *t) { return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_undo(IOleInPlaceSite *t) { return S_OK; }
static HRESULT STDMETHODCALLTYPE ips_posrect(IOleInPlaceSite *t, LPCRECT r) { return S_OK; }
static IOleInPlaceSiteVtbl ips_vt = { ips_qi, ips_addref, ips_release, ips_getwindow, ips_help, ips_can, ips_on, ips_onui, ips_context, ips_scroll, ips_onuide, ips_onde, ips_discard, ips_undo, ips_posrect };

/* ---- an events sink (IDispatch) ---- */
static LONG sink_refs;
static HRESULT STDMETHODCALLTYPE sk_qi(IDispatch *t, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IDispatch) || IsEqualIID(iid, &DIID_DWebBrowserEvents2)) { *out = t; sink_refs++; return S_OK; }
    *out = NULL;
    return E_NOINTERFACE;
}
static ULONG STDMETHODCALLTYPE sk_addref(IDispatch *t) { return ++sink_refs; }
static ULONG STDMETHODCALLTYPE sk_release(IDispatch *t) { return --sink_refs; }
static HRESULT STDMETHODCALLTYPE sk_count(IDispatch *t, UINT *n) { *n = 0; return S_OK; }
static HRESULT STDMETHODCALLTYPE sk_info(IDispatch *t, UINT i, LCID l, ITypeInfo **ti) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE sk_ids(IDispatch *t, REFIID r, LPOLESTR *n, UINT c, LCID l, DISPID *id) { return E_NOTIMPL; }
static HRESULT STDMETHODCALLTYPE sk_invoke(IDispatch *t, DISPID id, REFIID r, LCID l, WORD f, DISPPARAMS *p, VARIANT *v, EXCEPINFO *e, UINT *a) { return S_OK; }
static IDispatchVtbl sk_vt = { sk_qi, sk_addref, sk_release, sk_count, sk_info, sk_ids, sk_invoke };
static IDispatch sink = { &sk_vt };

static const char *has(IUnknown *u, REFIID iid) {
    IUnknown *p = NULL;
    HRESULT hr = IUnknown_QueryInterface(u, iid, (void **)&p);
    if (p) IUnknown_Release(p);
    return hr == S_OK ? "yes" : "no";
}

int main(void) {
    OleInitialize(NULL);
    site.cs.lpVtbl = &cs_vt;
    site.ips.lpVtbl = &ips_vt;
    parent = CreateWindowExA(0, "STATIC", "host", WS_POPUP | WS_VISIBLE, 0, 0, 400, 300, NULL, NULL, NULL, NULL);

    IOleObject *ole = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_WebBrowser, NULL, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER, &IID_IOleObject, (void **)&ole);
    printf("CoCreateInstance(WebBrowser, IOleObject): %08lx %d\n", hr, ole != NULL);
    if (!ole) return 0;
    IUnknown *u = (IUnknown *)ole;
    printf("interfaces: IWebBrowser2 %s IWebBrowser %s IDispatch %s IOleControl %s IPersistStreamInit %s IOleInPlaceObject %s IOleInPlaceActiveObject %s IConnectionPointContainer %s IViewObject2 %s IOleWindow %s IPersist %s\n",
           has(u, &IID_IWebBrowser2), has(u, &IID_IWebBrowser), has(u, &IID_IDispatch), has(u, &IID_IOleControl), has(u, &IID_IPersistStreamInit), has(u, &IID_IOleInPlaceObject),
           has(u, &IID_IOleInPlaceActiveObject), has(u, &IID_IConnectionPointContainer), has(u, &IID_IViewObject2), has(u, &IID_IOleWindow), has(u, &IID_IPersist));
    DWORD misc = 0;
    hr = IOleObject_GetMiscStatus(ole, DVASPECT_CONTENT, &misc);
    printf("GetMiscStatus: %08lx %08lx\n", hr, misc);
    CLSID cls;
    hr = IOleObject_GetUserClassID(ole, &cls);
    printf("GetUserClassID: %08lx is CLSID_WebBrowser %d\n", hr, IsEqualCLSID(&cls, &CLSID_WebBrowser));
    hr = IOleObject_SetClientSite(ole, &site.cs);
    printf("SetClientSite: %08lx\n", hr);
    IPersistStreamInit *psi = NULL;
    IOleObject_QueryInterface(ole, &IID_IPersistStreamInit, (void **)&psi);
    if (psi) {
        printf("InitNew: %08lx\n", IPersistStreamInit_InitNew(psi));
        hr = IPersistStreamInit_GetClassID(psi, &cls);
        printf("IPersistStreamInit GetClassID: %08lx is CLSID_WebBrowser %d\n", hr, IsEqualCLSID(&cls, &CLSID_WebBrowser));
        IPersistStreamInit_Release(psi);
    }
    IOleControl *ctl = NULL;
    IOleObject_QueryInterface(ole, &IID_IOleControl, (void **)&ctl);
    if (ctl) {
        CONTROLINFO ci = { sizeof ci };
        hr = IOleControl_GetControlInfo(ctl, &ci);
        printf("GetControlInfo: %08lx accels %d\n", hr, ci.cAccel);
        printf("OnAmbientPropertyChange: %08lx\n", IOleControl_OnAmbientPropertyChange(ctl, DISPID_UNKNOWN));
        IOleControl_Release(ctl);
    }
    IConnectionPointContainer *cpc = NULL;
    IOleObject_QueryInterface(ole, &IID_IConnectionPointContainer, (void **)&cpc);
    IConnectionPoint *cp = NULL;
    DWORD cookie = 0;
    if (cpc) {
        hr = IConnectionPointContainer_FindConnectionPoint(cpc, &DIID_DWebBrowserEvents2, &cp);
        printf("FindConnectionPoint(DWebBrowserEvents2): %08lx %d\n", hr, cp != NULL);
        IConnectionPoint *cp2 = NULL;
        hr = IConnectionPointContainer_FindConnectionPoint(cpc, &IID_IPropertyNotifySink, &cp2);
        printf("FindConnectionPoint(IPropertyNotifySink): %08lx %d\n", hr, cp2 != NULL);
        if (cp2) IConnectionPoint_Release(cp2);
        cp2 = NULL;
        hr = IConnectionPointContainer_FindConnectionPoint(cpc, &IID_IOleObject, &cp2);
        printf("FindConnectionPoint(IOleObject): %08lx %d\n", hr, cp2 != NULL);
        if (cp) {
            IID iid;
            hr = IConnectionPoint_GetConnectionInterface(cp, &iid);
            printf("GetConnectionInterface: %08lx is DWebBrowserEvents2 %d\n", hr, IsEqualIID(&iid, &DIID_DWebBrowserEvents2));
            hr = IConnectionPoint_Advise(cp, (IUnknown *)&sink, &cookie);
            printf("Advise: %08lx cookie %d sink held %d\n", hr, cookie != 0, sink_refs > 0);
        }
    }

    RECT r = { 10, 20, 330, 260 };
    hr = IOleObject_DoVerb(ole, OLEIVERB_INPLACEACTIVATE, NULL, &site.cs, 0, parent, &r);
    printf("DoVerb(INPLACEACTIVATE): %08lx CanInPlaceActivate %d OnInPlaceActivate %d\n", hr, calls[CAN_ACTIVATE] > 0, calls[ON_ACTIVATE] > 0);
    IOleInPlaceObject *ipo = NULL;
    IOleObject_QueryInterface(ole, &IID_IOleInPlaceObject, (void **)&ipo);
    HWND w = NULL;
    if (ipo) {
        hr = IOleInPlaceObject_GetWindow(ipo, &w);
        char cn[64] = "";
        GetClassNameA(w, cn, sizeof cn);
        RECT wr;
        GetWindowRect(w, &wr);
        printf("GetWindow: %08lx class '%s' parent is host %d child %d visible %d at %ld,%ld %ldx%ld\n", hr, cn, GetParent(w) == parent,
               (GetWindowLongA(w, GWL_STYLE) & WS_CHILD) != 0, IsWindowVisible(w), wr.left, wr.top, wr.right - wr.left, wr.bottom - wr.top);
        RECT r2 = { 0, 0, 200, 100 };
        hr = IOleInPlaceObject_SetObjectRects(ipo, &r2, &r2);
        GetWindowRect(w, &wr);
        printf("SetObjectRects: %08lx %ldx%ld\n", hr, wr.right - wr.left, wr.bottom - wr.top);
    }

    IWebBrowser2 *wb = NULL;
    IOleObject_QueryInterface(ole, &IID_IWebBrowser2, (void **)&wb);
    if (wb) {
        VARIANT_BOOL b = 1;
        hr = IWebBrowser2_get_Visible(wb, &b);
        printf("get_Visible: %08lx %d\n", hr, b);
        hr = IWebBrowser2_put_Silent(wb, VARIANT_TRUE);
        b = 1;
        HRESULT hr2 = IWebBrowser2_get_Silent(wb, &b);
        printf("put_Silent: %08lx get_Silent: %08lx %d\n", hr, hr2, b);
        hr = IWebBrowser2_get_RegisterAsDropTarget(wb, &b);
        printf("get_RegisterAsDropTarget: %08lx %d\n", hr, b);
        LONG v = -1;
        hr = IWebBrowser2_get_Width(wb, &v);
        printf("get_Width: %08lx %ld\n", hr, v);
        IDispatch *app = NULL;
        hr = IWebBrowser2_get_Application(wb, &app);
        printf("get_Application: %08lx %d\n", hr, app != NULL);
        if (app) IDispatch_Release(app);
        UINT n = 9;
        hr = IWebBrowser2_GetTypeInfoCount(wb, &n);
        printf("GetTypeInfoCount: %08lx\n", hr);
        IWebBrowser2_Release(wb);
    }

    if (cp) {
        hr = IConnectionPoint_Unadvise(cp, cookie);
        printf("Unadvise: %08lx again %08lx\n", hr, IConnectionPoint_Unadvise(cp, cookie));
        IConnectionPoint_Release(cp);
    }
    if (cpc) IConnectionPointContainer_Release(cpc);
    if (ipo) {
        hr = IOleInPlaceObject_InPlaceDeactivate(ipo);
        printf("InPlaceDeactivate: %08lx OnInPlaceDeactivate %d window gone %d\n", hr, calls[ON_DEACTIVATE] > 0, !IsWindow(w));
        IOleInPlaceObject_Release(ipo);
    }
    hr = IOleObject_Close(ole, OLECLOSE_NOSAVE);
    printf("Close: %08lx\n", hr);
    IOleObject_SetClientSite(ole, NULL);
    IOleObject_Release(ole);
    u = NULL;
    printf("CLSID_WebBrowser_V1: %08lx\n", CoCreateInstance(&CLSID_WebBrowser_V1, NULL, CLSCTX_INPROC_SERVER, &IID_IUnknown, (void **)&u));
    if (u) IUnknown_Release(u);
    OleUninitialize();
    return 0;
}
