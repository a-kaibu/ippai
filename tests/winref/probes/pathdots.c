/* Periods and spaces at the end of a path's folder names (#232): RPG Maker VX's Game.exe loads
 * "<RTP folder>.\RGSS202J.dll" (the registry's folder, then ".\" and the DLL), which Windows
 * finds in the RTP folder. GetFullPathName of such paths, and opening a file through them.
 * Runs in a scratch directory "wr-pathdots" under the current one. */
#include "../probe.h"

int main(void) {
    CreateDirectoryA("wr-pathdots", NULL);
    SetCurrentDirectoryA("wr-pathdots");
    CreateDirectoryA("dir", NULL);
    HANDLE h = CreateFileA("dir\\f.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(h, "x", 1, &w, NULL);
    CloseHandle(h);
    static const char *ps[] = { "dir.\\f.txt", "dir..\\f.txt", "dir...\\f.txt", "dir .\\f.txt", "dir. \\f.txt",
                                "dir. .\\f.txt", "dir.\\", "dir.", "dir.\\sub.\\g", "C:\\a.\\b.\\c", "C:\\a..\\c" };
    char cur[MAX_PATH];
    GetCurrentDirectoryA(MAX_PATH, cur);
    size_t cl = strlen(cur);
    for (unsigned i = 0; i < sizeof ps / sizeof *ps; i++) {
        char out[MAX_PATH], *part = NULL;
        GetFullPathNameA(ps[i], MAX_PATH, out, &part);
        int in_cur = strncmp(out, cur, cl) == 0;
        printf("GetFullPathName '%s' %s%s part %s\n", ps[i], in_cur ? "<cur>" : "", in_cur ? out + cl : out, part ? part : "(null)");
        SetLastError(0);
        DWORD a = GetFileAttributesA(ps[i]);
        printf("  GetFileAttributes %s err %lu\n", a == INVALID_FILE_ATTRIBUTES ? "none" : (a & FILE_ATTRIBUTE_DIRECTORY) ? "dir" : "file", a == INVALID_FILE_ATTRIBUTES ? GetLastError() : 0);
    }
    DeleteFileA("dir\\f.txt");
    RemoveDirectoryA("dir");
    SetCurrentDirectoryA("..");
    RemoveDirectoryA("wr-pathdots");
    return 0;
}
