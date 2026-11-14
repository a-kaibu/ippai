/* Limits the full-read review (b10) found sake aborting or leaking on, each answered as Windows
 * answers: ScriptBreak with a negative or huge count, WIC bitmaps and streams of sizes no
 * memory holds and strides shorter than a row, WindowsCreateString's length checked before its
 * characters are read, WMI queries made and released in a loop (each used to leak its
 * enumerator and object), and UDP datagrams sent to a socket nobody reads (Windows drops them
 * once its receive buffer is full; sake kept them all).
 * libs: -lusp10 -lole32 -loleaut32 -lwindowscodecs -lwbemuuid -luuid -lws2_32 */
#define __USE_MINGW_ANSI_STDIO 0
#define COBJMACROS
#define INITGUID
#include <winsock2.h>
#include "../probe.h"
#include <objbase.h>
#include <usp10.h>
#include <wincodec.h>
#include <wbemidl.h>

static void usp(void) {
    WCHAR t[4] = { 'a', ' ', 'b', 0 };
    SCRIPT_ITEM items[8];
    int n = 0;
    ScriptItemize(t, 3, 8, NULL, NULL, items, &n);
    SCRIPT_LOGATTR la[4];
    printf("ScriptBreak 3 %#lx\n", ScriptBreak(t, 3, &items[0].a, la));
    printf("ScriptBreak -1 %#lx\n", ScriptBreak(t, -1, &items[0].a, la));
    printf("ScriptBreak 0 %#lx\n", ScriptBreak(t, 0, &items[0].a, la));
    printf("ScriptBreak null %#lx\n", ScriptBreak(NULL, 3, &items[0].a, la));
}

static void wic(void) {
    IWICImagingFactory *f = NULL;
    if (FAILED(CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER, &IID_IWICImagingFactory, (void **)&f))) {
        printf("no WIC\n");
        return;
    }
    IWICBitmap *b = NULL;
    HRESULT hr = IWICImagingFactory_CreateBitmap(f, 0x10000, 0x10000, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad, &b);
    printf("CreateBitmap 65536x65536 %s\n", FAILED(hr) ? "fails" : "ok");
    if (b) IWICBitmap_Release(b), b = NULL;
    hr = IWICImagingFactory_CreateBitmap(f, 0xffffffff, 2, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad, &b);
    printf("CreateBitmap 4G x 2 %s\n", FAILED(hr) ? "fails" : "ok");
    if (b) IWICBitmap_Release(b), b = NULL;
    BYTE px[64] = { 0 };
    hr = IWICImagingFactory_CreateBitmapFromMemory(f, 4, 2, &GUID_WICPixelFormat32bppBGRA, 8, 64, px, &b);
    printf("FromMemory short stride %#lx\n", hr);
    if (b) IWICBitmap_Release(b), b = NULL;
    hr = IWICImagingFactory_CreateBitmapFromMemory(f, 4, 2, &GUID_WICPixelFormat32bppBGRA, 16, 20, px, &b);
    printf("FromMemory short buffer %#lx\n", hr);
    if (b) IWICBitmap_Release(b), b = NULL;
    hr = IWICImagingFactory_CreateBitmapFromMemory(f, 4, 2, &GUID_WICPixelFormat32bppBGRA, 16, 32, px, &b);
    printf("FromMemory exact %#lx\n", hr);
    if (b) {
        /* a rect past the bitmap */
        WICRect r = { 2, 1, 4, 4 };
        IWICBitmapLock *l = NULL;
        printf("Lock past %#lx\n", IWICBitmap_Lock(b, &r, WICBitmapLockRead, &l));
        if (l) IWICBitmapLock_Release(l);
        BYTE out[64];
        WICRect r2 = { 3, 0, 0x7fffffff, 1 };
        printf("CopyPixels wide %#lx\n", IWICBitmap_CopyPixels(b, &r2, 64, 64, out));
        IWICBitmap_Release(b);
    }
    IWICStream *s = NULL;
    IWICImagingFactory_CreateStream(f, &s);
    if (s) {
        BYTE m[16] = { 1, 2, 3, 4 };
        IWICStream_InitializeFromMemory(s, m, 16);
        LARGE_INTEGER at;
        at.QuadPart = 0x7fffffffffffffffLL;
        ULARGE_INTEGER pos;
        printf("Seek far %#lx\n", IWICStream_Seek(s, at, STREAM_SEEK_SET, &pos));
        BYTE o[4];
        ULONG got = 99;
        HRESULT rr = IWICStream_Read(s, o, 4, &got);
        printf("Read after %#lx %lu\n", rr, got);
        IWICStream_Release(s);
    }
    IWICImagingFactory_Release(f);
}

