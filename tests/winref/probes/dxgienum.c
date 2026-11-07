/* DXGI's factory, adapters and outputs as games walk them (#79: two trial games ask for
 * IDXGIFactory6 and call EnumAdapterByGpuPreference; another enumerates past adapter 0):
 * CreateDXGIFactory1 with each IDXGIFactory IID, QueryInterface of every factory / adapter /
 * output interface of DXGI 1.6, the adapter list through EnumAdapters, EnumAdapters1,
 * EnumAdapterByGpuPreference and EnumWarpAdapter (whether each is the same object), the LUID
 * found again, CheckFeatureSupport, and the outputs. Every Windows since 8 lists Microsoft Basic
 * Render Driver (WARP: DXGI_ADAPTER_FLAG_SOFTWARE, no outputs) last; only the last adapter's
 * description is printed (the display adapters differ from machine to machine).
 * libs: -ldxgi -luuid */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <dxgi1_6.h>

static const GUID *factories[] = { &IID_IDXGIFactory, &IID_IDXGIFactory1, &IID_IDXGIFactory2, &IID_IDXGIFactory3,
                                   &IID_IDXGIFactory4, &IID_IDXGIFactory5, &IID_IDXGIFactory6, &IID_IDXGIFactory7 };
static const GUID *adapters[] = { &IID_IDXGIAdapter, &IID_IDXGIAdapter1, &IID_IDXGIAdapter2, &IID_IDXGIAdapter3, &IID_IDXGIAdapter4 };
static const GUID *outputs[] = { &IID_IDXGIOutput, &IID_IDXGIOutput1, &IID_IDXGIOutput2, &IID_IDXGIOutput3,
                                 &IID_IDXGIOutput4, &IID_IDXGIOutput5, &IID_IDXGIOutput6 };

static void qi(const char *what, IUnknown *o, const GUID **iids, int n) {
    printf("%s QueryInterface:", what);
    for (int i = 0; i < n; i++) {
        IUnknown *q = (IUnknown *)(INT_PTR)1;
        HRESULT hr = IUnknown_QueryInterface(o, iids[i], (void **)&q);
        printf(" %lx%s", hr, SUCCEEDED(hr) ? (q == o ? "=" : "+") : (q ? "!" : ""));
        if (SUCCEEDED(hr)) IUnknown_Release(q);
    }
    printf("\n");
}

static int count_outputs(IDXGIAdapter1 *a) {
    int n = 0;
    IDXGIOutput *o;
    while (IDXGIAdapter1_EnumOutputs(a, n, &o) == S_OK) { IDXGIOutput_Release(o); n++; }
    return n;
}

