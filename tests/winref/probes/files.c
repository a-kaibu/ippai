/* kernel32 files: dispositions and the error codes games check, sharing, seeking past the end,
 * FindFirstFile patterns, GetFullPathName normalization, directories. Runs in a scratch
 * directory "wr-files" under the current one. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-60s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static void touch(const char *n, const char *s) {
    HANDLE h = CreateFileA(n, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(h, s, (DWORD)strlen(s), &w, NULL);
    CloseHandle(h);
}

static void find(const char *pat) {
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    char names[64][MAX_PATH];
    int n = 0;
    if (h != INVALID_HANDLE_VALUE) {
        do { if (n < 64) strcpy(names[n++], fd.cFileName); } while (FindNextFileA(h, &fd));
        printf("find %s: end err %lu:", pat, GetLastError());
        FindClose(h);
    } else {
        printf("find %s: err %lu:", pat, GetLastError());
    }
    /* the order is the file system's: sort */
    for (int i = 0; i < n; i++)
        for (int j = i + 1; j < n; j++)
            if (_stricmp(names[j], names[i]) < 0) { char t[MAX_PATH]; strcpy(t, names[i]); strcpy(names[i], names[j]); strcpy(names[j], t); }
    for (int i = 0; i < n; i++) printf(" %s", names[i]);
    printf("\n");
}

static void rmtree(const char *d) {
    char pat[MAX_PATH];
    WIN32_FIND_DATAA fd;
    sprintf(pat, "%s\\*", d);
    HANDLE h = FindFirstFileA(pat, &fd);
    if (h != INVALID_HANDLE_VALUE) {
        do {
            if (!strcmp(fd.cFileName, ".") || !strcmp(fd.cFileName, "..")) continue;
            char p[MAX_PATH];
            sprintf(p, "%s\\%s", d, fd.cFileName);
            if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) rmtree(p);
            else { SetFileAttributesA(p, FILE_ATTRIBUTE_NORMAL); DeleteFileA(p); }
        } while (FindNextFileA(h, &fd));
        FindClose(h);
    }
    RemoveDirectoryA(d);
}

