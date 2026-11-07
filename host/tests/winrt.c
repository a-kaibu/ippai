/* What Unity's players call at startup and sake once got wrong: combase's HSTRING functions and
 * Windows Runtime activation (Unity 2021+), and (x86) kernel32's Interlocked* exports on an
 * unaligned LONG (mono 2.x keeps counters in packed structures; wasm atomics fault on those).
 * The expected lines are what Windows gives. Built for x86 and x64 (the same lines; x64's
 * kernel32 has no Interlocked* exports, so there the locked instructions themselves run).
 * Build: i686-w64-mingw32-gcc -O2 winrt.c -o winrt.exe */
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef void *HSTRG;
typedef struct { void *p[5]; } HEADER; /* HSTRING_HEADER: 20 bytes (x86), 24 (x64) */

static HRESULT (WINAPI *pCreate)(const WCHAR *, UINT32, HSTRG *);
static HRESULT (WINAPI *pCreateRef)(const WCHAR *, UINT32, HEADER *, HSTRG *);
static HRESULT (WINAPI *pDelete)(HSTRG);
static HRESULT (WINAPI *pDup)(HSTRG, HSTRG *);
static const WCHAR *(WINAPI *pRaw)(HSTRG, UINT32 *);
static UINT32 (WINAPI *pLen)(HSTRG);
static BOOL (WINAPI *pEmpty)(HSTRG);
static HRESULT (WINAPI *pCmp)(HSTRG, HSTRG, INT32 *);
static HRESULT (WINAPI *pRoInit)(int);
static HRESULT (WINAPI *pFactory)(HSTRG, const GUID *, void **);
static HRESULT (WINAPI *pActivate)(HSTRG, void **);

static void show(const char *what, HSTRG h)
{
    UINT32 n = 12345;
    const WCHAR *s = pRaw(h, &n);
    char a[64];
    int k = WideCharToMultiByte(CP_UTF8, 0, s, (int)n, a, sizeof(a) - 1, 0, 0);
    a[k] = 0;
    printf("%s: null=%d len=%u/%u empty=%d text='%s' terminated=%d\n", what, h == 0, n, pLen(h), pEmpty(h), a, s[n] == 0);
}

int main(void)
{
    HMODULE m = LoadLibraryA("combase.dll");
    printf("combase loaded: %d\n", m != 0);
#define GET(p, n) *(FARPROC *)&p = GetProcAddress(m, n)
    GET(pCreate, "WindowsCreateString");
    GET(pCreateRef, "WindowsCreateStringReference");
    GET(pDelete, "WindowsDeleteString");
    GET(pDup, "WindowsDuplicateString");
    GET(pRaw, "WindowsGetStringRawBuffer");
    GET(pLen, "WindowsGetStringLen");
    GET(pEmpty, "WindowsIsStringEmpty");
    GET(pCmp, "WindowsCompareStringOrdinal");
    GET(pRoInit, "RoInitialize");
    GET(pFactory, "RoGetActivationFactory");
    GET(pActivate, "RoActivateInstance");

    HRESULT hr = pRoInit(1); /* RO_INIT_MULTITHREADED */
    printf("RoInitialize: %s\n", SUCCEEDED(hr) ? "ok" : "failed");

    HSTRG a = (HSTRG)1, b = (HSTRG)1, e = (HSTRG)1, d = (HSTRG)1, r = (HSTRG)1;
    printf("create: %08lx\n", (unsigned long)pCreate(L"Windows.Gaming.Input.Gamepad", 28, &a));
    show("a", a);
    printf("create empty: %08lx\n", (unsigned long)pCreate(L"", 0, &e));
    show("empty", e);
    printf("create NULL 0: %08lx\n", (unsigned long)pCreate(NULL, 0, &e));
    printf("create NULL 3: %08lx\n", (unsigned long)pCreate(NULL, 3, &b));
    printf("create part: %08lx\n", (unsigned long)pCreate(L"abcdef", 3, &b));
    show("b", b);

    HEADER hdr;
    WCHAR buf[] = L"UI.ViewManagement";
    printf("reference: %08lx\n", (unsigned long)pCreateRef(buf, 17, &hdr, &r));
    show("r", r);
    printf("reference unterminated: %08lx\n", (unsigned long)pCreateRef(buf, 2, &hdr, &d));
    printf("duplicate: %08lx\n", (unsigned long)pDup(a, &d));
    printf("duplicate is the same string: %d\n", d == a);
    printf("delete: %08lx\n", (unsigned long)pDelete(a));
    show("duplicate after delete", d);
    pDelete(d);
    printf("duplicate reference: %08lx\n", (unsigned long)pDup(r, &d));
    printf("duplicate of a reference is a copy: %d\n", d != r);
    buf[0] = L'X';
    show("copy", d);
    show("reference after change", r);
    printf("delete NULL: %08lx\n", (unsigned long)pDelete(NULL));

    INT32 c = 99;
    HSTRG B, lo, ab;
    pCreate(L"B", 1, &B);
    pCreate(L"a", 1, &lo);
    pCreate(L"ab", 2, &ab);
    pCmp(B, lo, &c);
    printf("cmp B a: %d\n", c);
    pCmp(lo, ab, &c);
    printf("cmp a ab: %d\n", c);
    pCmp(ab, ab, &c);
    printf("cmp ab ab: %d\n", c);
    pCmp(NULL, NULL, &c);
    printf("cmp NULL NULL: %d\n", c);
    pCmp(ab, NULL, &c);
    printf("cmp ab NULL: %d\n", c);

    void *f = (void *)1;
    static const GUID unk = {0, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}};
    hr = pFactory(B, &unk, &f);
    printf("activation factory of an unknown class: %s, out %s\n", FAILED(hr) ? "failed" : "ok", f ? "set" : "NULL");

