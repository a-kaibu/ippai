/* Small kernel32/user32/version calls the corpus found unimplemented or wrong: wvsprintf (the
 * va_list forms, 3be02ac), VerLanguageName, FindFirstFileEx (its find data is the third
 * argument: x64 passes it in R8), the preferred UI language lists (ec607c4), QueryDosDevice and
 * GetLogicalDriveStrings (fabe81d), LoadCursorFromFile (4d61256). Runs in a scratch folder.
 * libs: -lgdi32 */
#include "../probe.h"

static int wv(char *out, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = wvsprintfA(out, fmt, ap);
    va_end(ap);
    return n;
}
static int wvw(WCHAR *out, const WCHAR *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = wvsprintfW(out, fmt, ap);
    va_end(ap);
    return n;
}

static void find_ex(const char *pat, FINDEX_INFO_LEVELS level, FINDEX_SEARCH_OPS op, DWORD flags) {
    WIN32_FIND_DATAA fd;
    memset(&fd, 0xcd, sizeof fd);
    SetLastError(0xdead);
    HANDLE h = FindFirstFileExA(pat, level, &fd, op, NULL, flags);
    if (h == INVALID_HANDLE_VALUE) {
        printf("FindFirstFileExA %s level %d op %d flags %lx: invalid err %lu\n", pat, level, op, flags, GetLastError());
        return;
    }
    printf("FindFirstFileExA %s level %d op %d flags %lx:", pat, level, op, flags);
    int n = 0;
    do {
        printf(" %s%s(%s)", fd.cFileName, fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY ? "/" : "", (BYTE)fd.cAlternateFileName[0] == 0xcd ? "untouched" : fd.cAlternateFileName[0] ? "short" : "empty");
        memset(&fd, 0xcd, sizeof fd);
    } while (++n < 20 && FindNextFileA(h, &fd));
    printf(" next err %lu\n", GetLastError());
    FindClose(h);
}

