/* CreateHardLink as programs (msvcp140's _Link) use it: a second name of the same file (its
 * bytes shared both ways, one file index, the link count), the name kept when the first goes,
 * and the errors: an existing new name, a missing file, a directory. sake had no hard links
 * under node (#58). */
#include "../probe.h"

static void info(const char *what, const char *path) {
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    BY_HANDLE_FILE_INFORMATION i;
    if (h == INVALID_HANDLE_VALUE || !GetFileInformationByHandle(h, &i)) {
        printf("%s: no file (%lu)\n", what, GetLastError());
        return;
    }
    printf("%s: links %lu size %lu\n", what, i.nNumberOfLinks, i.nFileSizeLow);
    CloseHandle(h);
}
static unsigned long long index_of(const char *path) {
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    BY_HANDLE_FILE_INFORMATION i = {0};
    GetFileInformationByHandle(h, &i);
    CloseHandle(h);
    return (unsigned long long)i.nFileIndexHigh << 32 | i.nFileIndexLow;
}
static void put(const char *path, const char *s, DWORD how) {
    HANDLE h = CreateFileA(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, how, 0, NULL);
    DWORD w;
    if (how == OPEN_EXISTING) SetFilePointer(h, 0, NULL, FILE_END);
    WriteFile(h, s, strlen(s), &w, NULL);
    CloseHandle(h);
}
static void show(const char *what, const char *path) {
    char b[64] = {0};
    DWORD r = 0;
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    ReadFile(h, b, sizeof b - 1, &r, NULL);
    CloseHandle(h);
    printf("%s: \"%s\"\n", what, b);
}

int main(void) {
    CreateDirectoryA("hl", NULL);
    put("hl\\a.txt", "first", CREATE_ALWAYS);
    info("a alone", "hl\\a.txt");
    SetLastError(0xdead);
    BOOL r = CreateHardLinkA("hl\\b.txt", "hl\\a.txt", NULL);
    printf("CreateHardLink %d error %lu\n", r, r ? 0 : GetLastError());
    info("a", "hl\\a.txt");
    info("b", "hl\\b.txt");
    printf("one file index %d\n", index_of("hl\\a.txt") == index_of("hl\\b.txt"));
    put("hl\\b.txt", "+b", OPEN_EXISTING);
    show("a after writing b", "hl\\a.txt");
    put("hl\\a.txt", "+a", OPEN_EXISTING);
    show("b after writing a", "hl\\b.txt");

    SetLastError(0xdead);
    r = CreateHardLinkA("hl\\b.txt", "hl\\a.txt", NULL);
    printf("new name exists %d error %lu\n", r, r ? 0 : GetLastError());
    SetLastError(0xdead);
    r = CreateHardLinkA("hl\\c.txt", "hl\\none.txt", NULL);
    printf("missing file %d error %lu\n", r, r ? 0 : GetLastError());
    SetLastError(0xdead);
    r = CreateHardLinkA("hl\\d", "hl", NULL);
    printf("a directory %d error %lu\n", r, r ? 0 : GetLastError());
    SetLastError(0xdead);
    r = CreateHardLinkA("hl\\nodir\\e.txt", "hl\\a.txt", NULL);
    printf("into a missing folder %d error %lu\n", r, r ? 0 : GetLastError());

    printf("DeleteFile a %d\n", DeleteFileA("hl\\a.txt"));
    info("b after a went", "hl\\b.txt");
    show("b", "hl\\b.txt");
    DeleteFileA("hl\\b.txt");
    RemoveDirectoryA("hl");
    return 0;
}
