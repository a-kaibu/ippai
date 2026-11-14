/* WriteConsoleA/W on handles that are no console (a file, the null device) fail with
 * ERROR_INVALID_HANDLE and write nothing. Ruby 1.9 (RPG Maker VX Ace's RGSS301.dll) tries
 * WriteConsoleW with 0 characters on every file it writes and uses WriteFile when that fails:
 * sake took it, and a game's Marshal.dump went to the log, its file empty (#155). */
#include "../probe.h"

static void try_handle(const char *what, HANDLE h) {
    DWORD w = 0xdead;
    SetLastError(0xdead);
    BOOL ok = WriteConsoleW(h, L"", 0, &w, NULL);
    printf("%-6s WriteConsoleW 0: %d err %lu written %lx\n", what, ok, ok ? 0 : GetLastError(), w);
    w = 0xdead;
    SetLastError(0xdead);
    ok = WriteConsoleW(h, L"abc", 3, &w, NULL);
    printf("%-6s WriteConsoleW 3: %d err %lu written %lx\n", what, ok, ok ? 0 : GetLastError(), w);
    w = 0xdead;
    SetLastError(0xdead);
    ok = WriteConsoleA(h, "abc", 3, &w, NULL);
    printf("%-6s WriteConsoleA 3: %d err %lu written %lx\n", what, ok, ok ? 0 : GetLastError(), w);
}

int main(void) {
    HANDLE f = CreateFileA("writecon.tmp", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    printf("file type %lu\n", GetFileType(f));
    try_handle("file", f);
    printf("file size after %lu\n", GetFileSize(f, NULL));
    /* as Ruby writes: the console first, then the file */
    DWORD w = 0, mode = 0xdead;
    printf("file console mode: %d\n", GetConsoleMode(f, &mode));
    if (!WriteConsoleW(f, L"", 0, &w, NULL)) {
        BOOL ok = WriteFile(f, "\x04\x08o:\x15", 5, &w, NULL);
        printf("WriteFile: %d %lu\n", ok, w);
    }
    printf("file size %lu\n", GetFileSize(f, NULL));
    CloseHandle(f);
    DeleteFileA("writecon.tmp");
    HANDLE n = CreateFileA("NUL", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    try_handle("nul", n);
    CloseHandle(n);
    return 0;
}