int main(void) {
    char a[1100];
    int n = wv(a, "%d|%5d|%-5d|%05d|%u|%x|%X|%08lx|%s|%c|%%|%ld|%i", -42, 42, 42, 42, 0xffffffffu, 255, 255, 0xabcdefUL, "str", 'z', 123456789L, -1);
    printf("wvsprintfA %d [%s]\n", n, a);
    n = wv(a, "[%.3s][%10s][%-10s]", "abcdef", "right", "left");
    printf("wvsprintfA %d %s\n", n, a);
    n = wv(a, "%ls|%S|%hs|%hc|%lc", L"wide", L"WIDE", "narrow", 'n', L'w');
    printf("wvsprintfA %d %s\n", n, a);
    /* %p, %f (not supported by wsprintf), %I64d */
    n = wv(a, "%f|%e|%I64d", 1.5, 2.0, (long long)1 << 40);
    printf("wvsprintfA unsupported %d %s\n", n, a);
    /* 1024 characters at most */
    char big[2000];
    memset(big, 'q', 1500);
    big[1500] = 0;
    memset(a, 'x', sizeof a);
    n = wv(a, "%s", big);
    printf("wvsprintfA long: %d strlen %d after %d\n", n, (int)strlen(a), (unsigned char)a[1025]);
    WCHAR w[1100];
    n = wvw(w, L"%d|%s|%S|%c|%x|%ls|%hs", 7, L"wide", "narrow", L'c', 0xbeef, L"l", "h");
    printf("wvsprintfW %d %ls\n", n, w);
    n = wsprintfA(a, "%s=%d", "k", 5);
    printf("wsprintfA %d %s\n", n, a);

    /* VerLanguageName: the name depends on the UI language, so only the lengths' rules */
    char vn[256];
    memset(vn, 'x', sizeof vn);
    DWORD vl = VerLanguageNameA(0x411, vn, sizeof vn);
    printf("VerLanguageNameA 0411: %d terminated %d\n", vl > 0, vl > 0 && vl < sizeof vn && vn[vl] == 0 && strlen(vn) == vl);
    memset(vn, 'x', sizeof vn);
    DWORD vs = VerLanguageNameA(0x411, vn, 4);
    printf("VerLanguageNameA 0411 4 chars: ret==full %d strlen %d after %d\n", vs == vl, (int)strnlen(vn, 8), vn[4] == 'x');
    DWORD vnull = VerLanguageNameA(0x411, NULL, 0);
    printf("VerLanguageNameA 0411 NULL buffer: ret==full %d\n", vnull == vl);
    printf("VerLanguageNameA 0411 returns: 4 chars full%+d, NULL buffer full%+d\n", (int)(vs - vl), (int)(vnull - vl));
    WCHAR vw[256];
    DWORD wl = VerLanguageNameW(0x409, vw, 256);
    printf("VerLanguageNameW 0409: %d terminated %d\n", wl > 0, wl > 0 && wcslen(vw) == wl);
    vl = VerLanguageNameA(0x9999, vn, sizeof vn);
    printf("VerLanguageNameA 9999: %d\n", vl > 0);

    /* the preferred UI language lists */
    BOOL (WINAPI *pref)(DWORD, PULONG, PZZWSTR, PULONG);
    const char *names[] = { "GetThreadPreferredUILanguages", "GetUserPreferredUILanguages", "GetProcessPreferredUILanguages", "GetSystemPreferredUILanguages" };
    for (int i = 0; i < 4; i++) {
        pref = (void *)GetProcAddress(GetModuleHandleA("kernel32.dll"), names[i]);
        if (!pref) {
            printf("%s missing\n", names[i]);
            continue;
        }
        ULONG count = 0xdead, size = 0;
        SetLastError(0xdead);
        BOOL r = pref(MUI_LANGUAGE_NAME, &count, NULL, &size);
        printf("%s size query: ret %d count>0 %d size>2 %d", names[i], r != 0, r && count > 0, size > 2);
        WCHAR list[256];
        ULONG small = 1;
        SetLastError(0xdead);
        r = pref(MUI_LANGUAGE_NAME, &count, list, &small);
        printf(" short buffer ret %d err %lu", r != 0, r ? 0 : GetLastError());
        ULONG full = 256;
        r = pref(MUI_LANGUAGE_NAME, &count, list, &full);
        int ok = r && full == size && list[full - 1] == 0 && list[full - 2] == 0;
        printf(" list ret %d double-terminated %d\n", r != 0, ok);
        ULONG idsize = 0;
        r = pref(MUI_LANGUAGE_ID, &count, NULL, &idsize);
        printf("%s ids: ret %d size %s\n", names[i], r != 0, idsize == count * 5 + 1 ? "count*5+1" : "other");
    }

    /* QueryDosDevice / GetLogicalDriveStrings */
    char dd[4096];
    DWORD dn = QueryDosDeviceA("C:", dd, sizeof dd);
    printf("QueryDosDevice C: %d device %d double-terminated %d\n", dn > 0, strncmp(dd, "\\Device\\", 8) == 0, dn >= 2 && dd[dn - 1] == 0 && dd[dn - 2] == 0);
    SetLastError(0xdead);
    dn = QueryDosDeviceA("C:", dd, 3);
    printf("QueryDosDevice C: 3 chars: %lu err %lu\n", dn, GetLastError());
    SetLastError(0xdead);
    dn = QueryDosDeviceA("NOSUCHDEV", dd, sizeof dd);
    printf("QueryDosDevice NOSUCHDEV: %lu err %lu\n", dn, GetLastError());
    dn = QueryDosDeviceA(NULL, dd, sizeof dd);
    printf("QueryDosDevice all: %d\n", dn > 100);
    dn = GetLogicalDriveStringsA(0, NULL);
    DWORD d2 = GetLogicalDriveStringsA(sizeof dd, dd);
    printf("GetLogicalDriveStrings: need==got+1 %d starts C %d\n", dn == d2 + 1, strncmp(dd, "C:\\", 3) == 0);
    WCHAR ddw[512];
    printf("GetLogicalDriveStringsW: %d\n", GetLogicalDriveStringsW(512, ddw) == d2);

    /* FindFirstFileEx in a scratch folder */
    CreateDirectoryA("wr-strapis", NULL);
    SetCurrentDirectoryA("wr-strapis");
    CloseHandle(CreateFileA("file_a.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL));
    CloseHandle(CreateFileA("file_b.dat", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL));
    CreateDirectoryA("subdir", NULL);
    find_ex("*", FindExInfoStandard, FindExSearchNameMatch, 0);
    find_ex("*", FindExInfoBasic, FindExSearchNameMatch, 0);
    find_ex("file_?.txt", FindExInfoBasic, FindExSearchNameMatch, FIND_FIRST_EX_LARGE_FETCH);
    find_ex("*", FindExInfoStandard, FindExSearchLimitToDirectories, 0);
    find_ex("nothing*", FindExInfoStandard, FindExSearchNameMatch, 0);
    find_ex("subdir\\*", FindExInfoBasic, FindExSearchNameMatch, 0);
    find_ex("", FindExInfoStandard, FindExSearchNameMatch, 0);
    {
        WIN32_FIND_DATAW fw;
        HANDLE h = FindFirstFileExW(L"file_b*", FindExInfoStandard, &fw, FindExSearchNameMatch, NULL, 0);
        printf("FindFirstFileExW file_b*: %d %ls\n", h != INVALID_HANDLE_VALUE, h != INVALID_HANDLE_VALUE ? fw.cFileName : L"");
        if (h != INVALID_HANDLE_VALUE) FindClose(h);
        SetLastError(0xdead);
        h = FindFirstFileExA("*", 7, &fw, FindExSearchNameMatch, NULL, 0);
        printf("FindFirstFileExA bad level: %d err %lu\n", h != INVALID_HANDLE_VALUE, h != INVALID_HANDLE_VALUE ? 0 : GetLastError());
        if (h != INVALID_HANDLE_VALUE) FindClose(h);
    }

    /* LoadCursorFromFile: a 32x32 1 bpp .cur written here */
    {
        static BYTE cur[6 + 16 + 40 + 8 + 128 + 128];
        BYTE *p = cur;
        WORD hdr[3] = { 0, 2, 1 };
        memcpy(p, hdr, 6), p += 6;
        BYTE dir[16] = { 32, 32, 2, 0, 3, 0, 4, 0 };
        DWORD size = 40 + 8 + 128 + 128, off = 22;
        memcpy(dir + 8, &size, 4), memcpy(dir + 12, &off, 4);
        memcpy(p, dir, 16), p += 16;
        BITMAPINFOHEADER bi = { 40, 32, 64, 1, 1 };
        memcpy(p, &bi, 40), p += 40;
        DWORD pal[2] = { 0, 0xffffff };
        memcpy(p, pal, 8), p += 8;
        for (int i = 0; i < 128; i++) p[i] = (i / 4) % 2 ? 0xff : 0x0f; /* xor */
        for (int i = 0; i < 128; i++) p[128 + i] = (i / 4) % 3 ? 0x00 : 0xff; /* and */
        HANDLE f = CreateFileA("probe.cur", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        DWORD wr;
        WriteFile(f, cur, sizeof cur, &wr, NULL);
        CloseHandle(f);
        HCURSOR hc = LoadCursorFromFileA("probe.cur");
        printf("LoadCursorFromFileA: %d\n", hc != NULL);
        if (hc) {
            ICONINFO ii;
            BOOL r = GetIconInfo(hc, &ii);
            BITMAP bm = { 0 };
            if (r) GetObjectA(ii.hbmMask, sizeof bm, &bm);
            printf("cursor info: %d icon %d hotspot %lu,%lu mask %ldx%ld\n", r != 0, r ? ii.fIcon : -1, r ? ii.xHotspot : 0, r ? ii.yHotspot : 0, bm.bmWidth, bm.bmHeight);
            DestroyCursor(hc);
        }
        HCURSOR hw = LoadCursorFromFileW(L"probe.cur");
        printf("LoadCursorFromFileW: %d\n", hw != NULL);
        SetLastError(0xdead);
        hc = LoadCursorFromFileA("missing.cur");
        printf("LoadCursorFromFileA missing: %d err %lu\n", hc != NULL, GetLastError());
        DeleteFileA("probe.cur");
    }
    DeleteFileA("file_a.txt");
    DeleteFileA("file_b.dat");
    RemoveDirectoryA("subdir");
    SetCurrentDirectoryA("..");
    RemoveDirectoryA("wr-strapis");
    return 0;
}
