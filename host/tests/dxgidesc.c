/* DXGI descriptions written to exactly their size: DXGI_OUTPUT_DESC is 92 bytes on x86 (an
 * x64-sized one overwrote Unity 2019's factory pointer, next to it on the stack), 96 on x64;
 * DXGI_ADAPTER_DESC(1) likewise. The expected lines are what Windows gives.
 * Built for x86 and x64. libs: -ldxgi -luuid
 * Build: i686-w64-mingw32-gcc -O2 dxgidesc.c -o dxgidesc.exe -ldxgi -luuid */
#define COBJMACROS
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include <dxgi.h>

static int kept(const unsigned char *b, size_t from, size_t to)
{
    for (size_t i = from; i < to; i++)
        if (b[i] != 0xcc)
            return 0;
    return 1;
}

int main(void)
{
    IDXGIFactory1 *f = 0;
    HRESULT hr = CreateDXGIFactory1(&IID_IDXGIFactory1, (void **)&f);
    printf("factory: %s\n", SUCCEEDED(hr) && f ? "ok" : "failed");
    IDXGIAdapter1 *a = 0;
    printf("adapter: %s\n", SUCCEEDED(IDXGIFactory1_EnumAdapters1(f, 0, &a)) && a ? "ok" : "failed");
    static unsigned char buf[512];
    memset(buf, 0xcc, sizeof(buf));
    IDXGIAdapter1_GetDesc(a, (DXGI_ADAPTER_DESC *)buf);
    printf("adapter desc: the rest kept %d\n", kept(buf, sizeof(DXGI_ADAPTER_DESC), sizeof(buf)));
    memset(buf, 0xcc, sizeof(buf));
    IDXGIAdapter1_GetDesc1(a, (DXGI_ADAPTER_DESC1 *)buf);
    printf("adapter desc1: the rest kept %d\n", kept(buf, sizeof(DXGI_ADAPTER_DESC1), sizeof(buf)));
    IDXGIOutput *o = 0;
    printf("output: %s\n", SUCCEEDED(IDXGIAdapter1_EnumOutputs(a, 0, &o)) && o ? "ok" : "failed");
    memset(buf, 0xcc, sizeof(buf));
    IDXGIOutput_GetDesc(o, (DXGI_OUTPUT_DESC *)buf);
    DXGI_OUTPUT_DESC *d = (DXGI_OUTPUT_DESC *)buf;
    printf("output desc: the rest kept %d, attached %d, monitor set %d\n", kept(buf, sizeof(DXGI_OUTPUT_DESC), sizeof(buf)), d->AttachedToDesktop, d->Monitor != 0);
    IDXGIOutput_Release(o);
    IDXGIAdapter1_Release(a);
    IDXGIFactory1_Release(f);
    return 0;
}
