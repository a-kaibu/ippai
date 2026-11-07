/* VerifyVersionInfoA/W and VerSetConditionMask (libcurl, Unity 6's player and many installers
 * check the Windows version with them), asked about the version GetVersionEx reports and its
 * neighbours (one more or less in each field), so that the answers do not depend on which
 * Windows runs the probe: every condition on each field alone, the major/minor/service-pack
 * chain where the first field decides, build, platform, product type, suite masks with AND
 * and OR, and bad arguments (no types, no conditions, a condition for another field).
 * Also the SSPI function tables of InitSecurityInterfaceA/W (libcurl's Schannel backend calls
 * through them): which slots are filled.
 * libs: -lsecur32 */
#include "../probe.h"
#define SECURITY_WIN32
#include <security.h>
#include <sspi.h>

static OSVERSIONINFOEXW cur;

/* "1" or "0 <last error>" */
static void show(const char *what, BOOL r) {
    if (r) printf("%s: 1\n", what);
    else printf("%s: 0 %lu\n", what, GetLastError());
}

static BOOL verify(OSVERSIONINFOEXW *v, DWORD types, ULONGLONG mask, BOOL wide) {
    SetLastError(0xdeadbeef);
    if (wide) return VerifyVersionInfoW(v, types, mask);
    OSVERSIONINFOEXA a;
    memset(&a, 0, sizeof a);
    a.dwOSVersionInfoSize = sizeof a;
    a.dwMajorVersion = v->dwMajorVersion;
    a.dwMinorVersion = v->dwMinorVersion;
    a.dwBuildNumber = v->dwBuildNumber;
    a.dwPlatformId = v->dwPlatformId;
    a.wServicePackMajor = v->wServicePackMajor;
    a.wServicePackMinor = v->wServicePackMinor;
    a.wSuiteMask = v->wSuiteMask;
    a.wProductType = v->wProductType;
    return VerifyVersionInfoA(&a, types, mask);
}

static const char *cname[] = { "0", "EQUAL", "GREATER", "GREATER_EQUAL", "LESS", "LESS_EQUAL", "AND", "OR" };

