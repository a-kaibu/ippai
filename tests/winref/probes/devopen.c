/* CreateFile of \\.\ device names a program may try: a drive's volume opens with no access
 * asked (Enigma Protector, anemoi's SiglusEngine.exe, opens \\.\C: so), kernel debuggers'
 * devices (\\.\NTICE, \\.\SICE: SoftICE) and a drive letter nobody has are not there. */
#include "../probe.h"

static void try_open(const char *name, DWORD access) {
    SetLastError(0xdead);
    HANDLE h = CreateFileA(name, access, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    DWORD err = GetLastError();
    printf("%s access %lx: %s err %lu\n", name, access, h == INVALID_HANDLE_VALUE ? "fails" : "opens", h == INVALID_HANDLE_VALUE ? err : 0);
    if (h != INVALID_HANDLE_VALUE) printf("  close %d\n", CloseHandle(h));
}

int main(void) {
    try_open("\\\\.\\C:", 0);
    try_open("\\\\.\\c:", 0);
    try_open("\\\\.\\NTICE", 0);
    try_open("\\\\.\\NTICE", GENERIC_READ);
    try_open("\\\\.\\SICE", GENERIC_READ | GENERIC_WRITE);
    try_open("\\\\.\\SIWVID", GENERIC_READ);
    try_open("\\\\.\\Q:", 0);
    return 0;
}
