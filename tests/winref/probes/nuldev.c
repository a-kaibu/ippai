/* The null device: CreateFile of "NUL" (any case, as the last part of a path, with a colon)
 * opens it; writes take everything, reads are at the end at once; it is a character device.
 * Ruby (RPG Maker VX Ace's RGSS301.dll) opens it at start: when sake answered "file not found"
 * the game crashed. Also what "nul.txt" is (the device on older Windows, a file since 11). */
#include "../probe.h"

static void open_nul(const char *n) {
    SetLastError(0xdead);
    HANDLE h = CreateFileA(n, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    DWORD err = GetLastError();
    if (h == INVALID_HANDLE_VALUE) {
        printf("%-24s: fails, err %lu\n", n, err);
        return;
    }
    DWORD w = 0xdead, r = 0xdead;
    char buf[16] = "0123456789";
    BOOL wok = WriteFile(h, buf, 10, &w, NULL);
    BOOL rok = ReadFile(h, buf, sizeof buf, &r, NULL);
    printf("%-24s: opens, err %lu, type %lu, write %d %lu, read %d %lu\n", n, err, GetFileType(h), wok, w, rok, r);
    CloseHandle(h);
}

int main(void) {
    open_nul("NUL");
    open_nul("nul");
    open_nul("\\\\.\\NUL");
    open_nul("C:\\Windows\\nul");
    open_nul("nul:");
    /* (CREATE_ALWAYS of NUL opens the device too) */
    SetLastError(0xdead);
    HANDLE h = CreateFileA("NUL", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    printf("CREATE_ALWAYS NUL: %s err %lu\n", h == INVALID_HANDLE_VALUE ? "fails" : "opens", GetLastError());
    if (h != INVALID_HANDLE_VALUE) CloseHandle(h);
    return 0;
}
