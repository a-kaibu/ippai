/* shlwapi path helpers and oleaut32 BSTR/VARIANT basics as installers, launchers and script
 * engines use them: PathCombine/Append/Canonicalize/RemoveFileSpec/FindFileName/FindExtension/
 * StripPath/Add-RemoveBackslash/IsRelative on ordinary, relative, UNC, drive-only and
 * code page 932 paths (0x5c as a trail byte); SysAllocString lengths; VariantChangeType between
 * integers, reals, strings, booleans and dates in the Japanese locale, with the HRESULTs of
 * overflows and bad strings.
 * libs: -lshlwapi -loleaut32 */
#include "../probe.h"
#include <shlwapi.h>
#include <oleauto.h>

static void dbits(double d) { unsigned long long u; memcpy(&u, &d, 8); printf(" %08lx%08lx", (unsigned long)(u >> 32), (unsigned long)u); }
static void hexs(const char *s) { printf("["); for (; *s; s++) printf("%02x", (unsigned char)*s); printf("]"); }

static void paths(void) {
    static const char *p[] = { "C:\\dir\\file.txt", "C:\\dir\\", "C:\\", "C:", "dir\\file", "file", "\\\\srv\\share\\a.b", "\\\\srv\\share",
                               "C:\\a.b\\c", "C:\\dir\\.hidden", "C:\\dir\\a.tar.gz", "\\rooted\\x", "..\\up", "C:\\\x83\x5c\\\x95\x5c.txt", "", "a/b/c.d" };
    for (unsigned i = 0; i < sizeof p / sizeof *p; i++) {
        char b[MAX_PATH];
        printf("path "); hexs(p[i]); printf("\n");
        printf("  name "); hexs(PathFindFileNameA(p[i])); printf(" ext "); hexs(PathFindExtensionA(p[i]));
        printf(" rel %d\n", PathIsRelativeA(p[i]));
        strcpy(b, p[i]); BOOL r = PathRemoveFileSpecA(b); printf("  removespec %d ", r); hexs(b); printf("\n");
        strcpy(b, p[i]); PathStripPathA(b); printf("  strip "); hexs(b); printf("\n");
        strcpy(b, p[i]); char *e = PathAddBackslashA(b); printf("  addbs %d ", e ? (int)(e - b) : -1); hexs(b); printf("\n");
        strcpy(b, p[i]); e = PathRemoveBackslashA(b); printf("  rmbs %d ", e ? (int)(e - b) : -1); hexs(b); printf("\n");
    }
    static const char *c[][2] = { { "C:\\dir", "file" }, { "C:\\dir\\", "\\file" }, { "C:\\dir", "..\\x" }, { "C:\\dir", "D:\\abs" }, { "C:\\dir", "" },
                                  { "", "file" }, { NULL, "file" }, { "C:\\dir", NULL }, { "C:\\a\\b", ".\\..\\..\\..\\c" }, { "rel", "x" } };
    for (unsigned i = 0; i < sizeof c / sizeof *c; i++) {
        char b[MAX_PATH] = "junk";
        char *r = PathCombineA(b, c[i][0], c[i][1]);
        printf("combine %s + %s -> %d ", c[i][0] ? c[i][0] : "(null)", c[i][1] ? c[i][1] : "(null)", r != NULL); hexs(b); printf("\n");
        strcpy(b, c[i][0] ? c[i][0] : "");
        BOOL ok = c[i][1] ? PathAppendA(b, c[i][1]) : 0;
        printf("append -> %d ", ok); hexs(b); printf("\n");
    }
    static const char *k[] = { "C:\\a\\.\\b\\..\\c", "C:\\..\\..", "\\a\\..\\..\\b", "a\\..", "C:\\a\\...\\b", "C:\\a\\.", "\\\\s\\h\\..\\x", "C:\\a\\\\b" };
    for (unsigned i = 0; i < sizeof k / sizeof *k; i++) {
        char b[MAX_PATH] = "junk";
        BOOL r = PathCanonicalizeA(b, k[i]);
        printf("canon %s -> %d ", k[i], r); hexs(b); printf("\n");
    }
}

