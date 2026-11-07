/* APIs that trial versions needed (2026-11): window properties set and read under names of
 * other cases and through the A and W calls, SHStrDupA/W, PathRenameExtensionA/W on paths with
 * and without extensions, dots in directory names and overflowing results, CharNextExA on
 * code page 932 lead bytes, and the msvcrt "C" locale's wcstombs/mbstowcs (counting, short
 * buffers, characters above 0xff), _chdir/_wchdir and _setmaxstdio.
 * libs: -lshlwapi -lole32 */
#include "../probe.h"
#include <shlwapi.h>
#include <direct.h>
#include <errno.h>

static void hexs(const char *s) { printf("["); for (; *s; s++) printf("%02x", (unsigned char)*s); printf("]"); }
static void hexw(const WCHAR *s) { printf("["); for (; *s; s++) printf("%04x", *s); printf("]"); }

static void props(void) {
    HWND h = CreateWindowExA(0, "STATIC", "p", 0, 0, 0, 10, 10, NULL, NULL, NULL, NULL);
    printf("props\n");
    printf("  set %d %d %d\n", SetPropA(h, "MixedCase", (HANDLE)0x11), SetPropW(h, L"WideKey", (HANDLE)0x22), SetPropA(h, "lower", (HANDLE)0x33));
    printf("  get %p %p %p\n", GetPropA(h, "MixedCase"), GetPropA(h, "mixedcase"), GetPropA(h, "MIXEDCASE"));
    printf("  wide %p %p %p\n", GetPropW(h, L"widekey"), GetPropA(h, "WIDEKEY"), GetPropW(h, L"MixedCase"));
    printf("  lower %p %p\n", GetPropA(h, "LOWER"), GetPropW(h, L"Lower"));
    printf("  replace %d %p\n", SetPropA(h, "MIXEDCASE", (HANDLE)0x44), GetPropA(h, "MixedCase"));
    printf("  remove %p %p %p\n", RemovePropA(h, "mixedCASE"), GetPropA(h, "MixedCase"), RemovePropW(h, L"WIDEKEY"));
    printf("  missing %p %p\n", GetPropA(h, "none"), RemovePropA(h, "none"));
    DestroyWindow(h);
}

static void strdup_(void) {
    static const char *a[] = { "abc", "", "\x93\xfa\x96\x7b\x8c\xea", "C:\\\x83\x5c\\x" };
    printf("SHStrDup\n");
    for (unsigned i = 0; i < sizeof a / sizeof *a; i++) {
        WCHAR *w = NULL;
        HRESULT hr = SHStrDupA(a[i], &w);
        printf("  A "); hexs(a[i]); printf(" %08lx ", hr); if (w) hexw(w); printf("\n");
        WCHAR *w2 = NULL;
        hr = SHStrDupW(w, &w2);
        printf("  W %08lx %d\n", hr, w && w2 && w2 != w && !wcscmp(w, w2));
        CoTaskMemFree(w); CoTaskMemFree(w2);
    }
    WCHAR *w = (WCHAR *)1;
    HRESULT hr = SHStrDupA(NULL, &w);
    printf("  null A %08lx %d\n", hr, w == NULL);
    w = (WCHAR *)1;
    hr = SHStrDupW(NULL, &w);
    printf("  null W %08lx %d\n", hr, w == NULL);
}

static void rename_ext(void) {
    static const char *p[][2] = { { "C:\\dir\\file.txt", ".bmp" }, { "file", ".x" }, { "C:\\a.b\\c", ".e" }, { "C:\\dir\\.hidden", ".y" },
                                  { "noext.", ".z" }, { "a.tar.gz", ".zip" }, { "file.txt", "txt" }, { "file.txt", "" }, { "", ".a" },
                                  { "C:\\\x83\x5c.txt", ".dat" }, { "dir.d\\", ".q" } };
    printf("PathRenameExtension\n");
    for (unsigned i = 0; i < sizeof p / sizeof *p; i++) {
        char b[MAX_PATH];
        strcpy(b, p[i][0]);
        BOOL r = PathRenameExtensionA(b, p[i][1]);
        printf("  "); hexs(p[i][0]); printf(" "); hexs(p[i][1]); printf(" %d ", r); hexs(b); printf("\n");
        WCHAR w[MAX_PATH], e[16];
        MultiByteToWideChar(932, 0, p[i][0], -1, w, MAX_PATH);
        MultiByteToWideChar(932, 0, p[i][1], -1, e, 16);
        r = PathRenameExtensionW(w, e);
        printf("    W %d ", r); hexw(w); printf("\n");
    }
    /* results that would pass MAX_PATH */
    char b[MAX_PATH];
    memset(b, 'a', MAX_PATH - 3); b[MAX_PATH - 3] = 0;
    b[MAX_PATH - 6] = '.';
    BOOL r = PathRenameExtensionA(b, ".longer");
    printf("  long %d %d %s\n", r, (int)strlen(b), b + MAX_PATH - 10);
    memset(b, 'a', MAX_PATH - 8); b[MAX_PATH - 8] = 0;
    r = PathRenameExtensionA(b, ".abc");
    printf("  fits %d %d\n", r, (int)strlen(b));
}

