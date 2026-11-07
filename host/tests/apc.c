/* What Unity 6 / mono call and sake once lacked: user APCs (QueueUserAPC, run by alertable
 * waits, which then return WAIT_IO_COMPLETION), SetThreadErrorMode, and the display
 * configuration (GetDisplayConfigBufferSizes / QueryDisplayConfig / DisplayConfigGetDeviceInfo).
 * The expected lines are what Windows gives (one monitor). Built for x86 and x64.
 * Build: i686-w64-mingw32-gcc -O2 apc.c -o apc.exe */
#include <stdio.h>
#include <windows.h>

static char order[64];
static int n;
static void CALLBACK note(ULONG_PTR c)
{
    order[n++] = (char)c;
    order[n] = 0;
}
static void CALLBACK chain(ULONG_PTR c)
{
    order[n++] = (char)c;
    order[n] = 0;
    QueueUserAPC(note, GetCurrentThread(), 'z'); /* runs in the same wait */
}

static HANDLE waiter_ready, ev;
static volatile LONG got;
static DWORD WINAPI waiter(void *arg)
{
    SetEvent(waiter_ready);
    DWORD r = WaitForSingleObjectEx(ev, 10000, TRUE);
    printf("thread: alertable wait woken by an APC: %s, ran %d\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", (int)got);
    r = SleepEx(10000, TRUE);
    printf("thread: SleepEx woken by an APC: %s, ran %d\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", (int)got);
    return 0;
}
static void CALLBACK bump(ULONG_PTR c)
{
    InterlockedAdd(&got, (LONG)c);
}

typedef LONG (WINAPI *SIZES)(UINT32, UINT32 *, UINT32 *);
typedef LONG (WINAPI *QUERY)(UINT32, UINT32 *, void *, UINT32 *, void *, void *);
typedef LONG (WINAPI *INFO)(void *);

