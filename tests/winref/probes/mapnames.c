/* Named file mappings as F&C's overture.exe (C4 trial) uses them: a pagefile section created
 * with a name, opened again by OpenFileMapping, both views showing the same bytes; creating it
 * again (ERROR_ALREADY_EXISTS), the names shared with events, bad names, the name gone once the
 * last handle closes. Then a file mapping as F&C's file.dll writes HARDWARE_INFORMATION.TXT: the
 * file grows to the mapping's size, the view's bytes reach the file (FlushViewOfFile, unmap),
 * copy-on-write views do not. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-80s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

int main(void) {
    const char *n = "ippai-winref-map";
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, n) != NULL);
    SetLastError(0xdead);
    HANDLE h = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, n);
    printf("create %d err %lu\n", h != NULL, GetLastError());
    DWORD *v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    printf("view %d\n", v != NULL);
    v[6] = 0x12345678;
    HANDLE h2 = OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, n);
    printf("open %d other handle %d\n", h2 != NULL, h2 != h);
    DWORD *v2 = MapViewOfFile(h2, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    printf("second view +0x18 %08lx\n", v2 ? v2[6] : 0);
    v2[0] = 77;
    printf("first view +0 %lu\n", v[0]);
    SetLastError(0xdead);
    HANDLE h3 = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, n);
    printf("again %d err %lu\n", h3 != NULL, GetLastError());
    DWORD *v3 = MapViewOfFile(h3, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    printf("third view +0x18 %08lx\n", v3 ? v3[6] : 0);
    HANDLE hw = OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, L"ippai-winref-map");
    printf("open wide %d\n", hw != NULL);
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, NULL) != NULL);
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, "ippai-winref-none") != NULL);
    E(CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, "bad\\name") != NULL);
    /* one namespace with events */
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, "ippai-winref-mapev");
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, "ippai-winref-mapev") != NULL);
    E(CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, "ippai-winref-mapev") != NULL);
    CloseHandle(ev);
    HANDLE m = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, "ippai-winref-mapev");
    printf("mapping after the event closed %d\n", m != NULL);
    E(OpenEventA(EVENT_ALL_ACCESS, FALSE, "ippai-winref-mapev") != NULL);
    CloseHandle(m);
    /* every handle closed (the views stay mapped): the name is gone */
    UnmapViewOfFile(v); UnmapViewOfFile(v2); UnmapViewOfFile(v3);
    CloseHandle(h); CloseHandle(h2); CloseHandle(h3); CloseHandle(hw);
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, n) != NULL);
    SetLastError(0xdead);
    HANDLE h5 = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x20, n);
    printf("create again %d err %lu\n", h5 != NULL, GetLastError());
    DWORD *v5 = MapViewOfFile(h5, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    printf("new section +0x18 %08lx\n", v5 ? v5[6] : 0);

    /* a section mapped again at an offset (F&C's archive index: 0x49c47 bytes, then 0x40000 in) */
    HANDLE big = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x49c47, NULL);
    char *bv = MapViewOfFile(big, FILE_MAP_ALL_ACCESS, 0, 0, 0x49c47);
    strcpy(bv + 0x40000 + 0x9215, "startmes.mes");
    char *pv = MapViewOfFile(big, FILE_MAP_READ, 0, 0x40000, 0x931a);
    printf("offset view %s\n", pv ? pv + 0x9215 : "(null)");
    SetLastError(0xdead);
    void *pe = MapViewOfFile(big, FILE_MAP_READ, 0, 0x40000, 0x10000);
    printf("past the end %d err %lu\n", pe != NULL, GetLastError());
    SetLastError(0xdead);
    pe = MapViewOfFile(big, FILE_MAP_READ, 0, 0x50000, 0);
    printf("offset past the end %d err %lu\n", pe != NULL, GetLastError());
    UnmapViewOfFile(pv); UnmapViewOfFile(bv); CloseHandle(big);

    /* a file */
    char path[MAX_PATH], tmp[MAX_PATH];
    GetTempPathA(MAX_PATH, tmp);
    GetTempFileNameA(tmp, "map", 0, path);
    HANDLE f = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD done;
    WriteFile(f, "hello", 5, &done, NULL);
    HANDLE fm = CreateFileMappingA(f, NULL, PAGE_READWRITE, 0, 0x5000, NULL);
    printf("file mapping %d size now %lu\n", fm != NULL, GetFileSize(f, NULL));
    char *fv = MapViewOfFile(fm, FILE_MAP_WRITE, 0, 0, 0);
    memcpy(fv + 0x1000, "WORLD", 5);
    printf("flush %d\n", FlushViewOfFile(fv + 0x1000, 5));
    char rb[8] = { 0 };
    SetFilePointer(f, 0x1000, NULL, FILE_BEGIN);
    ReadFile(f, rb, 5, &done, NULL);
    printf("read after flush %s\n", rb);
    fv[0] = 'J';
    UnmapViewOfFile(fv);
    char *cv = MapViewOfFile(fm, FILE_MAP_COPY, 0, 0, 0);
    cv[1] = 'X';
    UnmapViewOfFile(cv);
    CloseHandle(fm);
    SetFilePointer(f, 0, NULL, FILE_BEGIN);
    ReadFile(f, rb, 5, &done, NULL);
    printf("read after unmap %s\n", rb);
    SetFilePointer(f, 0x191, NULL, FILE_BEGIN);
    SetEndOfFile(f);
    printf("truncated %lu\n", GetFileSize(f, NULL));
    CloseHandle(f);
    DeleteFileA(path);
    return 0;
}
