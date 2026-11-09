/* FILE_FLAG_DELETE_ON_CLOSE (NSIS opens its unpacked data so): the file is there while a
 * handle is open, also after a duplicate of it is closed, and goes with the last handle; a
 * child process that ends without closing its handle leaves nothing either. (sake:
 * files.rs File::delete_on_close; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static char path[MAX_PATH];

static int there(void) {
    return GetFileAttributesA(path) != INVALID_FILE_ATTRIBUTES;
}

int main(int argc, char **argv) {
    GetTempPathA(sizeof path, path);
    strcat(path, "deleteonclose.tmp");
    if (argc > 1) {
        /* the child: opens it and exits without closing */
        HANDLE h = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
        WriteFile(h, "abc", 3, &(DWORD){0}, NULL);
        ExitProcess(h == INVALID_HANDLE_VALUE ? 1 : 7);
    }
    HANDLE h = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE, NULL);
    DWORD n = 0;
    WriteFile(h, "hello", 5, &n, NULL);
    printf("open: %d, written %lu, there %d\n", h != INVALID_HANDLE_VALUE, n, there());
    HANDLE d;
    DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &d, 0, FALSE, DUPLICATE_SAME_ACCESS);
    CloseHandle(h);
    printf("duplicate closed: there %d\n", there());
    char b[8] = "";
    SetFilePointer(d, 0, NULL, FILE_BEGIN);
    ReadFile(d, b, 5, &n, NULL);
    printf("read back: \"%s\"\n", b);
    CloseHandle(d);
    printf("last closed: there %d\n", there());
    /* a plain file stays */
    h = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(h);
    printf("plain file: there %d\n", there());
    DeleteFileA(path);
    char cmd[2 * MAX_PATH];
    char self[MAX_PATH];
    GetModuleFileNameA(NULL, self, sizeof self);
    snprintf(cmd, sizeof cmd, "\"%s\" child", self);
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    if (CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        WaitForSingleObject(pi.hProcess, 60000);
        DWORD code = 0;
        GetExitCodeProcess(pi.hProcess, &code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        printf("child: exit %lu, there %d\n", code, there());
    }
    return 0;
}
