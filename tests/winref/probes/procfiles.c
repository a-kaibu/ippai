/* Files shared by the processes of one launch, as a protected launcher and the game it starts
 * (ソフト電池: the launcher writes the real executable, then starts it) need: a child reads what
 * the parent wrote before starting it (a file past 64 MiB too), starts a program the parent
 * made, writes and renames files the parent then reads, and deletes a file the parent still
 * holds open (the parent keeps reading it). The parent prints; children answer by exit codes.
 * run: browser (in the page, processes share one file system: kura/src/vfs/shared.rs)
 * arch: 32 */
#include "../probe.h"

#define DIR "C:\\masu-procfiles"
#define BIG (80u << 20)

static unsigned sum(const unsigned char *p, size_t n) {
    unsigned s = 0;
    for (size_t i = 0; i < n; i++) s = s * 31 + p[i];
    return s;
}
static int read_all(const char *path, char *buf, DWORD cap, DWORD *n) {
    HANDLE f = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) return 0;
    BOOL ok = ReadFile(f, buf, cap, n, NULL);
    CloseHandle(f);
    return ok;
}
static int write_all(const char *path, const void *p, DWORD n) {
    HANDLE f = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (f == INVALID_HANDLE_VALUE) return 0;
    DWORD w;
    BOOL ok = WriteFile(f, p, n, &w, NULL) && w == n;
    CloseHandle(f);
    return ok;
}

static int child(const char *mode) {
    char buf[256];
    DWORD n = 0;
    if (!strcmp(mode, "read")) {
        if (!read_all(DIR "\\parent.txt", buf, sizeof buf, &n) || n != 11 || memcmp(buf, "from parent", 11)) return 10;
        // (the big file: its size and contents)
        HANDLE f = CreateFileA(DIR "\\big.bin", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        if (f == INVALID_HANDLE_VALUE) return 11;
        if (GetFileSize(f, NULL) != BIG) return 12;
        unsigned char *p = malloc(BIG);
        if (!p || !ReadFile(f, p, BIG, &n, NULL) || n != BIG) return 13;
        CloseHandle(f);
        unsigned s = sum(p, BIG);
        free(p);
        if (!write_all(DIR "\\child.tmp", &s, 4)) return 14;
        if (!MoveFileExA(DIR "\\child.tmp", DIR "\\child.txt", MOVEFILE_REPLACE_EXISTING)) return 15;
        return 0;
    }
    if (!strcmp(mode, "made")) return 7;  /* (the program the parent wrote: it ran) */
    if (!strcmp(mode, "delete")) return DeleteFileA(DIR "\\held.txt") ? 0 : 20;
    return 99;
}

static char self[MAX_PATH];
static void run(const char *exe, const char *mode) {
    char cmd[MAX_PATH * 2];
    snprintf(cmd, sizeof cmd, "\"%s\" child %s", exe, mode);
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    BOOL ok = CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    DWORD code = 0xdead;
    if (ok) {
        WaitForSingleObject(pi.hProcess, 120000);
        GetExitCodeProcess(pi.hProcess, &code);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    printf("child %s: started %d exit %lu\n", mode, ok, code);
}

int main(int argc, char **argv) {
    if (argc > 2 && !strcmp(argv[1], "child")) return child(argv[2]);
    GetModuleFileNameA(NULL, self, sizeof self);
    CreateDirectoryA(DIR, NULL);
    printf("write parent: %d\n", write_all(DIR "\\parent.txt", "from parent", 11));
    unsigned char *p = malloc(BIG);
    for (unsigned i = 0; i < BIG; i++) p[i] = (unsigned char)(i * 7 + (i >> 13));
    unsigned s = sum(p, BIG);
    printf("write big: %d\n", write_all(DIR "\\big.bin", p, BIG));
    free(p);
    run(self, "read");
    char buf[16];
    DWORD n = 0;
    unsigned got = 0;
    int r = read_all(DIR "\\child.txt", buf, sizeof buf, &n);
    if (n == 4) memcpy(&got, buf, 4);
    printf("child wrote: %d %lu, its sum matches: %d\n", r, n, got == s);
    printf("temporary gone: %d\n", GetFileAttributesA(DIR "\\child.tmp") == INVALID_FILE_ATTRIBUTES);
    /* a program written by this one (as a launcher writes the game), then started */
    printf("copy self: %d\n", CopyFileA(self, DIR "\\made.exe", FALSE));
    run(DIR "\\made.exe", "made");
    /* a file deleted by the child while this one holds it open */
    write_all(DIR "\\held.txt", "still here", 10);
    HANDLE h = CreateFileA(DIR "\\held.txt", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    run(self, "delete");
    memset(buf, 0, sizeof buf);
    BOOL ok = ReadFile(h, buf, 10, &n, NULL);
    printf("held file after delete: %d %lu %.10s\n", ok, n, buf);
    CloseHandle(h);
    printf("held file gone after close: %d\n", GetFileAttributesA(DIR "\\held.txt") == INVALID_FILE_ATTRIBUTES);
    DeleteFileA(DIR "\\parent.txt"); DeleteFileA(DIR "\\big.bin"); DeleteFileA(DIR "\\child.txt"); DeleteFileA(DIR "\\made.exe");
    RemoveDirectoryA(DIR);
    return 0;
}