#ifndef _WIN64
    /* kernel32's exports (mingw's InterlockedIncrement is an intrinsic: these go through sake) */
    HMODULE k = GetModuleHandleA("kernel32.dll");
    LONG (WINAPI *inc)(LONG volatile *) = (void *)GetProcAddress(k, "InterlockedIncrement");
    LONG (WINAPI *dec)(LONG volatile *) = (void *)GetProcAddress(k, "InterlockedDecrement");
    LONG (WINAPI *xchg)(LONG volatile *, LONG) = (void *)GetProcAddress(k, "InterlockedExchange");
    LONG (WINAPI *xadd)(LONG volatile *, LONG) = (void *)GetProcAddress(k, "InterlockedExchangeAdd");
    LONG (WINAPI *cas)(LONG volatile *, LONG, LONG) = (void *)GetProcAddress(k, "InterlockedCompareExchange");
#else
    /* x64's kernel32 has no such exports: the compiler's locked instructions on the same dwords */
#define inc(p) InterlockedIncrement(p)
#define dec(p) InterlockedDecrement(p)
#define xchg(p, v) InterlockedExchange(p, v)
#define xadd(p, v) InterlockedExchangeAdd(p, v)
#define cas(p, n, c) InterlockedCompareExchange(p, n, c)
#endif
    static unsigned char mem[24] __attribute__((aligned(8)));
    for (int off = 0; off <= 12; off++)
    {
        memset(mem, 0xaa, sizeof(mem));
        LONG volatile *p = (LONG volatile *)(mem + off);
        *(LONG *)(mem + off) = 0x11223344;
        LONG r1 = inc(p), r2 = dec(p), r3 = xadd(p, 0x100), r4 = xchg(p, 7), r5 = cas(p, 9, 8), r6 = cas(p, 9, 7);
        int clean = 1;
        for (int i = 0; i < (int)sizeof(mem); i++)
            if ((i < off || i >= off + 4) && mem[i] != 0xaa) clean = 0;
        printf("offset %2d: %08lx %08lx %08lx %08lx %08lx %08lx -> %08lx neighbours %s\n", off, r1, r2, r3, r4, r5, r6,
               *(LONG *)(mem + off), clean ? "kept" : "CHANGED");
    }
    return 0;
}
