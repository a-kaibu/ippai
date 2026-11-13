/* FindFirstFile on system DLLs, as F&C's overture.exe (C4 trial) checks for DirectX 9: the
 * entry's attributes and name, then ERROR_NO_MORE_FILES; a DLL that is not there. */
#include "../probe.h"

static void find(const char *dir, const char *name) {
    char path[MAX_PATH];
    snprintf(path, sizeof path, "%s\\%s", dir, name);
    WIN32_FIND_DATAA d;
    SetLastError(0xdead);
    HANDLE h = FindFirstFileA(path, &d);
    if (h == INVALID_HANDLE_VALUE) { printf("%s: none err %lu\n", name, GetLastError()); return; }
    printf("%s: found %s attrs %lx\n", name, d.cFileName, d.dwFileAttributes & ~FILE_ATTRIBUTE_NOT_CONTENT_INDEXED);
    SetLastError(0xdead);
    BOOL more = FindNextFileA(h, &d);
    printf("  next %d err %lu\n", more, GetLastError());
    FindClose(h);
}

int main(void) {
    char sys[MAX_PATH];
    GetSystemDirectoryA(sys, MAX_PATH);
    find(sys, "d3d9.dll");
    find(sys, "ddraw.dll");
    find(sys, "kernel32.dll");
    find(sys, "ippai-no-such-lib.dll");
    printf("attrs %lx\n", GetFileAttributesA(strcat(sys, "\\d3d9.dll")) & ~FILE_ATTRIBUTE_NOT_CONTENT_INDEXED);
    return 0;
}
