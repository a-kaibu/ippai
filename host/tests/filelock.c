/* Byte-range locks: LockFile / UnlockFile and LockFileEx / UnlockFileEx (shared, exclusive,
 * LOCKFILE_FAIL_IMMEDIATELY) between two handles of one file, a handle's own overlapping
 * locks, zero-length and past-the-end ranges, exact-range unlocking (ERROR_NOT_LOCKED),
 * locks released by CloseHandle, and a waiting LockFileEx that another thread's unlock lets
 * through. (CisLugI's trial stopped after LockFileEx failed; sake: kura::filelock, k32.rs
 * lock_file; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static char path[MAX_PATH];

static void say(const char *what, BOOL ok) {
    printf("%-40s %d", what, ok);
    if (!ok) printf(" error %lu", GetLastError());
    printf("\n");
}

static BOOL ex(HANDLE h, DWORD flags, DWORD off, DWORD len) {
    OVERLAPPED o = {0};
    o.Offset = off;
    return LockFileEx(h, flags, 0, len, 0, &o);
}
static BOOL unex(HANDLE h, DWORD off, DWORD len) {
    OVERLAPPED o = {0};
    o.Offset = off;
    return UnlockFileEx(h, 0, len, 0, &o);
}

static HANDLE waiter_h;
static volatile LONG waited;
static DWORD WINAPI waiter(void *p) {
    (void)p;
    BOOL ok = ex(waiter_h, LOCKFILE_EXCLUSIVE_LOCK, 100, 10);
    InterlockedExchange(&waited, ok ? 1 : 2);
    return 0;
}

int main(void) {
    GetTempPathA(sizeof path, path);
    strcat(path, "filelock.tmp");
    DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    HANDLE a = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, share, NULL, CREATE_ALWAYS, 0, NULL);
    HANDLE b = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, share, NULL, OPEN_EXISTING, 0, NULL);
    WriteFile(a, "0123456789abcdef", 16, &(DWORD){0}, NULL);
    printf("open: %d %d\n", a != INVALID_HANDLE_VALUE, b != INVALID_HANDLE_VALUE);
    DWORD now = LOCKFILE_FAIL_IMMEDIATELY, excl = LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY;

    say("a LockFile 0+10", LockFile(a, 0, 0, 10, 0));
    say("a LockFile 5+1 again", LockFile(a, 5, 0, 1, 0));
    say("b LockFile 9+5", LockFile(b, 9, 0, 5, 0));
    say("b LockFile 10+5 (next byte)", LockFile(b, 10, 0, 5, 0));
    say("b shared 0+1 over a's exclusive", ex(b, now, 0, 1));
    say("a shared 0+10 inside its own", ex(a, now, 0, 10));
    say("b LockFile 3+0 (zero bytes)", LockFile(b, 3, 0, 0, 0));
    say("a UnlockFile 0+9 (not the range)", UnlockFile(a, 0, 0, 9, 0));
    say("a UnlockFile 0+10 (the exclusive)", UnlockFile(a, 0, 0, 10, 0));
    say("b exclusive 0+1 (a's shared left)", ex(b, excl, 0, 1));
    say("b shared 0+1 beside it", ex(b, now, 0, 1));
    say("a UnlockFileEx 0+10 (the shared)", unex(a, 0, 10));
    say("a UnlockFileEx 0+10 again", unex(a, 0, 10));
    say("b UnlockFileEx 0+1", unex(b, 0, 1));
    say("a exclusive past the end 1000+10", ex(a, excl, 1000, 10));
    say("b shared 1005+1", ex(b, now, 1005, 1));
    say("b UnlockFile 10+5", UnlockFile(b, 10, 0, 5, 0));
    say("b UnlockFile 3+0", UnlockFile(b, 3, 0, 0, 0));
    /* closing a releases its locks */
    CloseHandle(a);
    say("b exclusive 1000+10 after a closed", ex(b, excl, 1000, 10));
    say("b UnlockFile 1000+10", UnlockFile(b, 1000, 0, 10, 0));
    /* a waiting lock: b holds 100+10; another handle waits until b unlocks */
    say("b exclusive 100+10", ex(b, excl, 100, 10));
    waiter_h = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, share, NULL, OPEN_EXISTING, 0, NULL);
    HANDLE t = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    Sleep(200);
    printf("%-40s %ld\n", "waiter still waiting", waited);
    say("b UnlockFileEx 100+10", unex(b, 100, 10));
    WaitForSingleObject(t, 5000);
    printf("%-40s %ld\n", "waiter got it (1)", waited);
    say("b exclusive 100+10 now", ex(b, excl, 100, 10));
    CloseHandle(t);
    CloseHandle(waiter_h);
    CloseHandle(b);
    DeleteFileA(path);
    return 0;
}
