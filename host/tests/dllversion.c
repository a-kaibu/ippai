/* libs: -lshlwapi */
/* DllGetVersion of shlwapi, shell32 and comctl32 (no common controls 6 manifest): programs ask
 * it before they start (魔王戦記 by 玉藻スタジオ: "IE5以降がインストールされていません" when
 * shlwapi's is missing). DLLVERSIONINFO and DLLVERSIONINFO2 (dwFlags, ullVersion in 16-bit
 * fields), other cbSize values and NULL. The build is Windows' own, so only whether it is set
 * and agrees with ullVersion is printed. (sake: misc.rs dll_get_version) */
#include <windows.h>
#include <shlwapi.h>
#include <stdio.h>
#include <string.h>

typedef HRESULT (WINAPI *DGV)(DLLVERSIONINFO *);

int main(void) {
    static const char *dlls[] = { "shlwapi.dll", "shell32.dll", "comctl32.dll" };
    for (int i = 0; i < 3; i++) {
        HMODULE m = LoadLibraryA(dlls[i]);
        DGV f = m ? (DGV)GetProcAddress(m, "DllGetVersion") : NULL;
        printf("%s: %s\n", dlls[i], f ? "DllGetVersion" : "no DllGetVersion");
        if (!f) continue;
        DLLVERSIONINFO2 v;
        memset(&v, 0xcc, sizeof v);
        v.info1.cbSize = sizeof(DLLVERSIONINFO);
        HRESULT hr = f(&v.info1);
        printf("  v1: %lx %lu.%lu platform %lu build set %d, dwFlags untouched %d\n", hr, v.info1.dwMajorVersion,
               v.info1.dwMinorVersion, v.info1.dwPlatformID, v.info1.dwBuildNumber != 0 && v.info1.dwBuildNumber != 0xcccccccc,
               v.dwFlags == 0xcccccccc);
        DWORD build = v.info1.dwBuildNumber;
        memset(&v, 0xcc, sizeof v);
        v.info1.cbSize = sizeof v;
        hr = f(&v.info1);
        printf("  v2: %lx %lu.%lu platform %lu flags %lx ullVersion %u.%u build agrees %d\n", hr, v.info1.dwMajorVersion,
               v.info1.dwMinorVersion, v.info1.dwPlatformID, v.dwFlags, (unsigned)(v.ullVersion >> 48),
               (unsigned)(v.ullVersion >> 32 & 0xffff), (DWORD)(v.ullVersion >> 16 & 0xffff) == (build & 0xffff) && v.info1.dwBuildNumber == build);
        DWORD sizes[] = { 0, 4, 16, 24, 28, 36, 64 };
        for (int k = 0; k < 7; k++) {
            memset(&v, 0xcc, sizeof v);
            v.info1.cbSize = sizes[k];
            hr = f(&v.info1);
            printf("  cbSize %lu: %lx major untouched %d\n", sizes[k], hr, v.info1.dwMajorVersion == 0xcccccccc);
        }
    }
    return 0;
}
