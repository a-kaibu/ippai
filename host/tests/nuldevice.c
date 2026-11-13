/* The null device by CreateFile: "NUL" in its spellings opens (any folder, "\\.\NUL", a trailing
 * ':'), reads end at once with success, writes take every byte, and it is a character device.
 * RGSS3's Ruby (RPG Maker VX Ace) opens NUL for its standard handles and crashed when it did
 * not open (犯されスライム体験版). The expected lines are Windows' answers; x86 and x86-64.
 * libs: -nostdlib -luser32 -lkernel32 */
#include <windows.h>

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

static void try_name(const char *name, DWORD disp)
{
    HANDLE h = CreateFileA(name, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, disp, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        out("%s: error %d\n", name, (int)GetLastError());
        return;
    }
    char buf[8] = "abcdefg";
    DWORD w = 99, r = 99;
    BOOL wr = WriteFile(h, buf, 5, &w, NULL);
    BOOL rd = ReadFile(h, buf, sizeof buf, &r, NULL);
    DWORD type = GetFileType(h);
    BOOL closed = CloseHandle(h);
    out("%s: open, type %d, write %d %d, read %d %d, buffer kept %d, close %d\n", name, (int)type, wr, (int)w, rd, (int)r,
        buf[0] == 'a', closed);
}

void mainCRTStartup(void)
{
    try_name("NUL", OPEN_EXISTING);
    try_name("nul", OPEN_EXISTING);
    try_name("\\\\.\\NUL", OPEN_EXISTING);
    try_name("C:\\windows\\nul", OPEN_EXISTING);
    try_name("nul:", OPEN_EXISTING);
    try_name("NUL", CREATE_ALWAYS);
    try_name("NUL", OPEN_ALWAYS);
    /* (a read-only handle reads too) */
    HANDLE h = CreateFileA("NUL", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    DWORD r = 7;
    char b[4];
    BOOL rd = ReadFile(h, b, 4, &r, NULL);
    out("read-only: %d read %d %d\n", h != INVALID_HANDLE_VALUE, rd, (int)r);
    CloseHandle(h);
    /* not the device: a name that only starts with NUL */
    h = CreateFileA("NULX", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    out("NULX: %d error %d\n", h != INVALID_HANDLE_VALUE, (int)GetLastError());
    ExitProcess(0);
}