static void charnext(void) {
    static const char *s[] = { "a", "\x82\xa0", "\x82", "", "\x82\xa0\x82\xa1", "\x81\x40x", "\xa0\xa0", "\xe0\x40" };
    printf("CharNextExA\n");
    for (unsigned i = 0; i < sizeof s / sizeof *s; i++) {
        printf("  "); hexs(s[i]);
        printf(" 932 %d acp %d 1252 %d prev %d\n", (int)(CharNextExA(932, s[i], 0) - s[i]), (int)(CharNextExA(CP_ACP, s[i], 0) - s[i]),
               (int)(CharNextExA(1252, s[i], 0) - s[i]), (int)(CharPrevExA(932, s[i], s[i] + strlen(s[i]), 0) - s[i]));
    }
}

static void conv(void) {
    printf("wcstombs\n");
    static const WCHAR *w[] = { L"abc", L"", L"caf\xe9", L"\x3042", L"a\x0100" };
    for (unsigned i = 0; i < sizeof w / sizeof *w; i++) {
        char b[8];
        memset(b, 'x', sizeof b);
        errno = 0;
        size_t n = wcstombs(NULL, w[i], 0);
        int e1 = errno;
        errno = 0;
        size_t m = wcstombs(b, w[i], sizeof b);
        printf("  "); hexw(w[i]); printf(" count %d errno %d into %d errno %d", (int)n, e1, (int)m, errno);
        printf(" ["); for (int k = 0; k < 6; k++) printf("%02x", (unsigned char)b[k]); printf("]\n");
    }
    char b[8];
    memset(b, 'x', sizeof b);
    int sn = (int)wcstombs(b, L"abcdef", 3);
    printf("  short %d", sn);
    printf(" ["); for (int k = 0; k < 5; k++) printf("%02x", (unsigned char)b[k]); printf("]\n");
    printf("mbstowcs\n");
    static const char *a[] = { "abc", "", "\xe9z", "\x82\xa0" };
    for (unsigned i = 0; i < sizeof a / sizeof *a; i++) {
        WCHAR o[8];
        for (int k = 0; k < 8; k++) o[k] = 0x7878;
        errno = 0;
        size_t n = mbstowcs(NULL, a[i], 0);
        size_t m = mbstowcs(o, a[i], 8);
        printf("  "); hexs(a[i]); printf(" count %d into %d errno %d [", (int)n, (int)m, errno);
        for (int k = 0; k < 4; k++) printf("%04x", o[k]);
        printf("]\n");
    }
    WCHAR o[8];
    for (int k = 0; k < 8; k++) o[k] = 0x7878;
    int sm = (int)mbstowcs(o, "abcdef", 2);
    printf("  short %d [%04x%04x%04x]\n", sm, o[0], o[1], o[2]);
}

static void dirs(void) {
    char cwd[MAX_PATH];
    GetCurrentDirectoryA(MAX_PATH, cwd);
    errno = 0;
    int r = _chdir("no_such_dir_here");
    printf("chdir missing %d errno %d\n", r, errno);
    errno = 0;
    r = _wchdir(L"no_such_dir_here");
    printf("wchdir missing %d errno %d\n", r, errno);
    CreateDirectoryA("winref_chdir", NULL);
    r = _chdir("winref_chdir");
    char now[MAX_PATH];
    GetCurrentDirectoryA(MAX_PATH, now);
    printf("chdir %d moved %d\n", r, !strcmp(now + strlen(now) - 12, "winref_chdir"));
    r = _wchdir(L"..");
    GetCurrentDirectoryA(MAX_PATH, now);
    printf("wchdir up %d back %d\n", r, !strcmp(now, cwd));
    RemoveDirectoryA("winref_chdir");
    printf("setmaxstdio %d %d %d %d %d\n", _setmaxstdio(512), _setmaxstdio(19), _setmaxstdio(20), _setmaxstdio(2048), _setmaxstdio(2049));
}

int main(void) {
    props();
    strdup_();
    rename_ext();
    charnext();
    conv();
    dirs();
    return 0;
}