int main(void) {
    cur.dwOSVersionInfoSize = sizeof cur;
    GetVersionExW((OSVERSIONINFOW *)&cur);

    /* VerSetConditionMask: which slot each type's condition goes to */
    DWORD types[] = { VER_MINORVERSION, VER_MAJORVERSION, VER_BUILDNUMBER, VER_PLATFORMID, VER_SERVICEPACKMINOR,
                      VER_SERVICEPACKMAJOR, VER_SUITENAME, VER_PRODUCT_TYPE, VER_MAJORVERSION | VER_MINORVERSION,
                      VER_MINORVERSION | VER_PRODUCT_TYPE, 0, 0x100 };
    for (int t = 0; t < (int)(sizeof types / sizeof *types); t++)
        for (int c = 0; c <= 8; c++) {
            ULONGLONG m = VerSetConditionMask(0, types[t], c);
            ULONGLONG m2 = VerSetConditionMask(0x123456789abcULL, types[t], c);
            printf("mask %#lx %d: %08lx%08lx %08lx%08lx\n", types[t], c, (unsigned long)(m >> 32), (unsigned long)m,
                   (unsigned long)(m2 >> 32), (unsigned long)m2);
        }

    for (int wide = 0; wide < 2; wide++) {
        const char *w = wide ? "W" : "A";
        char what[160];
        /* each numeric field alone, at the current value and one off */
        struct { DWORD type; const char *name; } one[] = {
            { VER_MAJORVERSION, "major" }, { VER_MINORVERSION, "minor" }, { VER_BUILDNUMBER, "build" },
            { VER_PLATFORMID, "platform" }, { VER_SERVICEPACKMAJOR, "spmajor" }, { VER_SERVICEPACKMINOR, "spminor" },
            { VER_PRODUCT_TYPE, "product" },
        };
        for (int f = 0; f < 7; f++)
            for (int d = -1; d <= 1; d++)
                for (int c = 1; c <= 5; c++) {
                    OSVERSIONINFOEXW v = cur;
                    switch (one[f].type) {
                    case VER_MAJORVERSION: v.dwMajorVersion += d; break;
                    case VER_MINORVERSION: v.dwMinorVersion += d; break;
                    case VER_BUILDNUMBER: v.dwBuildNumber += d; break;
                    case VER_PLATFORMID: v.dwPlatformId += d; break;
                    case VER_SERVICEPACKMAJOR: v.wServicePackMajor += d; break;
                    case VER_SERVICEPACKMINOR: v.wServicePackMinor += d; break;
                    case VER_PRODUCT_TYPE: v.wProductType += d; break;
                    }
                    snprintf(what, sizeof what, "%s %s%+d %s", w, one[f].name, d, cname[c]);
                    show(what, verify(&v, one[f].type, VerSetConditionMask(0, one[f].type, c), wide));
                }
        /* the chain: major, minor, service pack major, minor compare as one number */
        DWORD chain[] = { VER_MAJORVERSION | VER_MINORVERSION, VER_MAJORVERSION | VER_MINORVERSION | VER_SERVICEPACKMAJOR,
                          VER_MAJORVERSION | VER_SERVICEPACKMAJOR, VER_MINORVERSION | VER_SERVICEPACKMAJOR | VER_SERVICEPACKMINOR };
        for (int k = 0; k < 4; k++)
            for (int dmaj = -1; dmaj <= 1; dmaj++)
                for (int dmin = -1; dmin <= 1; dmin++)
                    for (int c1 = 1; c1 <= 5; c1++)
                        for (int c2 = 0; c2 <= 5; c2 += 1) {
                            OSVERSIONINFOEXW v = cur;
                            v.dwMajorVersion += dmaj;
                            v.dwMinorVersion += dmin;
                            v.wServicePackMajor += dmin;
                            v.wServicePackMinor += dmaj;
                            ULONGLONG m = 0;
                            /* the first field's condition, then the others' */
                            DWORD t = chain[k], first = t & VER_MAJORVERSION ? VER_MAJORVERSION : VER_MINORVERSION;
                            m = VerSetConditionMask(m, first, c1);
                            for (DWORD b = 1; b <= VER_SERVICEPACKMAJOR; b <<= 1)
                                if (t & b && b != first) m = VerSetConditionMask(m, b, c2);
                            snprintf(what, sizeof what, "%s chain %#lx maj%+d min/sp%+d %s %s", w, t, dmaj, dmin, cname[c1], cname[c2]);
                            show(what, verify(&v, t, m, wide));
                        }
        /* the check programs make: at least 6.1 / 6.2 / 10.0 (IsWindows7OrGreater ...) */
        int at[][2] = { { 5, 1 }, { 6, 0 }, { 6, 1 }, { 6, 2 }, { 6, 3 }, { 10, 0 } };
        for (int k = 0; k < 6; k++) {
            OSVERSIONINFOEXW v;
            memset(&v, 0, sizeof v);
            v.dwOSVersionInfoSize = sizeof v;
            v.dwMajorVersion = at[k][0];
            v.dwMinorVersion = at[k][1];
            ULONGLONG m = VerSetConditionMask(VerSetConditionMask(VerSetConditionMask(0, VER_MAJORVERSION, VER_GREATER_EQUAL), VER_MINORVERSION,
                                                                  VER_GREATER_EQUAL), VER_SERVICEPACKMAJOR, VER_GREATER_EQUAL);
            BOOL r = verify(&v, VER_MAJORVERSION | VER_MINORVERSION | VER_SERVICEPACKMAJOR, m, wide);
            BOOL want = cur.dwMajorVersion > (DWORD)at[k][0] || (cur.dwMajorVersion == (DWORD)at[k][0] && cur.dwMinorVersion >= (DWORD)at[k][1]);
            printf("%s at least %d.%d: %s\n", w, at[k][0], at[k][1], r == want ? "as GetVersionEx" : "not as GetVersionEx");
        }
        /* suites */
        WORD suites[] = { 0, cur.wSuiteMask, VER_SUITE_SMALLBUSINESS, VER_SUITE_ENTERPRISE, VER_SUITE_SINGLEUSERTS, (WORD)(cur.wSuiteMask | 0x8000) };
        for (int s = 0; s < 6; s++)
            for (int c = 0; c <= 8; c++) {
                OSVERSIONINFOEXW v = cur;
                v.wSuiteMask = suites[s];
                snprintf(what, sizeof what, "%s suite %d %s", w, s, c < 8 ? cname[c] : "8");
                show(what, verify(&v, VER_SUITENAME, VerSetConditionMask(0, VER_SUITENAME, c), wide));
            }
        /* bad arguments */
        OSVERSIONINFOEXW v = cur;
        snprintf(what, sizeof what, "%s no types", w);
        show(what, verify(&v, 0, VerSetConditionMask(0, VER_MAJORVERSION, VER_EQUAL), wide));
        snprintf(what, sizeof what, "%s no mask", w);
        show(what, verify(&v, VER_MAJORVERSION, 0, wide));
        snprintf(what, sizeof what, "%s mask of another field", w);
        show(what, verify(&v, VER_MAJORVERSION, VerSetConditionMask(0, VER_BUILDNUMBER, VER_EQUAL), wide));
        snprintf(what, sizeof what, "%s condition 0 for minor", w);
        show(what, verify(&v, VER_MAJORVERSION | VER_MINORVERSION, VerSetConditionMask(0, VER_MAJORVERSION, VER_EQUAL), wide));
        v.dwMinorVersion++;
        snprintf(what, sizeof what, "%s condition 0 for minor, minor+1", w);
        show(what, verify(&v, VER_MAJORVERSION | VER_MINORVERSION, VerSetConditionMask(0, VER_MAJORVERSION, VER_EQUAL), wide));
    }

    /* SSPI tables */
    PSecurityFunctionTableA ta = InitSecurityInterfaceA();
    PSecurityFunctionTableW tw = InitSecurityInterfaceW();
    printf("InitSecurityInterfaceA: %s, again same %d\n", ta ? "table" : "NULL", ta == InitSecurityInterfaceA());
    printf("InitSecurityInterfaceW: %s, again same %d\n", tw ? "table" : "NULL", tw == InitSecurityInterfaceW());
    if (ta && tw) {
        printf("versions %lu %lu\n", ta->dwVersion, tw->dwVersion);
        /* the slots of SECURITY_SUPPORT_PROVIDER_INTERFACE_VERSION 1 (up to DecryptMessage) */
        void **a = (void **)&ta->EnumerateSecurityPackagesA, **b = (void **)&tw->EnumerateSecurityPackagesW;
        for (int k = 0; k < 26; k++) printf("slot %d: %s %s\n", k, a[k] ? "set" : "NULL", b[k] ? "set" : "NULL");
        /* the A and W tables share the functions without strings */
        printf("shared FreeCredentialsHandle %d FreeContextBuffer %d EncryptMessage %d\n", ta->FreeCredentialsHandle == tw->FreeCredentialsHandle,
               ta->FreeContextBuffer == tw->FreeContextBuffer, (void *)ta->EncryptMessage == (void *)tw->EncryptMessage);
        /* the entries are the exported functions */
        HMODULE s = GetModuleHandleA("secur32.dll");
        printf("FreeContextBuffer exported %d\n", s && (void *)GetProcAddress(s, "FreeContextBuffer") == (void *)tw->FreeContextBuffer);
    }
    return 0;
}
