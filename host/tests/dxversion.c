/* The DirectX SDK's GetDXVersion() (DX8 SDK sample, as SinsAbell 体験版 and other games of its
 * time link it): DirectDraw (DX2), IDirectDraw2 + DirectInput (DX3), IDirectDrawSurface3 (DX5),
 * IDirectDrawSurface4 (DX6), a DirectMusic object (DX6.1), IDirectDraw7 (DX7), d3d8.dll's
 * Direct3DCreate8 (DX8). Without CLSID_DirectMusic it stopped at 0x600 and SinsAbell refused to
 * start ("DirectX 8.0 以降のバージョンのインストールが必要です"). Also DirectMusic's own
 * behaviour: QueryInterface of its interfaces and not others, reference counts, aggregation
 * refused.
 * libs: -lddraw -lole32 -luuid -ldxguid */
#include <stdio.h>
#include <windows.h>
#include <ddraw.h>
#include <dmusicc.h>

static DWORD dx_version(void)
{
    DWORD v = 0;
    HMODULE dd = LoadLibraryA("DDRAW.DLL");
    if (!dd) return 0;
    HRESULT (WINAPI *create)(GUID *, IDirectDraw **, IUnknown *) = (void *)GetProcAddress(dd, "DirectDrawCreate");
    IDirectDraw *d = NULL;
    if (!create || FAILED(create(NULL, &d, NULL))) return 0;
    v = 0x200;
    IDirectDraw2 *d2 = NULL;
    if (FAILED(IDirectDraw_QueryInterface(d, &IID_IDirectDraw2, (void **)&d2))) goto out;
    IDirectDraw2_Release(d2);
    HMODULE di = LoadLibraryA("DINPUT.DLL");
    if (!di || !GetProcAddress(di, "DirectInputCreateA")) goto out;
    v = 0x300;
    if (FAILED(IDirectDraw_SetCooperativeLevel(d, NULL, DDSCL_NORMAL))) goto out;
    DDSURFACEDESC desc = {sizeof desc};
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    IDirectDrawSurface *s = NULL;
    if (FAILED(IDirectDraw_CreateSurface(d, &desc, &s, NULL))) goto out;
    IDirectDrawSurface3 *s3 = NULL;
    if (FAILED(IDirectDrawSurface_QueryInterface(s, &IID_IDirectDrawSurface3, (void **)&s3))) goto surf;
    IDirectDrawSurface3_Release(s3);
    v = 0x500;
    IDirectDrawSurface4 *s4 = NULL;
    if (FAILED(IDirectDrawSurface_QueryInterface(s, &IID_IDirectDrawSurface4, (void **)&s4))) goto surf;
    IDirectDrawSurface4_Release(s4);
    v = 0x600;
    CoInitialize(NULL);
    IDirectMusic *m = NULL;
    if (FAILED(CoCreateInstance(&CLSID_DirectMusic, NULL, CLSCTX_INPROC_SERVER, &IID_IDirectMusic, (void **)&m))) goto surf;
    IDirectMusic_Release(m);
    v = 0x601;
    IDirectDraw7 *d7 = NULL;
    HRESULT (WINAPI *create_ex)(GUID *, void **, REFIID, IUnknown *) = (void *)GetProcAddress(dd, "DirectDrawCreateEx");
    if (!create_ex || FAILED(create_ex(NULL, (void **)&d7, &IID_IDirectDraw7, NULL))) goto surf;
    IDirectDraw7_Release(d7);
    v = 0x700;
    HMODULE d8 = LoadLibraryA("D3D8.DLL");
    if (d8 && GetProcAddress(d8, "Direct3DCreate8")) v = 0x800;
surf:
    IDirectDrawSurface_Release(s);
out:
    IDirectDraw_Release(d);
    return v;
}

static const GUID iid_unknown = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
static const GUID iid_classfactory = {1, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};

int main(void)
{
    printf("GetDXVersion: %x\n", (unsigned)dx_version());
    IDirectMusic *m = (void *)1;
    HRESULT hr = CoCreateInstance(&CLSID_DirectMusic, NULL, CLSCTX_INPROC_SERVER, &IID_IDirectMusic, (void **)&m);
    printf("CoCreateInstance(IDirectMusic): %s\n", SUCCEEDED(hr) && m ? "ok" : "failed");
    if (FAILED(hr)) return 1;
    void *p;
    const struct { const char *name; const GUID *iid; } ifs[] = {
        {"IUnknown", &iid_unknown}, {"IDirectMusic2", &IID_IDirectMusic2}, {"IDirectMusic8", &IID_IDirectMusic8},
        {"IClassFactory", &iid_classfactory}, {"IDirectSound", &IID_IDirectSound}, {"IDirectDraw", &IID_IDirectDraw},
    };
    for (int i = 0; i < 6; i++) {
        p = (void *)1;
        hr = IDirectMusic_QueryInterface(m, ifs[i].iid, &p);
        printf("QueryInterface(%s): %s, %s\n", ifs[i].name, hr == S_OK ? "S_OK" : hr == E_NOINTERFACE ? "E_NOINTERFACE" : "?",
               p == NULL ? "null" : p == (void *)m ? "same object" : "other object");
        if (hr == S_OK) ((IUnknown *)p)->lpVtbl->Release((IUnknown *)p);
    }
    ULONG a = IDirectMusic_AddRef(m), b = IDirectMusic_Release(m);
    printf("AddRef %lu, Release %lu\n", a, b);
    printf("last Release: %lu\n", IDirectMusic_Release(m));
    IUnknown outer = {0};
    m = (void *)1;
    hr = CoCreateInstance(&CLSID_DirectMusic, &outer, CLSCTX_INPROC_SERVER, &iid_unknown, (void **)&m);
    printf("aggregated: %s, %s\n", hr == CLASS_E_NOAGGREGATION ? "CLASS_E_NOAGGREGATION" : SUCCEEDED(hr) ? "ok" : "other error", m ? "set" : "null");
    m = (void *)1;
    hr = CoCreateInstance(&CLSID_DirectMusic, NULL, CLSCTX_INPROC_SERVER, &IID_IDirectSound, (void **)&m);
    printf("CoCreateInstance(IDirectSound): %s, %s\n", hr == E_NOINTERFACE ? "E_NOINTERFACE" : SUCCEEDED(hr) ? "ok" : "other error", m ? "set" : "null");
    return 0;
}
