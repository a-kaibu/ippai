/* ole32/oleaut32 basics that engines, installers and script hosts use: CreateStreamOnHGlobal
 * streams (Seek past the end, Read/Write counts, SetSize, Stat, Clone, CopyTo, the HGLOBAL
 * behind it), CLSID/GUID strings both ways (and the malformed ones), CoTaskMem, SafeArray
 * bounds/locks/element access/redim, BSTR reallocation and comparisons (VarBstrCmp, VarBstrCat),
 * VarCmp / VarAdd / VarMul on mixed types, VariantClear, and the HRESULTs.
 * libs: -lole32 -loleaut32 -luuid */
#include "../probe.h"
#include <objbase.h>
#include <oleauto.h>

#define H(expr) do { HRESULT hr_ = (expr); printf("%-70s = %08lx\n", #expr, (unsigned long)hr_); } while (0)

static void guid(const char *l, const GUID *g) {
    printf("%s %08lx-%04x-%04x-%02x%02x-%02x%02x%02x%02x%02x%02x\n", l, g->Data1, g->Data2, g->Data3, g->Data4[0], g->Data4[1], g->Data4[2], g->Data4[3], g->Data4[4], g->Data4[5], g->Data4[6], g->Data4[7]);
}
static void wout(const WCHAR *w) { for (; *w; w++) putchar(*w < 128 ? *w : '?'); }

static void var(const char *l, VARIANT *v) {
    printf("%s vt %u", l, v->vt);
    switch (v->vt) {
    case VT_I2: printf(" %d", v->iVal); break;
    case VT_I4: printf(" %ld", v->lVal); break;
    case VT_R8: printf(" %.17g", v->dblVal); break;
    case VT_R4: printf(" %.9g", v->fltVal); break;
    case VT_BOOL: printf(" %d", v->boolVal); break;
    case VT_BSTR: printf(" \""); wout(v->bstrVal); printf("\""); break;
    case VT_UI1: printf(" %u", v->bVal); break;
    case VT_I8: printf(" %lld", v->llVal); break;
    case VT_CY: printf(" %lld", v->cyVal.int64); break;
    case VT_DATE: printf(" %.17g", v->date); break;
    }
    printf("\n");
}

static VARIANT mk(VARTYPE t, double d, const WCHAR *s) {
    VARIANT v;
    VariantInit(&v);
    v.vt = t;
    if (t == VT_I4) v.lVal = (LONG)d;
    else if (t == VT_I2) v.iVal = (SHORT)d;
    else if (t == VT_R8) v.dblVal = d;
    else if (t == VT_BOOL) v.boolVal = d ? VARIANT_TRUE : VARIANT_FALSE;
    else if (t == VT_BSTR) v.bstrVal = SysAllocString(s);
    else if (t == VT_UI1) v.bVal = (BYTE)d;
    return v;
}

