/* advapi32 registry as games use it for settings and install paths: create dispositions, every
 * value type, the sizes and error codes of queries (ERROR_MORE_DATA and the size it asks for,
 * strings stored without their NUL, A vs W conversions in code page 932), the default value,
 * enumeration orders, RegQueryInfoKey's counts and maxima, deleting keys with subkeys, case
 * insensitivity, predefined keys. Under HKCU\Software\ippai-winref, removed first and last. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-70s = %lld\n", #expr, r_); } while (0)
#define BASE "Software\\ippai-winref"

static void hexa(const BYTE *a, DWORD n) { for (DWORD i = 0; i < n && i < 48; i++) printf(" %02x", a[i]); }

static void query(HKEY k, const char *name, DWORD bufsize) {
    BYTE buf[256];
    memset(buf, 0xcc, sizeof buf);
    DWORD type = 0xdead, size = bufsize;
    LONG r = RegQueryValueExA(k, name, NULL, &type, bufsize == 999 ? NULL : buf, &size);
    printf("query %s (%lu): %ld type %lu size %lu:", name ? name : "(default)", bufsize, r, type, size);
    if (r == 0 && bufsize != 999) hexa(buf, size + 1 < bufsize ? size + 1 : size);
    printf("\n");
}

static void queryw(HKEY k, const WCHAR *name, DWORD bufsize) {
    BYTE buf[256];
    memset(buf, 0xcc, sizeof buf);
    DWORD type = 0xdead, size = bufsize;
    LONG r = RegQueryValueExW(k, name, NULL, &type, buf, &size);
    printf("queryw (%lu): %ld type %lu size %lu:", bufsize, r, type, size);
    if (r == 0) hexa(buf, size + 2 < bufsize ? size + 2 : size);
    printf("\n");
}

int main(void) {
    HKEY k, sub, sub2;
    DWORD disp = 0xdead;
    RegDeleteTreeA(HKEY_CURRENT_USER, BASE);
    E(RegOpenKeyExA(HKEY_CURRENT_USER, BASE, 0, KEY_ALL_ACCESS, &k));
    E(RegCreateKeyExA(HKEY_CURRENT_USER, BASE, 0, NULL, 0, KEY_ALL_ACCESS, NULL, &k, &disp)); printf("disp %lu\n", disp);
    RegCloseKey(k);
    E(RegCreateKeyExA(HKEY_CURRENT_USER, BASE, 0, NULL, 0, KEY_ALL_ACCESS, NULL, &k, &disp)); printf("disp %lu\n", disp);

    /* values of every type */
    DWORD dw = 0x12345678;
    ULONGLONG qw = 0x1122334455667788ULL;
    E(RegSetValueExA(k, "dword", 0, REG_DWORD, (BYTE *)&dw, 4));
    E(RegSetValueExA(k, "qword", 0, REG_QWORD, (BYTE *)&qw, 8));
    E(RegSetValueExA(k, "sz", 0, REG_SZ, (BYTE *)"hello", 6));
    E(RegSetValueExA(k, "sz-nonul", 0, REG_SZ, (BYTE *)"abc", 3));
    E(RegSetValueExA(k, "sz-empty", 0, REG_SZ, (BYTE *)"", 0));
    E(RegSetValueExA(k, "sjis", 0, REG_SZ, (BYTE *)"\x82\xa0\x8a\xbf", 5));
    E(RegSetValueExA(k, "expand", 0, REG_EXPAND_SZ, (BYTE *)"%WINDIR%\\x", 11));
    E(RegSetValueExA(k, "multi", 0, REG_MULTI_SZ, (BYTE *)"a\0bc\0\0", 6));
    E(RegSetValueExA(k, "bin", 0, REG_BINARY, (BYTE *)"\x01\x02\x00\x03", 4));
    E(RegSetValueExA(k, "bin-empty", 0, REG_BINARY, NULL, 0));
    E(RegSetValueExA(k, "dword-short", 0, REG_DWORD, (BYTE *)&dw, 2));
    E(RegSetValueExA(k, "odd-type", 0, 0x1234, (BYTE *)"zz", 2));
    E(RegSetValueExA(k, NULL, 0, REG_SZ, (BYTE *)"default", 8));
    E(RegSetValueExA(k, "MiXeD", 0, REG_DWORD, (BYTE *)&dw, 4));
    E(RegSetValueExA(k, "mixed", 0, REG_SZ, (BYTE *)"replaced", 9));

    const char *names[] = { "dword", "qword", "sz", "sz-nonul", "sz-empty", "sjis", "expand", "multi", "bin", "bin-empty", "dword-short", "odd-type", NULL, "", "MIXED", "missing" };
    for (unsigned i = 0; i < sizeof names / sizeof *names; i++) {
        query(k, names[i], 256);
        query(k, names[i], 2);
        query(k, names[i], 999); /* size only */
    }
    WCHAR wn[][12] = { L"sz", L"sz-nonul", L"sjis", L"multi", L"dword" };
    for (int i = 0; i < 5; i++) { queryw(k, wn[i], 256); queryw(k, wn[i], 3); }
    WCHAR wv[] = L"\x3042\x6f22";
    E(RegSetValueExW(k, L"wide", 0, REG_SZ, (BYTE *)wv, sizeof wv));
    query(k, "wide", 256);
    query(k, "wide", 3);

    /* RegGetValue's type filters and NUL handling */
    char gb[64]; DWORD gs = sizeof gb, gt = 0;
    E(RegGetValueA(k, NULL, "sz-nonul", RRF_RT_REG_SZ, &gt, gb, &gs)); printf("get %lu %lu %s\n", gt, gs, gb);
    gs = sizeof gb;
    E(RegGetValueA(k, NULL, "dword", RRF_RT_REG_SZ, &gt, gb, &gs));
    gs = sizeof gb;
    E(RegGetValueA(k, NULL, "expand", RRF_RT_REG_SZ | RRF_NOEXPAND, &gt, gb, &gs));

    /* subkeys, enumeration */
    const char *subs[] = { "zeta", "Alpha", "beta", "\x83\x65\x83\x58\x83\x67", "beta\\deep" };
    for (int i = 0; i < 5; i++) { E(RegCreateKeyExA(k, subs[i], 0, NULL, 0, KEY_ALL_ACCESS, NULL, &sub, &disp)); printf("disp %lu\n", disp); RegCloseKey(sub); }
    E(RegCreateKeyExA(k, "BETA", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &sub, &disp)); printf("disp %lu\n", disp);
    RegCloseKey(sub);
    for (DWORD i = 0;; i++) {
        char nm[64]; DWORD nl = sizeof nm;
        LONG r = RegEnumKeyExA(k, i, nm, &nl, NULL, NULL, NULL, NULL);
        printf("key %lu: %ld", i, r);
        if (r) { printf("\n"); break; }
        printf(" %s len %lu\n", nm, nl);
    }
    char small[3]; DWORD sl = sizeof small;
    E(RegEnumKeyExA(k, 0, small, &sl, NULL, NULL, NULL, NULL)); printf("len %lu\n", sl);
    for (DWORD i = 0;; i++) {
        char nm[64]; BYTE data[64]; DWORD nl = sizeof nm, dl = sizeof data, ty = 0;
        LONG r = RegEnumValueA(k, i, nm, &nl, NULL, &ty, data, &dl);
        printf("value %lu: %ld", i, r);
        if (r) { printf("\n"); break; }
        printf(" [%s] len %lu type %lu size %lu\n", nm, nl, ty, dl);
    }
    char vn[2]; DWORD vl = sizeof vn;
    E(RegEnumValueA(k, 0, vn, &vl, NULL, NULL, NULL, NULL)); printf("len %lu\n", vl);
    DWORD nsub, maxsub, maxcls, nval, maxvn, maxvd;
    E(RegQueryInfoKeyA(k, NULL, NULL, NULL, &nsub, &maxsub, &maxcls, &nval, &maxvn, &maxvd, NULL, NULL));
    printf("info %lu %lu %lu %lu %lu %lu\n", nsub, maxsub, maxcls, nval, maxvn, maxvd);

    /* deleting */
    E(RegDeleteKeyA(k, "beta"));
    E(RegDeleteKeyA(k, "beta\\deep"));
    E(RegDeleteKeyA(k, "beta"));
    E(RegDeleteKeyA(k, "beta"));
    E(RegDeleteValueA(k, "dword"));
    E(RegDeleteValueA(k, "dword"));
    E(RegDeleteValueA(k, NULL));
    query(k, NULL, 256);
    E(RegOpenKeyExA(k, "ALPHA", 0, KEY_READ, &sub2));
    RegCloseKey(sub2);
    E(RegOpenKeyExA(k, "alpha\\", 0, KEY_READ, &sub2));
    E(RegOpenKeyExA(k, "\\alpha", 0, KEY_READ, &sub2));
    E(RegOpenKeyExA(k, "", 0, KEY_READ, &sub2));
    E(RegOpenKeyExA(k, NULL, 0, KEY_READ, &sub2));

    /* the old API: RegSetValue/RegQueryValue write a subkey's default value */
    E(RegSetValueA(k, "old", REG_SZ, "olddata", 7));
    char ob[32]; LONG ol = sizeof ob;
    E(RegQueryValueA(k, "old", ob, &ol)); printf("old [%s] %ld\n", ob, ol);
    ol = 2;
    E(RegQueryValueA(k, "old", ob, &ol)); printf("old size %ld\n", ol);
    E(RegQueryValueA(k, "nothere", ob, &ol));

    /* handles and predefined keys */
    E(RegCloseKey(k));
    E(RegCloseKey(k));
    E(RegCloseKey(HKEY_CURRENT_USER));
    E(RegQueryValueExA((HKEY)(ULONG_PTR)0x1234, "x", NULL, NULL, NULL, NULL));
    E(RegOpenKeyExA(HKEY_LOCAL_MACHINE, "Software", 0, KEY_READ, &sub2));
    E(RegDeleteTreeA(HKEY_CURRENT_USER, BASE));
    E(RegOpenKeyExA(HKEY_CURRENT_USER, BASE, 0, KEY_READ, &sub2));
    return 0;
}
