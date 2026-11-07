/* ntdll's NtQueryDirectoryFile, which .NET's Directory.GetFiles uses under mono (Unity 2021+):
 * the information classes' layouts, masks (taken on the first call or a restart), single
 * entries, short buffers and the end. Runs in a scratch directory "wr-ntdir". The order of
 * entries is the file system's: names are sorted before printing. */
#include "../probe.h"

typedef struct { USHORT Length, MaximumLength; PWSTR Buffer; } US;
typedef struct { union { LONG Status; void *p; }; ULONG_PTR Information; } IOSB;
typedef LONG (WINAPI *QDF)(HANDLE, HANDLE, void *, void *, IOSB *, void *, ULONG, int, BOOLEAN, US *, BOOLEAN);
static QDF qdf;

static void touch(const char *n, int size) {
    HANDLE h = CreateFileA(n, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    char b[100] = {0};
    DWORD w;
    WriteFile(h, b, size, &w, NULL);
    CloseHandle(h);
}
static HANDLE opendir(const char *d) {
    return CreateFileA(d, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
}

static char names[64][MAX_PATH];
static int nn;
static void sorted(const char *what) {
    for (int i = 0; i < nn; i++)
        for (int j = i + 1; j < nn; j++)
            if (_stricmp(names[j], names[i]) < 0) { char t[MAX_PATH]; strcpy(t, names[i]); strcpy(names[i], names[j]); strcpy(names[j], t); }
    printf("%s:", what);
    for (int i = 0; i < nn; i++) printf(" %s", names[i]);
    printf("\n");
    nn = 0;
}
/* the name at offset `at` of an entry, `len_at` its length */
static void take(const unsigned char *e, int len_at, int at) {
    ULONG n = *(ULONG *)(e + len_at);
    WideCharToMultiByte(CP_UTF8, 0, (WCHAR *)(e + at), n / 2, names[nn], MAX_PATH, 0, 0);
    names[nn][n / 2] = 0;
    nn++;
}

/* every entry of `dir` in class `cls`, with the fields shown for a.txt */
static void list(const char *dir, int cls, int len_at, int name_at, const WCHAR *mask) {
    HANDLE h = opendir(dir);
    static __declspec(align(8)) unsigned char buf[4096];
    IOSB io;
    US m = {0};
    if (mask) m.Length = m.MaximumLength = (USHORT)(wcslen(mask) * 2), m.Buffer = (PWSTR)mask;
    int calls = 0, aligned = 1;
    LONG st;
    for (;;) {
        memset(&io, 0x77, sizeof io);
        st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, cls, FALSE, mask ? &m : 0, FALSE);
        if (st) break;
        calls++;
        ULONG off = 0, last = 0;
        for (;;) {
            unsigned char *e = buf + off;
            take(e, len_at, name_at);
            if (cls != 12 && !strcmp(names[nn - 1], "a.txt"))
                printf("class %d a.txt: size %lld alloc>=size %d attrs %lx name length %lu times set %d\n", cls, *(LONGLONG *)(e + 40),
                       *(LONGLONG *)(e + 48) >= *(LONGLONG *)(e + 40), *(ULONG *)(e + 56), *(ULONG *)(e + len_at), *(LONGLONG *)(e + 24) != 0);
            if (cls != 12 && !strcmp(names[nn - 1], "sub"))
                printf("class %d sub: size %lld attrs %lx\n", cls, *(LONGLONG *)(e + 40), *(ULONG *)(e + 56));
            ULONG next = *(ULONG *)e;
            last = off;
            if (!next) break;
            aligned &= next % 8 == 0;
            off += next;
        }
        printf("class %d call: information is the end of the last entry %d\n", cls, io.Information == last + name_at + *(ULONG *)(buf + last + len_at));
    }
    printf("class %d end: %lx, iosb status %lx, calls %d, aligned %d\n", cls, (unsigned long)st, (unsigned long)io.Status, calls, aligned);
    char w[64];
    sprintf(w, "class %d%s%ls", cls, mask ? " mask " : "", mask ? mask : L"");
    sorted(w);
    CloseHandle(h);
}

int main(void) {
    qdf = (QDF)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryDirectoryFile");
    printf("NtQueryDirectoryFile: %d\n", qdf != 0);
    CreateDirectoryA("wr-ntdir", NULL);
    SetCurrentDirectoryA("wr-ntdir");
    touch("a.txt", 5);
    touch("b.dat", 0);
    touch("Long File Name.text", 70);
    CreateDirectoryA("sub", NULL);

    list(".", 1, 60, 64, 0);
    list(".", 2, 60, 68, 0);
    list(".", 3, 60, 94, 0);
    list(".", 12, 8, 12, 0);
    list(".", 37, 60, 104, 0);
    list(".", 38, 60, 80, 0);
    /* the 128-bit and 64-bit file id classes (Ext, Both with the short name) */
    list(".", 60, 60, 88, 0);
    list(".", 63, 60, 114, 0);
    list(".", 78, 60, 80, 0);
    list(".", 79, 60, 106, 0);
    list(".", 80, 60, 96, 0);
    list(".", 81, 60, 122, 0);
    list(".", 1, 60, 64, L"*.txt");
    list(".", 1, 60, 64, L"b.dat");
    list(".", 1, 60, 64, L"?.*");

    HANDLE h = opendir(".");
    static __declspec(align(8)) unsigned char buf[4096];
    IOSB io;
    US m = {10, 10, L"*.txt"};
    LONG st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, TRUE, &m, FALSE);
    take(buf, 60, 64);
    printf("single: %lx, next offset %lu, information %d\n", (unsigned long)st, *(ULONG *)buf, io.Information == 64 + *(ULONG *)(buf + 60));
    US other = {10, 10, L"*.dat"};
    while (!(st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, TRUE, &other, FALSE)))
        take(buf, 60, 64);
    printf("later mask ignored, end %lx\n", (unsigned long)st);
    sorted("first mask kept");
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, &other, TRUE);
    for (ULONG off = 0;; off += *(ULONG *)(buf + off)) {
        take(buf + off, 60, 64);
        if (!*(ULONG *)(buf + off)) break;
    }
    printf("restart with a new mask: %lx\n", (unsigned long)st);
    sorted("restart");
    CloseHandle(h);

    h = opendir(".");
    st = qdf(h, 0, 0, 0, &io, buf, 63, 1, FALSE, 0, FALSE);
    printf("shorter than the fixed part: %lx\n", (unsigned long)st);
    US lm = {38, 38, L"Long File Name.text"};
    memset(&io, 0x77, sizeof io);
    memset(buf, 0x55, 200);
    st = qdf(h, 0, 0, 0, &io, buf, 70, 1, FALSE, &lm, FALSE);
    printf("name cut: %lx, iosb %lx information %lu, name length %lu, 'Lon' %d, past the buffer untouched %d\n", (unsigned long)st, (unsigned long)io.Status,
           (unsigned long)io.Information, *(ULONG *)(buf + 60), !memcmp(buf + 64, L"Lon", 6), buf[70] == 0x55);
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, &lm, FALSE);
    printf("then with room: %lx, length %lu\n", (unsigned long)st, *(ULONG *)(buf + 60));
    CloseHandle(h);

    h = opendir(".");
    US none = {10, 10, L"*.zzz"};
    memset(&io, 0x77, sizeof io);
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, &none, FALSE);
    printf("nothing matches: %lx, iosb %lx\n", (unsigned long)st, (unsigned long)io.Status);
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, &none, FALSE);
    printf("again: %lx\n", (unsigned long)st);
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 99, FALSE, 0, FALSE);
    printf("bad class: %lx\n", (unsigned long)st);
    /* classes from FileMaximumInformation (84 in Wine's headers) on, and below it */
    for (int k = 76; k <= 100; k += 4) {
        st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, k, FALSE, 0, FALSE);
        printf("class %d: %lx\n", k, (unsigned long)st);
    }
    CloseHandle(h);
    /* nothing matches on a handle's first call: which ways the IO_STATUS_BLOCK gets the status
     * (single entry, restart scan; classes 1 and 3) */
    for (int k = 0; k < 8; k++) {
        h = opendir(".");
        memset(&io, 0x77, sizeof io);
        st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, k & 4 ? 3 : 1, k & 1, &none, (k & 2) != 0);
        printf("nothing, class %d single %d restart %d: %lx, iosb %lx\n", k & 4 ? 3 : 1, k & 1, (k & 2) != 0, (unsigned long)st, (unsigned long)io.Status);
        CloseHandle(h);
    }
    h = opendir(".");
    CloseHandle(h);
    h = CreateFileA("a.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    st = qdf(h, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, 0, FALSE);
    printf("a file, not a directory: %lx\n", (unsigned long)st);
    CloseHandle(h);
    st = qdf((HANDLE)0x1234, 0, 0, 0, &io, buf, sizeof buf, 1, FALSE, 0, FALSE);
    printf("bad handle: %lx\n", (unsigned long)st);

    DeleteFileA("a.txt");
    DeleteFileA("b.dat");
    DeleteFileA("Long File Name.text");
    RemoveDirectoryA("sub");
    SetCurrentDirectoryA("..");
    RemoveDirectoryA("wr-ntdir");
    return 0;
}