static void variants(void) {
    BSTR s = SysAllocString(L"abc");
    printf("bstr len %u bytes %u\n", SysStringLen(s), SysStringByteLen(s));
    SysFreeString(s);
    s = SysAllocStringByteLen("abcde", 5);
    printf("bytelen len %u bytes %u\n", SysStringLen(s), SysStringByteLen(s));
    SysFreeString(s);
    printf("null len %u\n", SysStringLen(NULL));

    struct { VARTYPE vt; LONGLONG i; double d; const WCHAR *s; } in[] = {
        { VT_I4, 42 }, { VT_I4, -1 }, { VT_I4, 0x7fffffff }, { VT_R8, 0, 2.5 }, { VT_R8, 0, 3.5 }, { VT_R8, 0, -2.5 }, { VT_R8, 0, 1e10 },
        { VT_R8, 0, 0.1 }, { VT_R8, 0, 1.0 / 3 }, { VT_R8, 0, 45000.75 }, { VT_BOOL, -1 }, { VT_BOOL, 0 },
        { VT_BSTR, 0, 0, L"123" }, { VT_BSTR, 0, 0, L" 12 " }, { VT_BSTR, 0, 0, L"1,234.5" }, { VT_BSTR, 0, 0, L"0x10" }, { VT_BSTR, 0, 0, L"&H10" },
        { VT_BSTR, 0, 0, L"abc" }, { VT_BSTR, 0, 0, L"" }, { VT_BSTR, 0, 0, L"True" }, { VT_BSTR, 0, 0, L"-0" }, { VT_BSTR, 0, 0, L"1e400" },
        { VT_BSTR, 0, 0, L"2024/03/05" }, { VT_BSTR, 0, 0, L"\xff11\xff12" },
    };
    VARTYPE to[] = { VT_I2, VT_I4, VT_UI1, VT_R4, VT_R8, VT_BOOL, VT_BSTR, VT_DATE, VT_CY };
    for (unsigned i = 0; i < sizeof in / sizeof *in; i++) {
        VARIANT v; VariantInit(&v);
        v.vt = in[i].vt;
        if (v.vt == VT_I4) v.lVal = (LONG)in[i].i;
        else if (v.vt == VT_R8) v.dblVal = in[i].d;
        else if (v.vt == VT_BOOL) v.boolVal = (VARIANT_BOOL)in[i].i;
        else v.bstrVal = SysAllocString(in[i].s);
        printf("from vt %u #%u:", in[i].vt, i);
        for (unsigned j = 0; j < sizeof to / sizeof *to; j++) {
            VARIANT o; VariantInit(&o);
            HRESULT hr = VariantChangeTypeEx(&o, &v, 0x411, 0, to[j]);
            printf(" [%u %lx", to[j], (unsigned long)hr);
            if (hr == S_OK) {
                switch (o.vt) {
                case VT_I2: printf(" %d", o.iVal); break;
                case VT_I4: printf(" %ld", o.lVal); break;
                case VT_UI1: printf(" %u", o.bVal); break;
                case VT_R4: dbits(o.fltVal); break;
                case VT_R8: dbits(o.dblVal); break;
                case VT_DATE: dbits(o.date); break;
                case VT_BOOL: printf(" %d", o.boolVal); break;
                case VT_CY: printf(" %lld", (long long)o.cyVal.int64); break;
                case VT_BSTR: printf(" \""); for (WCHAR *p = o.bstrVal; *p; p++) printf(*p < 128 ? "%c" : "\\u%04x", *p); printf("\""); break;
                }
            }
            printf("]");
            VariantClear(&o);
        }
        printf("\n");
        VariantClear(&v);
    }
}

int main(void) {
    paths();
    variants();
    return 0;
}
