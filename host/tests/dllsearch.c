#ifdef BUILD_FIXTURE
__declspec(dllexport) int Answer(void) { return 42; }
#else
#include <windows.h>
#include <stdio.h>
#include <string.h>
#include <winver.h>
/* libs: -lversion */

static int version(const char *path) {
    DWORD n = GetFileVersionInfoSizeA(path, NULL);
    BYTE info[1024];
    VS_FIXEDFILEINFO *fixed = NULL;
    UINT size = 0;
    int ok = n && n <= sizeof(info) && GetFileVersionInfoA(path, 0, n, info)
        && VerQueryValueA(info, "\\", (void **)&fixed, &size)
        && size == sizeof(*fixed) && fixed->dwFileVersionMS == 0x10002
        && fixed->dwFileVersionLS == 0x30004;
    printf("unloaded file version: %s\n", ok ? "ok" : "FAIL");
    return !ok;
}

static int check(const char *label, const char *path) {
    HMODULE m = LoadLibraryA(path);
    int (*answer)(void) = m ? (int (*)(void))GetProcAddress(m, "Answer") : NULL;
    int ok = answer && answer() == 42;
    printf("%s: %s\n", label, ok ? "ok" : "FAIL");
    if (m) FreeLibrary(m);
    return !ok;
}
int main(void) {
    char fixture[MAX_PATH], target[MAX_PATH], absolute[MAX_PATH];
    GetModuleFileNameA(NULL, fixture, sizeof(fixture));
    char *name = strrchr(fixture, '\\');
    if (!name) return 1;
    strcpy(name + 1, "dllsearch-fixture.dll");
    GetSystemDirectoryA(target, sizeof(target));
    strcat(target, "\\sake-vendor-search.dll");
    GetSystemDirectoryA(absolute, sizeof(absolute));
    strcat(absolute, "\\sake-vendor-absolute.dll");
    if (!CopyFileA(fixture, target, FALSE) || !CopyFileA(fixture, absolute, FALSE)) return 1;
    int failed = version(absolute);
    int unloaded = GetModuleHandleA("sake-vendor-absolute.dll") == NULL;
    printf("version does not load DLL: %s\n", unloaded ? "ok" : "FAIL");
    failed |= !unloaded;
    failed |= check("vendor system directory", "sake-vendor-search.dll");
    failed |= check("vendor absolute path", absolute);
    HMODULE kernel = LoadLibraryA("C:\\windows\\system32\\kernel32.dll");
    int ok = kernel && GetProcAddress(kernel, "GetCurrentProcessId") != NULL;
    printf("system implementation: %s\n", ok ? "ok" : "FAIL");
    failed |= !ok;
    DeleteFileA(target);
    DeleteFileA(absolute);
    return failed;
}
#endif
