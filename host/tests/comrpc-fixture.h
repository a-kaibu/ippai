#include <windows.h>
#include <objbase.h>
#include <oleauto.h>
static const CLSID test_class = {0x532fa440,0x1561,0x4111,{0x88,0x79,0x4f,0x93,0x81,0x60,0x21,0x01}};
static const IID test_iid = {0x532fa441,0x1561,0x4111,{0x88,0x79,0x4f,0x93,0x81,0x60,0x21,0x01}};
#define CLASS_TEXT "{532fa440-1561-4111-8879-4f9381602101}"
typedef struct Test Test;
typedef struct TestVtbl {
    HRESULT (WINAPI *QueryInterface)(Test *, REFIID, void **);
    ULONG (WINAPI *AddRef)(Test *);
    ULONG (WINAPI *Release)(Test *);
    HRESULT (WINAPI *GetValue)(Test *, LONG, LONG *);
    HRESULT (WINAPI *Echo)(Test *, BSTR, BSTR *);
    HRESULT (WINAPI *Fail)(Test *, LONG *);
} TestVtbl;
struct Test { const TestVtbl *lpVtbl; LONG refs; };
