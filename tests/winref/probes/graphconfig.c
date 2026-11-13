/* DirectShow's filter graph tearing itself down as games do: IGraphConfig (which the graph
 * hands out; 薔薇ノ木ニ薔薇ノ花咲ク took it without checking and crashed on NULL) and its
 * RemoveFilterEx, and IFilterGraph::RemoveFilter: a filter added and removed leaves the
 * enumeration, a second removal fails, and the filter is told it left (its graph pointer).
 * libs: -lole32 -lstrmiids -luuid */
#define COBJMACROS
#include "../probe.h"
#include <dshow.h>
#include <initguid.h>

/* (mingw's strmiids lacks these two) */
DEFINE_GUID(IID_IGraphConfig_, 0x03a1eb8e, 0x32bf, 0x4245, 0x85, 0x02, 0x11, 0x4d, 0x08, 0xa9, 0xcb, 0x88);
DEFINE_GUID(CLSID_EVR_, 0xfa10746c, 0x9b63, 0x4b6c, 0xbc, 0x49, 0xfc, 0x30, 0x0e, 0xa5, 0xf2, 0x56);

/* IGraphConfig's vtable as far as RemoveFilterEx (slot 12) */
typedef struct GraphConfig GraphConfig;
typedef struct {
    HRESULT(STDMETHODCALLTYPE *QueryInterface)(GraphConfig *, REFIID, void **);
    ULONG(STDMETHODCALLTYPE *AddRef)(GraphConfig *);
    ULONG(STDMETHODCALLTYPE *Release)(GraphConfig *);
    void *slots[9];
    HRESULT(STDMETHODCALLTYPE *RemoveFilterEx)(GraphConfig *, IBaseFilter *, DWORD);
} GraphConfigVtbl;
struct GraphConfig { GraphConfigVtbl *lpVtbl; };

static int count(IGraphBuilder *gb, IBaseFilter *want, int *found) {
    IEnumFilters *ef = NULL;
    IBaseFilter *f;
    int n = 0;
    *found = 0;
    if (FAILED(IGraphBuilder_EnumFilters(gb, &ef)) || !ef) return -1;
    while (IEnumFilters_Next(ef, 1, &f, NULL) == S_OK) {
        n++;
        *found |= f == want;
        IBaseFilter_Release(f);
    }
    IEnumFilters_Release(ef);
    return n;
}

static void graph_of(IBaseFilter *f, const char *when) {
    FILTER_INFO fi;
    memset(&fi, 0, sizeof fi);
    HRESULT hr = IBaseFilter_QueryFilterInfo(f, &fi);
    printf("%s: QueryFilterInfo %08lx graph %s\n", when, hr, fi.pGraph ? "set" : "null");
    if (fi.pGraph) IFilterGraph_Release(fi.pGraph);
}

int main(void) {
    CoInitialize(NULL);
    IGraphBuilder *gb = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb);
    printf("FilterGraph %08lx\n", hr);
    if (FAILED(hr)) return 0;
    GraphConfig *gc = (GraphConfig *)(INT_PTR)1;
    hr = IGraphBuilder_QueryInterface(gb, &IID_IGraphConfig_, (void **)&gc);
    printf("IGraphConfig %08lx %s\n", hr, gc == NULL ? "null" : gc == (GraphConfig *)(INT_PTR)1 ? "untouched" : "set");
    IBaseFilter *evr = NULL;
    hr = CoCreateInstance(&CLSID_EVR_, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&evr);
    printf("EVR %08lx\n", hr);
    if (FAILED(hr) || FAILED(IGraphBuilder_QueryInterface(gb, &IID_IGraphConfig_, (void **)&gc)) || !gc) return 0;
    int found, n;
    printf("AddFilter %08lx\n", IGraphBuilder_AddFilter(gb, evr, L"EVR"));
    n = count(gb, evr, &found);
    printf("after add: %d filters, ours %d\n", n, found);
    graph_of(evr, "added");
    printf("RemoveFilterEx %08lx\n", gc->lpVtbl->RemoveFilterEx(gc, evr, 0));
    n = count(gb, evr, &found);
    printf("after RemoveFilterEx: %d filters, ours %d\n", n, found);
    graph_of(evr, "removed");
    IBaseFilter *byname = NULL;
    hr = IGraphBuilder_FindFilterByName(gb, L"EVR", &byname);
    printf("FindFilterByName after removal %08lx %s\n", hr, byname ? "found" : "null");
    if (byname) IBaseFilter_Release(byname);
    printf("RemoveFilterEx again %08lx\n", gc->lpVtbl->RemoveFilterEx(gc, evr, 0));
    printf("RemoveFilter not in it %08lx\n", IGraphBuilder_RemoveFilter(gb, evr));
    printf("AddFilter again %08lx\n", IGraphBuilder_AddFilter(gb, evr, L"EVR"));
    byname = NULL;
    hr = IGraphBuilder_FindFilterByName(gb, L"EVR", &byname);
    printf("FindFilterByName %08lx %s\n", hr, byname == evr ? "ours" : byname ? "other" : "null");
    if (byname) IBaseFilter_Release(byname);
    printf("RemoveFilter %08lx\n", IGraphBuilder_RemoveFilter(gb, evr));
    n = count(gb, evr, &found);
    printf("after RemoveFilter: %d filters, ours %d\n", n, found);
    printf("RemoveFilterEx NULL %08lx\n", gc->lpVtbl->RemoveFilterEx(gc, NULL, 0));
    gc->lpVtbl->Release(gc);
    IBaseFilter_Release(evr);
    IGraphBuilder_Release(gb);
    return 0;
}