typedef HRESULT(WINAPI *create_fn)(const WCHAR *, UINT32, void **);
typedef HRESULT(WINAPI *delete_fn)(void *);
static void winrt(void) {
    HMODULE m = LoadLibraryA("combase.dll");
    create_fn create = m ? (create_fn)GetProcAddress(m, "WindowsCreateString") : NULL;
    delete_fn del = m ? (delete_fn)GetProcAddress(m, "WindowsDeleteString") : NULL;
    if (!create || !del) {
        printf("no WindowsCreateString\n");
        return;
    }
    WCHAR t[3] = { 'h', 'i', 0 };
    void *h = NULL;
    printf("create 2 %#lx\n", create(t, 2, &h));
    del(h);
    h = NULL;
    /* a length no memory holds: refused before the characters are read */
    printf("create 0x7fffffff %#lx\n", create(t, 0x7fffffff, &h));
    printf("create 0xffffffff %#lx\n", create(t, 0xffffffff, &h));
}

static void wmi(void) {
    IWbemLocator *loc = NULL;
    if (FAILED(CoCreateInstance(&CLSID_WbemLocator, NULL, CLSCTX_INPROC_SERVER, &IID_IWbemLocator, (void **)&loc))) {
        printf("no WMI\n");
        return;
    }
    IWbemServices *svc = NULL;
    BSTR ns = SysAllocString(L"ROOT\\CIMV2"), lang = SysAllocString(L"WQL"), q = SysAllocString(L"SELECT * FROM Win32_Processor");
    HRESULT hr = IWbemLocator_ConnectServer(loc, ns, NULL, NULL, NULL, 0, NULL, NULL, &svc);
    if (FAILED(hr)) {
        printf("ConnectServer %#lx\n", hr);
    } else {
        CoSetProxyBlanket((IUnknown *)svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, NULL, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE);
        int found = 0, refs = 0;
        for (int i = 0; i < 200; i++) {
            IEnumWbemClassObject *e = NULL;
            if (FAILED(IWbemServices_ExecQuery(svc, lang, q, WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, NULL, &e)) || !e) continue;
            IWbemClassObject *o = NULL;
            ULONG n = 0;
            if (SUCCEEDED(IEnumWbemClassObject_Next(e, WBEM_INFINITE, 1, &o, &n)) && n == 1) {
                VARIANT v;
                VariantInit(&v);
                if (SUCCEEDED(IWbemClassObject_Get(o, L"NumberOfCores", 0, &v, NULL, NULL)) && v.vt == VT_I4) found++;
                VariantClear(&v);
                IWbemClassObject_AddRef(o);
                refs += IWbemClassObject_Release(o) >= 1;
                IWbemClassObject_Release(o);
            }
            IEnumWbemClassObject_Release(e);
        }
        printf("wmi queries %d, AddRef/Release kept %d\n", found, refs);
        IWbemServices_Release(svc);
    }
    SysFreeString(ns);
    SysFreeString(lang);
    SysFreeString(q);
    IWbemLocator_Release(loc);
}

static void udp(void) {
    WSADATA d;
    WSAStartup(MAKEWORD(2, 2), &d);
    SOCKET r = socket(AF_INET, SOCK_DGRAM, 0), s = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in a = { 0 };
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(r, (struct sockaddr *)&a, sizeof a);
    int len = sizeof a;
    getsockname(r, (struct sockaddr *)&a, &len);
    int buf = 16384;
    setsockopt(r, SOL_SOCKET, SO_RCVBUF, (char *)&buf, sizeof buf);
    char data[1000];
    memset(data, 7, sizeof data);
    int sent = 0;
    for (int i = 0; i < 3000; i++) sent += sendto(s, data, sizeof data, 0, (struct sockaddr *)&a, sizeof a) == sizeof data;
    Sleep(100);
    u_long nb = 1;
    ioctlsocket(r, FIONBIO, &nb);
    int got = 0;
    while (recv(r, data, sizeof data, 0) > 0) got++;
    printf("udp sent %d, kept some %d, dropped some %d\n", sent, got > 0, got < sent);
    /* huge buffer options: sends still go and come back */
    int huge = 0x7fffffff;
    setsockopt(r, SOL_SOCKET, SO_RCVBUF, (char *)&huge, sizeof huge);
    setsockopt(s, SOL_SOCKET, SO_SNDBUF, (char *)&huge, sizeof huge);
    sendto(s, data, 10, 0, (struct sockaddr *)&a, sizeof a);
    Sleep(50);
    printf("udp after huge buffers %d\n", recv(r, data, sizeof data, 0));
    /* a WSABUF count no memory holds */
    WSABUF wb = { 4, data };
    DWORD n = 0;
    int rc = WSASendTo(s, &wb, 0x40000000, &n, 0, (struct sockaddr *)&a, sizeof a, NULL, NULL);
    printf("WSASendTo huge count %d\n", rc == SOCKET_ERROR);
    closesocket(r);
    closesocket(s);
    WSACleanup();
}

int main(void) {
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    CoInitializeSecurity(NULL, -1, NULL, NULL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, NULL, EOAC_NONE, NULL);
    usp();
    wic();
    winrt();
    wmi();
    udp();
    return 0;
}
