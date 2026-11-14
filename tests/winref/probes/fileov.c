/* kernel32 ReadFile / WriteFile with an OVERLAPPED on a synchronous handle and on a
 * FILE_FLAG_OVERLAPPED one: IO at Offset whatever the file pointer says, where the pointer is
 * after, reads at and past the end (ERROR_HANDLE_EOF and the OVERLAPPED's status), writes past
 * the end, FILE_WRITE_TO_END_OF_FILE (all ones), FILE_USE_FILE_POINTER_POSITION, the event.
 * SQLite reads and writes its pages so; CisLugI 体験版's database was ruined while sake wrote
 * at the pointer (#154). Runs in "wr-fileov" under the current directory. */
#include "../probe.h"

static HANDLE ev;

static void ov_at(OVERLAPPED *o, DWORD lo, DWORD hi) {
    memset(o, 0, sizeof *o);
    o->Internal = 0x1234;
    o->InternalHigh = 0x5678;
    o->Offset = lo;
    o->OffsetHigh = hi;
    o->hEvent = ev;
    ResetEvent(ev);
}

static void show_ov(const char *what, BOOL r, DWORD n, OVERLAPPED *o) {
    printf("%-34s = %d err %lu n %lu status %lx info %lu event %lu\n", what, r, r ? 0 : GetLastError(), n,
           (unsigned long)o->Internal, (unsigned long)o->InternalHigh, WaitForSingleObject(ev, 0));
}

/* (a FILE_FLAG_OVERLAPPED handle's IO may finish later, ERROR_IO_PENDING: wait, so the lines
 * say how it ended whichever way Windows took) */
static BOOL settle(HANDLE h, BOOL r, DWORD *n, OVERLAPPED *o) {
    if (r || GetLastError() != ERROR_IO_PENDING) return r;
    return GetOverlappedResult(h, o, n, TRUE);
}

static DWORD pointer(HANDLE h) { return SetFilePointer(h, 0, NULL, FILE_CURRENT); }

static void wr(HANDLE h, const char *what, DWORD lo, DWORD hi, const char *s) {
    OVERLAPPED o;
    DWORD n = 77;
    ov_at(&o, lo, hi);
    SetLastError(0xdead);
    BOOL r = WriteFile(h, s, (DWORD)strlen(s), &n, &o);
    r = settle(h, r, &n, &o);
    show_ov(what, r, n, &o);
    printf("  pointer %lu size %lu\n", pointer(h), GetFileSize(h, NULL));
}

static void rd(HANDLE h, const char *what, DWORD lo, DWORD hi, DWORD len) {
    OVERLAPPED o;
    char b[32] = { 0 };
    DWORD n = 77;
    ov_at(&o, lo, hi);
    SetLastError(0xdead);
    BOOL r = ReadFile(h, b, len, &n, &o);
    r = settle(h, r, &n, &o);
    show_ov(what, r, n, &o);
    printf("  pointer %lu got \"%.*s\"\n", pointer(h), (int)(n <= sizeof b ? n : 0), b);
}

static void contents(HANDLE h) {
    char b[64];
    DWORD n = 0, i;
    SetFilePointer(h, 0, NULL, FILE_BEGIN);
    ReadFile(h, b, sizeof b, &n, NULL);
    printf("  contents %lu:", n);
    for (i = 0; i < n; i++) printf(b[i] >= 0x20 && b[i] < 0x7f ? "%c" : "<%02x>", (unsigned char)b[i]);
    printf("\n");
}

int main(void) {
    DWORD n;
    ev = CreateEventA(NULL, TRUE, FALSE, NULL);
    CreateDirectoryA("wr-fileov", NULL);
    SetCurrentDirectoryA("wr-fileov");

    printf("synchronous handle\n");
    HANDLE h = CreateFileA("sync.dat", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(h, "0123456789", 10, &n, NULL);
    SetFilePointer(h, 2, NULL, FILE_BEGIN);
    wr(h, "write AB at 5 (pointer 2)", 5, 0, "AB");
    rd(h, "read 3 at 1", 1, 0, 3);
    rd(h, "read 10 at 8", 8, 0, 10);
    rd(h, "read 3 at the end (10)", 10, 0, 3);
    rd(h, "read 3 past the end (100)", 100, 0, 3);
    rd(h, "read 0 past the end (100)", 100, 0, 0);
    wr(h, "write x at 12 (past the end)", 12, 0, "x");
    wr(h, "write yz at all ones", 0xffffffff, 0xffffffff, "yz");
    SetFilePointer(h, 0, NULL, FILE_BEGIN);
    wr(h, "write P at the pointer (0)", 0xfffffffe, 0xffffffff, "P");
    rd(h, "read 2 at the pointer (1)", 0xfffffffe, 0xffffffff, 2);
    contents(h);
    /* SQLite's way: pages written at their offsets, the pointer never moved by it */
    SetFilePointer(h, 0, NULL, FILE_BEGIN);
    wr(h, "page 2", 32, 0, "page-two");
    wr(h, "page 1", 16, 0, "page-one");
    wr(h, "page 0", 0, 0, "page-zero");
    rd(h, "page 2 back", 32, 0, 8);
    contents(h);
    CloseHandle(h);

    printf("FILE_FLAG_OVERLAPPED handle\n");
    h = CreateFileA("async.dat", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_FLAG_OVERLAPPED, NULL);
    {
        OVERLAPPED o;
        ov_at(&o, 0, 0);
        if (!WriteFile(h, "0123456789", 10, &n, &o) && GetLastError() == ERROR_IO_PENDING) GetOverlappedResult(h, &o, &n, TRUE);
    }
    SetFilePointer(h, 3, NULL, FILE_BEGIN);
    wr(h, "write AB at 5 (pointer 3)", 5, 0, "AB");
    rd(h, "read 3 at 1", 1, 0, 3);
    rd(h, "read 3 past the end (100)", 100, 0, 3);
    wr(h, "write yz at all ones", 0xffffffff, 0xffffffff, "yz");
    contents(h);
    CloseHandle(h);

    DeleteFileA("sync.dat");
    DeleteFileA("async.dat");
    SetCurrentDirectoryA("..");
    RemoveDirectoryA("wr-fileov");
    return 0;
}
