/* d3d12.dll through a delay-load import (UnityPlayer.dll's way, no __try around it: on a
 * system without the DLL the helper raises c06d007e and Unity 6 crashed at start, corpus
 * dl_RJ01636335 and 15 more): the DLL is there, and D3D12CreateDevice says the adapter can't
 * (as a PC whose GPU has no Direct3D 12 driver), the device pointer cleared, so a program
 * falls back to Direct3D 11. The same through LoadLibrary/GetProcAddress. Unity 6000.3/6000.4
 * import both by ordinal (101 D3D12CreateDevice, 102 D3D12GetDebugInterface, as Windows'
 * d3d12.dll numbers them): without the ordinals the helper raised c06d007f (dl_RJ01610703,
 * dl_RJ01684055, dl_RJ01690707). ord101/ord102 are those imports (tests/host-tests.sh).
 * libs: -ld3d12 -ld3d11
 * Build: the d3d12 import library is a delay-load one (tests/host-tests.sh, dlltool -y) */
#include <stdio.h>
#include <windows.h>
#include <d3d11.h>

typedef HRESULT (WINAPI *create_t)(void *, int, const GUID *, void **);
HRESULT WINAPI D3D12CreateDevice(void *adapter, int level, const GUID *riid, void **device);
HRESULT WINAPI D3D12GetDebugInterface(const GUID *riid, void **out);
HRESULT WINAPI ord101(void *adapter, int level, const GUID *riid, void **device);
HRESULT WINAPI ord102(const GUID *riid, void **out);

static const GUID iid_dev = {0x189819f1, 0x1db6, 0x4b57, {0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7}};
static const GUID iid_dbg = {0x344488b7, 0x6846, 0x474b, {0xb9, 0x89, 0xf0, 0x27, 0x44, 0x82, 0x45, 0xe0}};

int main(void)
{
    void *dev = (void *)1, *dbg = (void *)1;
    HRESULT hr = D3D12CreateDevice(NULL, 0xb000, &iid_dev, &dev);
    printf("delay-load D3D12CreateDevice: %s, device %s\n", FAILED(hr) ? "failed" : "ok", dev ? "set" : "null");
    hr = D3D12CreateDevice(NULL, 0xb000, &iid_dev, NULL);
    printf("support check: %s\n", hr == S_FALSE ? "supported" : FAILED(hr) ? "failed" : "?");
    hr = D3D12GetDebugInterface(&iid_dbg, &dbg);
    printf("debug interface: %s, %s\n", FAILED(hr) ? "failed" : "ok", dbg ? "set" : "null");
    dev = (void *)1;
    hr = ord101(NULL, 0xb000, &iid_dev, &dev);
    printf("by ordinal 101: %s, device %s\n", FAILED(hr) ? "failed" : "ok", dev ? "set" : "null");
    dbg = (void *)1;
    hr = ord102(&iid_dbg, &dbg);
    printf("by ordinal 102: %s, %s\n", FAILED(hr) ? "failed" : "ok", dbg ? "set" : "null");
    HMODULE m = LoadLibraryA("d3d12.dll");
    create_t f = m ? (create_t)GetProcAddress(m, "D3D12CreateDevice") : NULL;
    printf("LoadLibrary: %s, GetProcAddress: %s\n", m ? "ok" : "none", f ? "ok" : "none");
    FARPROC o101 = m ? GetProcAddress(m, (LPCSTR)101) : NULL;
    FARPROC o102 = m ? GetProcAddress(m, (LPCSTR)102) : NULL;
    FARPROC dbgf = m ? GetProcAddress(m, "D3D12GetDebugInterface") : NULL;
    int o101same = o101 == (FARPROC)f;
    int o102same = o102 == dbgf;
    printf("ordinal 101 is D3D12CreateDevice: %d, 102 is D3D12GetDebugInterface: %d\n", o101same, o102same);
    ID3D11Device *d11 = NULL;
    hr = D3D11CreateDevice(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0, D3D11_SDK_VERSION, &d11, NULL, NULL);
    printf("then D3D11CreateDevice: %s\n", SUCCEEDED(hr) && d11 ? "ok" : "failed");
    if (d11)
        d11->lpVtbl->Release(d11);
    return 0;
}