int main(void) {
    /* streams */
    IStream *s = NULL;
    H(CreateStreamOnHGlobal(NULL, TRUE, &s));
    ULONG n = 99;
    LARGE_INTEGER li;
    ULARGE_INTEGER pos;
    H(s->lpVtbl->Write(s, "hello world", 11, &n)); printf("wrote %lu\n", n);
    li.QuadPart = 0;
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_CUR, &pos)); printf("pos %llu\n", pos.QuadPart);
    li.QuadPart = 20;
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_SET, &pos)); printf("pos %llu\n", pos.QuadPart);
    char b[64] = "";
    n = 99;
    H(s->lpVtbl->Read(s, b, 4, &n)); printf("read past end %lu\n", n);
    H(s->lpVtbl->Write(s, "X", 1, &n));
    STATSTG st;
    H(s->lpVtbl->Stat(s, &st, STATFLAG_NONAME)); printf("size %llu type %lu name null %d\n", st.cbSize.QuadPart, st.type, st.pwcsName == NULL);
    li.QuadPart = -1;
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_SET, &pos)); printf("pos %llu\n", pos.QuadPart);
    li.QuadPart = -3;
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_END, &pos)); printf("pos %llu\n", pos.QuadPart);
    H(s->lpVtbl->Seek(s, li, 7, &pos));
    li.QuadPart = 0;
    s->lpVtbl->Seek(s, li, STREAM_SEEK_SET, NULL);
    memset(b, 0, sizeof b);
    H(s->lpVtbl->Read(s, b, 64, &n));
    printf("read %lu:", n); for (ULONG i = 0; i < n; i++) printf(" %02x", (unsigned char)b[i]); printf("\n");
    ULARGE_INTEGER sz; sz.QuadPart = 5;
    H(s->lpVtbl->SetSize(s, sz));
    H(s->lpVtbl->Stat(s, &st, STATFLAG_NONAME)); printf("size %llu\n", st.cbSize.QuadPart);
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_CUR, &pos)); printf("pos after shrink %llu\n", pos.QuadPart);
    IStream *c = NULL;
    H(s->lpVtbl->Clone(s, &c));
    li.QuadPart = 1;
    c->lpVtbl->Seek(c, li, STREAM_SEEK_SET, NULL);
    li.QuadPart = 0;
    H(s->lpVtbl->Seek(s, li, STREAM_SEEK_CUR, &pos)); printf("clone has its own position: %llu\n", pos.QuadPart);
    IStream *d = NULL;
    CreateStreamOnHGlobal(NULL, TRUE, &d);
    ULARGE_INTEGER rd, wr, all; all.QuadPart = 100;
    H(c->lpVtbl->CopyTo(c, d, all, &rd, &wr)); printf("copied %llu %llu\n", rd.QuadPart, wr.QuadPart);
    HGLOBAL g = NULL;
    H(GetHGlobalFromStream(d, &g));
    printf("hglobal size >= 4: %d first %.4s\n", GlobalSize(g) >= 4, (char *)GlobalLock(g));
    GlobalUnlock(g);
    H(s->lpVtbl->Commit(s, 0));
    H(s->lpVtbl->Revert(s));
    ULARGE_INTEGER z0; z0.QuadPart = 0;
    H(s->lpVtbl->LockRegion(s, z0, sz, 0));
    printf("release %lu %lu %lu\n", c->lpVtbl->Release(c), d->lpVtbl->Release(d), s->lpVtbl->Release(s));
    HGLOBAL mine = GlobalAlloc(GMEM_MOVEABLE, 8);
    memcpy(GlobalLock(mine), "abcdefgh", 8);
    GlobalUnlock(mine);
    H(CreateStreamOnHGlobal(mine, FALSE, &s));
    H(s->lpVtbl->Stat(s, &st, STATFLAG_NONAME)); printf("size on given hglobal %llu\n", st.cbSize.QuadPart);
    s->lpVtbl->Release(s);
    printf("hglobal still there %d\n", GlobalSize(mine) == 8);
    H(CreateStreamOnHGlobal(NULL, TRUE, NULL));

    /* GUIDs */
    CLSID id;
    H(CLSIDFromString(L"{00020400-0000-0000-C000-000000000046}", &id)); guid("parsed", &id);
    H(CLSIDFromString(L"{00020400-0000-0000-c000-000000000046}", &id));
    H(CLSIDFromString(L"00020400-0000-0000-C000-000000000046", &id));
    H(CLSIDFromString(L"{00020400-0000-0000-C000-00000000004}", &id));
    H(CLSIDFromString(L"{0002040G-0000-0000-C000-000000000046}", &id));
    H(CLSIDFromString(L"", &id)); guid("empty", &id);
    H(CLSIDFromString(NULL, &id)); guid("null", &id);
    H(IIDFromString(L"{00020400-0000-0000-C000-000000000046}", &id));
    H(IIDFromString(L"{00020400-0000-0000-C000-00000000004x}", &id));
    WCHAR w[40];
    int k = StringFromGUID2(&id, w, 40);
    printf("StringFromGUID2 %d ", k); wout(w); printf("\n");
    printf("StringFromGUID2 short %d\n", StringFromGUID2(&id, w, 38));
    LPOLESTR os = NULL;
    H(StringFromCLSID(&id, &os)); wout(os); printf("\n");
    CoTaskMemFree(os);
    H(CLSIDFromProgID(L"No.Such.ProgId", &id));
    printf("IsEqualGUID %d\n", IsEqualGUID(&IID_IUnknown, &IID_IUnknown));

    /* CoTaskMem */
    void *m = CoTaskMemAlloc(16);
    m = CoTaskMemRealloc(m, 64);
    printf("realloc ok %d zero alloc %d\n", m != NULL, CoTaskMemAlloc(0) != NULL);
    CoTaskMemFree(m);
    CoTaskMemFree(NULL);

    /* SAFEARRAY */
    SAFEARRAYBOUND bd[2] = { { 3, 1 }, { 2, -1 } };
    SAFEARRAY *sa = SafeArrayCreate(VT_I4, 2, bd);
    printf("dims %u elem %lu features %x\n", SafeArrayGetDim(sa), SafeArrayGetElemsize(sa), sa->fFeatures);
    LONG lb, ub;
    H(SafeArrayGetLBound(sa, 1, &lb)); H(SafeArrayGetUBound(sa, 1, &ub)); printf("dim1 %ld..%ld\n", lb, ub);
    H(SafeArrayGetLBound(sa, 2, &lb)); H(SafeArrayGetUBound(sa, 2, &ub)); printf("dim2 %ld..%ld\n", lb, ub);
    H(SafeArrayGetLBound(sa, 3, &lb));
    LONG ix[2] = { 1, -1 }, val = 5;
    H(SafeArrayPutElement(sa, ix, &val));
    ix[0] = 2; ix[1] = 1; val = 7;
    H(SafeArrayPutElement(sa, ix, &val));
    ix[0] = 3; ix[1] = 0; val = 9;
    H(SafeArrayPutElement(sa, ix, &val));
    ix[0] = 4;
    H(SafeArrayPutElement(sa, ix, &val));
    ix[0] = 2; ix[1] = 1; val = 0;
    H(SafeArrayGetElement(sa, ix, &val)); printf("got %ld\n", val);
    LONG *data;
    H(SafeArrayAccessData(sa, (void **)&data));
    printf("layout:"); for (int i = 0; i < 6; i++) printf(" %ld", data[i]); printf(" locks %lu\n", sa->cLocks);
    H(SafeArrayDestroy(sa));
    H(SafeArrayUnaccessData(sa));
    H(SafeArrayUnaccessData(sa));
    SAFEARRAYBOUND nb = { 5, 0 };
    SAFEARRAY *v1 = SafeArrayCreateVector(VT_UI1, 0, 4);
    H(SafeArrayRedim(v1, &nb));
    H(SafeArrayGetUBound(v1, 1, &ub)); printf("redim ub %ld\n", ub);
    H(SafeArrayDestroy(v1));
    H(SafeArrayDestroy(sa));
    H(SafeArrayDestroy(NULL));
    printf("create bad vt %d\n", SafeArrayCreate(VT_EMPTY, 1, bd) != NULL);

    /* BSTR */
    BSTR x = SysAllocString(L"abc");
    /* (one call a statement: gcc evaluates arguments right to left) */
    INT ra = SysReAllocString(&x, L"longer string");
    printf("realloc %d len %u\n", ra, SysStringLen(x));
    ra = SysReAllocStringLen(&x, L"xy", 5);
    printf("realloclen %d len %u\n", ra, SysStringLen(x));
    SysFreeString(x);
    BSTR a1 = SysAllocString(L"abc"), a2 = SysAllocString(L"ABC"), a3 = SysAllocString(L"abd"), e0 = SysAllocString(L"");
    printf("VarBstrCmp %ld %ld %ld %ld %ld\n", VarBstrCmp(a1, a2, 0x411, 0), VarBstrCmp(a1, a2, 0x411, NORM_IGNORECASE), VarBstrCmp(a1, a3, 0x411, 0), VarBstrCmp(NULL, e0, 0x411, 0), VarBstrCmp(a3, a1, 0x411, 0));
    BSTR cat = NULL;
    H(VarBstrCat(a1, NULL, &cat)); printf("cat len %u\n", SysStringLen(cat));
    printf("SysStringByteLen null %u alloc null %d\n", SysStringByteLen(NULL), SysAllocString(NULL) == NULL);

    /* variant arithmetic and comparison */
    VARIANT l = mk(VT_I4, 7, 0), r2 = mk(VT_R8, 2.5, 0), bs = mk(VT_BSTR, 0, L"10"), bo = mk(VT_BOOL, 1, 0), i2 = mk(VT_I2, 30000, 0), u1 = mk(VT_UI1, 200, 0), out;
    VARIANT *ops[][2] = { { &l, &r2 }, { &l, &bs }, { &bs, &bs }, { &l, &bo }, { &i2, &i2 }, { &u1, &u1 }, { &bo, &bo }, { &bs, &r2 } };
    for (int i = 0; i < 8; i++) {
        VariantInit(&out);
        HRESULT hr = VarAdd(ops[i][0], ops[i][1], &out);
        printf("add %u+%u %08lx", ops[i][0]->vt, ops[i][1]->vt, (unsigned long)hr); var("", &out);
        VariantClear(&out);
        hr = VarMul(ops[i][0], ops[i][1], &out);
        printf("mul %u*%u %08lx", ops[i][0]->vt, ops[i][1]->vt, (unsigned long)hr); var("", &out);
        VariantClear(&out);
        printf("cmp %u,%u %ld\n", ops[i][0]->vt, ops[i][1]->vt, (long)VarCmp(ops[i][0], ops[i][1], 0x411, 0));
    }
    VARIANT ne = mk(VT_I4, -3, 0);
    VariantInit(&out); H(VarNeg(&ne, &out)); var("neg", &out);
    VariantInit(&out); H(VarDiv(&l, &r2, &out)); var("div", &out);
    VariantInit(&out); H(VarIdiv(&l, &r2, &out)); var("idiv", &out);
    VariantInit(&out); H(VarMod(&l, &r2, &out)); var("mod", &out);
    VARIANT zero = mk(VT_I4, 0, 0);
    VariantInit(&out); H(VarDiv(&l, &zero, &out));
    VariantInit(&out); H(VarIdiv(&l, &zero, &out));
    H(VariantClear(&bs)); printf("cleared vt %u\n", bs.vt);
    VARIANT bad; VariantInit(&bad); bad.vt = 0x7777;
    H(VariantClear(&bad));
    return 0;
}