int main(void) {
    rmtree("wr-files");
    E(CreateDirectoryA("wr-files", NULL));
    E(CreateDirectoryA("wr-files", NULL));
    E(SetCurrentDirectoryA("wr-files"));
    E(CreateFileA("missing.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE);
    E(CreateFileA("nodir\\missing.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE);
    E(GetFileAttributesA("missing.txt"));
    E(GetFileAttributesA("nodir\\x"));
    touch("a.txt", "hello");
    E(CreateFileA("a.txt", GENERIC_READ, 0, NULL, CREATE_NEW, 0, NULL) == INVALID_HANDLE_VALUE);
    {
        HANDLE h = CreateFileA("a.txt", GENERIC_READ, 0, NULL, OPEN_ALWAYS, 0, NULL);
        printf("OPEN_ALWAYS existing err %lu\n", GetLastError());
        E(CreateFileA("a.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE);
        E(DeleteFileA("a.txt"));
        char b[16] = { 0 };
        DWORD r = 7;
        E(ReadFile(h, b, 3, &r, NULL));
        printf("read %lu %s\n", r, b);
        E(SetFilePointer(h, 10, NULL, FILE_BEGIN));
        r = 7;
        E(ReadFile(h, b, 3, &r, NULL));
        printf("read past end %lu\n", r);
        E(SetFilePointer(h, -20, NULL, FILE_CURRENT));
        E(GetFileSize(h, NULL));
        CloseHandle(h);
    }
    {
        HANDLE h = CreateFileA("a.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        E(CreateFileA("a.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL) == INVALID_HANDLE_VALUE);
        HANDLE h2 = CreateFileA("a.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        printf("second reader %d\n", h2 != INVALID_HANDLE_VALUE);
        CloseHandle(h2);
        CloseHandle(h);
    }
    {
        HANDLE h = CreateFileA("b.bin", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        printf("CREATE_ALWAYS new err %lu\n", GetLastError());
        E(SetFilePointer(h, 100, NULL, FILE_BEGIN));
        DWORD w;
        E(WriteFile(h, "Z", 1, &w, NULL));
        E(GetFileSize(h, NULL));
        unsigned char b[101];
        SetFilePointer(h, 0, NULL, FILE_BEGIN);
        ReadFile(h, b, 101, &w, NULL);
        int zeros = 0;
        for (int i = 0; i < 100; i++) zeros += b[i] == 0;
        printf("gap zeros %d last %c\n", zeros, b[100]);
        E(SetFilePointer(h, 50, NULL, FILE_BEGIN));
        E(SetEndOfFile(h));
        E(GetFileSize(h, NULL));
        CloseHandle(h);
        h = CreateFileA("b.bin", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
        E(WriteFile(h, "Z", 1, &w, NULL));
        CloseHandle(h);
        h = CreateFileA("b.bin", GENERIC_WRITE, 0, NULL, TRUNCATE_EXISTING, 0, NULL);
        E(GetFileSize(h, NULL));
        CloseHandle(h);
    }
    E(MoveFileA("a.txt", "b.bin"));
    E(MoveFileExA("a.txt", "b.bin", MOVEFILE_REPLACE_EXISTING));
    E(GetFileAttributesA("a.txt"));
    E(CopyFileA("b.bin", "c.dat", TRUE));
    E(CopyFileA("b.bin", "c.dat", TRUE));
    E(SetFileAttributesA("c.dat", FILE_ATTRIBUTE_READONLY));
    E(DeleteFileA("c.dat"));
    E(GetFileAttributesA("c.dat"));
    E(SetFileAttributesA("c.dat", FILE_ATTRIBUTE_NORMAL));
    E(CreateDirectoryA("sub", NULL));
    touch("sub\\x1.txt", "1");
    touch("sub\\x22.txt", "22");
    touch("sub\\Y.TXT", "Y");
    touch("sub\\noext", "n");
    touch("sub\\two.dots.txt", "d");
    touch("sub\\long_name_file.text", "l");
    E(RemoveDirectoryA("sub"));
    E(RemoveDirectoryA("nosuch"));
    find("sub\\*");
    find("sub\\*.txt");
    find("sub\\*.TXT");
    find("sub\\x?.txt");
    find("sub\\x??.txt");
    find("sub\\*.");
    find("sub\\*.*");
    find("sub\\*.tex");
    find("sub\\y.txt");
    find("sub\\none*");
    find("nodir\\*");
    find("sub");
    find("sub\\");
    {
        static const char *ps[] = { "x.txt", ".\\a\\..\\b", "sub\\..\\..\\q", "a/b\\c", "trail.", "trail. .",
                                    "C:\\dir\\..\\..\\..\\file", "\\\\server\\share\\x\\..", "a\\\\b" };
        char cur[MAX_PATH], par[MAX_PATH];
        GetCurrentDirectoryA(MAX_PATH, cur);
        size_t cl = strlen(cur);
        strcpy(par, cur);
        *strrchr(par, '\\') = 0;
        size_t pl = strlen(par);
        for (unsigned i = 0; i < sizeof ps / sizeof *ps; i++) {
            char out[MAX_PATH], *part = NULL;
            DWORD n = GetFullPathNameA(ps[i], MAX_PATH, out, &part);
            int in_cur = strncmp(out, cur, cl) == 0, in_par = !in_cur && strncmp(out, par, pl) == 0;
            const char *shown = in_cur ? out + cl : in_par ? out + pl : (out[1] == ':' ? out + 2 : out);
            printf("GetFullPathName '%s' %s%s part %s\n", ps[i], in_cur ? "<cur>" : in_par ? "<parent>" : "",
                   shown, part ? part : "(null)");
            (void)n;
        }
    }
    /* CreateFile2 (Windows 8's; AdvHD opens its files with it): the dispositions and errors of
     * CreateFileW, attributes from the extended parameters */
    {
        typedef HANDLE(WINAPI * CF2)(LPCWSTR, DWORD, DWORD, DWORD, void *);
        CF2 cf2 = (CF2)GetProcAddress(GetModuleHandleA("kernel32.dll"), "CreateFile2");
        printf("CreateFile2 exported %d\n", cf2 != NULL);
        if (cf2) {
            struct { DWORD size, attrs, flags, qos; void *sa; HANDLE tmpl; } ex = { sizeof ex, FILE_ATTRIBUTE_HIDDEN, 0, 0, NULL, NULL };
            E(cf2(L"cf2-missing.txt", GENERIC_READ, 0, OPEN_EXISTING, NULL) == INVALID_HANDLE_VALUE);
            HANDLE h = cf2(L"cf2.txt", GENERIC_WRITE, 0, CREATE_NEW, &ex);
            printf("CreateFile2 CREATE_NEW ok %d err %lu\n", h != INVALID_HANDLE_VALUE, GetLastError());
            DWORD w = 0;
            E(WriteFile(h, "abc", 3, &w, NULL));
            CloseHandle(h);
            E(GetFileAttributesA("cf2.txt"));
            E(cf2(L"cf2.txt", GENERIC_READ, 0, CREATE_NEW, NULL) == INVALID_HANDLE_VALUE);
            h = cf2(L"cf2.txt", GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING, NULL);
            char b[8] = { 0 };
            DWORD r = 0;
            E(ReadFile(h, b, 8, &r, NULL));
            printf("CreateFile2 read %lu %s\n", r, b);
            E(cf2(L"cf2.txt", GENERIC_WRITE, 0, OPEN_EXISTING, NULL) == INVALID_HANDLE_VALUE);
            CloseHandle(h);
            h = cf2(L"cf2.txt", GENERIC_WRITE, 0, OPEN_ALWAYS, NULL);
            printf("CreateFile2 OPEN_ALWAYS existing ok %d err %lu\n", h != INVALID_HANDLE_VALUE, GetLastError());
            CloseHandle(h);
            SetFileAttributesA("cf2.txt", FILE_ATTRIBUTE_NORMAL);
            E(DeleteFileA("cf2.txt"));
        }
    }
    SetCurrentDirectoryA("..");
    rmtree("wr-files");
    E(GetFileAttributesA("wr-files"));
    return 0;
}