int main(void) {
    for (int i = 0; i < 8; i++) {
        void *f = NULL;
        HRESULT hr = CreateDXGIFactory1(factories[i], &f);
        printf("CreateDXGIFactory1 IDXGIFactory%d %lx\n", i, hr);
        if (f) IUnknown_Release((IUnknown *)f);
    }
    IDXGIFactory1 *f1 = NULL;
    if (FAILED(CreateDXGIFactory1(&IID_IDXGIFactory1, (void **)&f1))) return 1;
    qi("factory", (IUnknown *)f1, factories, 8);
    qi("factory as adapter", (IUnknown *)f1, adapters, 5);

    IDXGIAdapter1 *list[8] = { 0 };
    int n = 0;
    for (; n < 8; n++) {
        HRESULT hr = IDXGIFactory1_EnumAdapters1(f1, n, &list[n]);
        if (hr != S_OK) { printf("EnumAdapters1 %d %lx null %d\n", n, hr, list[n] == NULL); break; }
    }
    printf("adapters %d\n", n);
    for (int i = 0; i < n; i++) {
        DXGI_ADAPTER_DESC1 d;
        IDXGIAdapter1_GetDesc1(list[i], &d);
        printf("adapter %s: flags %x outputs %d", i == n - 1 ? "last" : "display", d.Flags, count_outputs(list[i]));
        if (i == n - 1) printf(" \"%ls\" vendor %x device %x dedicated %lu", d.Description, d.VendorId, d.DeviceId, (unsigned long)d.DedicatedVideoMemory);
        printf("\n");
        IDXGIAdapter *a0 = NULL;
        IDXGIFactory1_EnumAdapters(f1, i, &a0);
        printf("  EnumAdapters same %d\n", (void *)a0 == (void *)list[i]);
        if (a0) IDXGIAdapter_Release(a0);
        qi("  adapter", (IUnknown *)list[i], adapters, 5);
        if (i == 0) {
            IDXGIOutput *o;
            if (IDXGIAdapter1_EnumOutputs(list[i], 0, &o) == S_OK) {
                qi("  output", (IUnknown *)o, outputs, 7);
                IDXGIOutput6 *o6;
                if (SUCCEEDED(IDXGIOutput_QueryInterface(o, &IID_IDXGIOutput6, (void **)&o6))) {
                    DXGI_OUTPUT_DESC1 od;
                    HRESULT hr = IDXGIOutput6_GetDesc1(o6, &od);
                    printf("  GetDesc1 %lx bits %u colorspace %u attached %d\n", hr, od.BitsPerColor, od.ColorSpace, od.AttachedToDesktop);
                    UINT c0 = 0, c1 = 0;
                    IDXGIOutput6_GetDisplayModeList(o6, DXGI_FORMAT_R8G8B8A8_UNORM, 0, &c0, NULL);
                    hr = IDXGIOutput6_GetDisplayModeList1(o6, DXGI_FORMAT_R8G8B8A8_UNORM, 0, &c1, NULL);
                    printf("  GetDisplayModeList1 %lx same count %d\n", hr, c0 == c1);
                    UINT flags = 9;
                    hr = IDXGIOutput6_CheckHardwareCompositionSupport(o6, &flags);
                    printf("  CheckHardwareCompositionSupport %lx\n", hr);
                    IDXGIOutput6_Release(o6);
                }
                IDXGIOutput_Release(o);
            }
        }
    }

    IDXGIFactory6 *f6 = NULL;
    if (SUCCEEDED(IDXGIFactory1_QueryInterface(f1, &IID_IDXGIFactory6, (void **)&f6))) {
        for (int pref = 0; pref < 4; pref++)
            for (int i = 0; i <= n; i++) {
                IDXGIAdapter1 *a = (IDXGIAdapter1 *)(INT_PTR)1;
                HRESULT hr = IDXGIFactory6_EnumAdapterByGpuPreference(f6, i, pref, &IID_IDXGIAdapter1, (void **)&a);
                printf("EnumAdapterByGpuPreference %d pref %d %lx", i, pref, hr);
                if (hr == S_OK) {
                    int k = 0;
                    while (k < n && list[k] != a) k++;
                    printf(" is EnumAdapters1 %d", k < n ? k : -1);
                    IDXGIAdapter1_Release(a);
                } else printf(" null %d", a == NULL);
                printf("\n");
            }
        IDXGIAdapter1 *w = NULL;
        HRESULT hr = IDXGIFactory6_EnumWarpAdapter(f6, &IID_IDXGIAdapter1, (void **)&w);
        int k = 0;
        while (w && k < n && list[k] != w) k++;
        printf("EnumWarpAdapter %lx is EnumAdapters1 %d\n", hr, w ? (k < n ? k : -1) : -2);
        if (w) IDXGIAdapter1_Release(w);
        DXGI_ADAPTER_DESC1 d;
        IDXGIAdapter1_GetDesc1(list[0], &d);
        IDXGIAdapter1 *a = NULL;
        hr = IDXGIFactory6_EnumAdapterByLuid(f6, d.AdapterLuid, &IID_IDXGIAdapter1, (void **)&a);
        printf("EnumAdapterByLuid 0 %lx same %d\n", hr, a == list[0]);
        if (a) IDXGIAdapter1_Release(a);
        LUID bad = { 0x7777777, 0x77 };
        a = (IDXGIAdapter1 *)(INT_PTR)1;
        hr = IDXGIFactory6_EnumAdapterByLuid(f6, bad, &IID_IDXGIAdapter1, (void **)&a);
        printf("EnumAdapterByLuid unknown %lx null %d\n", hr, a == NULL);
        BOOL tearing = 9;
        hr = IDXGIFactory6_CheckFeatureSupport(f6, DXGI_FEATURE_PRESENT_ALLOW_TEARING, &tearing, sizeof tearing);
        printf("CheckFeatureSupport tearing %lx\n", hr);
        hr = IDXGIFactory6_CheckFeatureSupport(f6, DXGI_FEATURE_PRESENT_ALLOW_TEARING, &tearing, 8);
        printf("CheckFeatureSupport size 8 %lx\n", hr);
        printf("GetCreationFlags %x\n", IDXGIFactory6_GetCreationFlags(f6));
        IDXGIFactory6_Release(f6);
    }
    for (int i = 0; i < n; i++) printf("adapter %d Release %lu\n", i, IDXGIAdapter1_Release(list[i]));
    printf("factory Release %lu\n", IDXGIFactory1_Release(f1));
    return 0;
}
