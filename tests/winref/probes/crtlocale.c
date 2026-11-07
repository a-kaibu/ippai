/* _create_locale's _locale_t as the CRT's inline code reads it (#58): { locinfo, mbcinfo },
 * and where in each the code page, the ctype table and the case maps are, for msvcrt.dll (its
 * threadlocinfo) and ucrtbase.dll (__crt_locale_data, whose public prefix comes first). The
 * user's locale (""): its code page is GetACP() on any machine, and 'A' / 'a' have the same
 * classes and cases in any ANSI code page, so the offsets printed hold everywhere. */
#include "../probe.h"
#include <locale.h>

typedef void *(__cdecl *create_fn)(int, const char *);
typedef void(__cdecl *free_fn)(void *);

static int readable(const void *p, size_t n) {
    MEMORY_BASIC_INFORMATION m;
    if (!p || !VirtualQuery(p, &m, sizeof m) || m.State != MEM_COMMIT || (m.Protect & (PAGE_NOACCESS | PAGE_GUARD))) return 0;
    return (const char *)p + n <= (const char *)m.BaseAddress + m.RegionSize;
}

/* (offsets within the first n bytes of s: DWORDs equal to v; pointers to a ctype table (pctype
 * 'A' and 'a' classes); pointers to a lower / upper case map) */
static void scan(const char *dll, const char *what, const unsigned char *s, int n) {
    printf("%s %s acp at", dll, what);
    for (int o = 0; o + 4 <= n; o += 4)
        if (*(const DWORD *)(s + o) == GetACP()) printf(" %#x", o);
    printf("\n%s %s pctype at", dll, what);
    for (int o = 0; o + (int)sizeof(void *) <= n; o += sizeof(void *)) {
        const unsigned short *t = *(const unsigned short *const *)(s + o);
        if (readable(t, 512) && t['A'] == 0x181 && t['a'] == 0x182 && t['0'] == 0x84) printf(" %#x", o);
    }
    printf("\n%s %s lower map at", dll, what);
    for (int o = 0; o + (int)sizeof(void *) <= n; o += sizeof(void *)) {
        const unsigned char *t = *(const unsigned char *const *)(s + o);
        if (readable(t, 256) && t['A'] == 'a' && t['a'] == 'a' && t['Z'] == 'z') printf(" %#x", o);
    }
    printf("\n%s %s upper map at", dll, what);
    for (int o = 0; o + (int)sizeof(void *) <= n; o += sizeof(void *)) {
        const unsigned char *t = *(const unsigned char *const *)(s + o);
        if (readable(t, 256) && t['a'] == 'A' && t['A'] == 'A' && t['z'] == 'Z') printf(" %#x", o);
    }
    printf("\n");
}

int main(void) {
    static const char *dlls[] = { "msvcrt.dll", "ucrtbase.dll" };
    for (int i = 0; i < 2; i++) {
        HMODULE m = LoadLibraryA(dlls[i]);
        create_fn create = m ? (create_fn)GetProcAddress(m, "_create_locale") : NULL;
        free_fn release = m ? (free_fn)GetProcAddress(m, "_free_locale") : NULL;
        if (!create) { printf("%s no _create_locale\n", dlls[i]); continue; }
        void **loc = create(LC_ALL, "");
        printf("%s _create_locale %s\n", dlls[i], loc ? "ok" : "NULL");
        if (!loc) continue;
        scan(dlls[i], "locinfo", loc[0], 0x180);
        /* (mbcinfo: the code page; mbctype: lead bytes, 'A' is _MUP 0x10 on any code page) */
        const unsigned char *mb = loc[1];
        printf("%s mbcinfo acp at", dlls[i]);
        for (int o = 0; o + 4 <= 0x20; o += 4)
            if (*(const DWORD *)(mb + o) == GetACP()) printf(" %#x", o);
        printf("\n%s mbcinfo mbctype at", dlls[i]);
        for (int o = 0x10; o <= 0x20; o += 4)
            if ((mb[o + 1 + 'A'] & 0x30) == 0x10 && (mb[o + 1 + 'a'] & 0x30) == 0x20) printf(" %#x", o);
        printf("\n");
        if (release) release(loc);
    }
    return 0;
}