int main(void)
{
    HANDLE me = GetCurrentThread();
    DWORD r;
    printf("queue to self: %d\n", (int)QueueUserAPC(note, me, 'a'));
    QueueUserAPC(note, me, 'b');
    r = SleepEx(0, FALSE);
    printf("SleepEx not alertable: %lu, ran '%s'\n", r, order);
    r = SleepEx(0, TRUE);
    printf("SleepEx alertable: %s, ran '%s'\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", order);
    printf("SleepEx alertable, nothing queued: %lu\n", SleepEx(1, TRUE));

    HANDLE set = CreateEventA(0, TRUE, TRUE, 0);
    QueueUserAPC(chain, me, 'c');
    r = WaitForSingleObjectEx(set, 0, FALSE);
    printf("signaled, not alertable: %lu, ran '%s'\n", r, order);
    r = WaitForSingleObjectEx(set, 0, TRUE);
    printf("signaled, alertable with an APC queued: %s, ran '%s'\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", order);
    r = WaitForSingleObjectEx(set, 0, TRUE);
    printf("signaled, alertable, nothing queued: %lu\n", r);
    HANDLE two[2] = {CreateEventA(0, FALSE, FALSE, 0), set};
    QueueUserAPC(note, me, 'd');
    r = WaitForMultipleObjectsEx(2, two, FALSE, 0, TRUE);
    printf("wait-any, alertable: %s, ran '%s'\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", order);
    printf("wait-any after: %lu\n", WaitForMultipleObjectsEx(2, two, FALSE, 0, TRUE));

    QueueUserAPC(note, me, 'e');
    r = MsgWaitForMultipleObjectsEx(0, 0, 0, QS_ALLINPUT, 0);
    printf("MsgWait, not alertable: %lu, ran '%s'\n", r, order);
    r = MsgWaitForMultipleObjectsEx(0, 0, 0, QS_ALLINPUT, MWMO_ALERTABLE);
    printf("MsgWait, alertable: %s, ran '%s'\n", r == WAIT_IO_COMPLETION ? "WAIT_IO_COMPLETION" : "other", order);

    SetLastError(0);
    printf("queue to a bad handle: %d\n", (int)QueueUserAPC(note, (HANDLE)0x1234, 'x'));

    ev = CreateEventA(0, FALSE, FALSE, 0);
    waiter_ready = CreateEventA(0, FALSE, FALSE, 0);
    HANDLE t = CreateThread(0, 0, waiter, 0, 0, 0);
    WaitForSingleObject(waiter_ready, INFINITE);
    Sleep(200);
    QueueUserAPC(bump, t, 1);
    Sleep(200);
    QueueUserAPC(bump, t, 10);
    printf("thread ended: %lu\n", WaitForSingleObject(t, 20000));
    HANDLE o = OpenThread(THREAD_ALL_ACCESS, FALSE, GetCurrentThreadId());
    printf("OpenThread self: %d, same id %d\n", o != 0, GetThreadId(o) == GetCurrentThreadId());
    CloseHandle(o);
    o = OpenThread(SYNCHRONIZE, FALSE, GetThreadId(t));
    printf("OpenThread of the ended thread: %d, signaled %lu\n", o != 0, WaitForSingleObject(o, 0));
    CloseHandle(o);
    SetLastError(0);
    o = OpenThread(SYNCHRONIZE, FALSE, 0x7ffffff0);
    printf("OpenThread of no thread: %d, error %lu\n", o != 0, GetLastError());

    DWORD old = 77;
    BOOL ok = SetThreadErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX, &old);
    printf("SetThreadErrorMode: %d, old %lu\n", (int)ok, old);
    ok = SetThreadErrorMode(0, &old);
    printf("SetThreadErrorMode again: %d, old %lx\n", (int)ok, old);
    SetLastError(0);
    ok = SetThreadErrorMode(SEM_NOALIGNMENTFAULTEXCEPT, &old);
    printf("SetThreadErrorMode bad flags: %d, error %lu\n", (int)ok, GetLastError());
    printf("GetThreadErrorMode: %lu\n", GetThreadErrorMode());

    HMODULE u = LoadLibraryA("user32.dll");
    SIZES sizes = (SIZES)GetProcAddress(u, "GetDisplayConfigBufferSizes");
    QUERY query = (QUERY)GetProcAddress(u, "QueryDisplayConfig");
    INFO info = (INFO)GetProcAddress(u, "DisplayConfigGetDeviceInfo");
    UINT32 np = 99, nm = 99;
    LONG e = sizes(2 /* QDC_ONLY_ACTIVE_PATHS */, &np, &nm);
    printf("buffer sizes: %ld, %u paths %u modes\n", e, np, nm);
    printf("buffer sizes, no flags: %ld\n", sizes(0, &np, &nm));
    static unsigned char paths[4][72], modes[8][64];
    UINT32 small = 1;
    UINT32 one = 1;
    printf("query, too few modes: %ld\n", query(2, &one, paths, &small, modes, 0));
    np = 4, nm = 8;
    e = query(2, &np, paths, &nm, modes, 0);
    printf("query: %ld, %u paths %u modes\n", e, np, nm);
    UINT32 *p = (UINT32 *)paths[0];
    printf("path: source mode %u, target mode %u, refresh %u/%u, flags %x\n", p[3], p[8], p[12], p[13], p[17]);
    UINT32 *m0 = (UINT32 *)modes[p[3]], *m1 = (UINT32 *)modes[p[8]];
    printf("source mode: type %u, %ux%u, format %u, at %d,%d\n", m0[0], m0[4], m0[5], m0[6], (int)m0[7], (int)m0[8]);
    printf("target mode: type %u, active %ux%u = source %d, vsync %u/%u\n", m1[0], m1[10], m1[11], m1[10] == m0[4] && m1[11] == m0[5], m1[8], m1[9]);
    printf("same screen as GetSystemMetrics: %d\n", (int)m0[4] == GetSystemMetrics(SM_CXSCREEN) && (int)m0[5] == GetSystemMetrics(SM_CYSCREEN));

    struct { UINT32 type, size; LUID adapter; UINT32 id; WCHAR name[32]; } src = {1, sizeof(src)};
    src.adapter = *(LUID *)(p + 0);
    src.id = p[2];
    e = info(&src);
    char a[300];
    WideCharToMultiByte(CP_UTF8, 0, src.name, -1, a, sizeof(a), 0, 0);
    printf("source name: %ld '%s'\n", e, a);
    src.size = 20;
    printf("source name, short: %ld\n", info(&src));
    return 0;
}
