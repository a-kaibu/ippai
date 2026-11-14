/* The process's thread pool (review B-4): work items, thread pool work/timers/waits, timer-queue
 * timers and registered waits run their callbacks on a few pool threads that are used again,
 * as Windows has them, not on a new thread each (sake started one guest thread per item: 1 MiB
 * of stack, a TEB, a Web Worker). Each part checks what its callbacks did and how many distinct
 * threads ran them; then a thread that can't get its stack is NULL + ERROR_NOT_ENOUGH_MEMORY
 * (sake ended the whole session there).
 * Build: i686-w64-mingw32-gcc -O2 threadpool.c -o threadpool.exe (also x86-64) */
#include <stdio.h>
#include <windows.h>

#define MAXT 4096
static DWORD tids[MAXT];
static volatile LONG ntids;
static CRITICAL_SECTION cs;

static void seen(void)
{
    DWORD me = GetCurrentThreadId();
    EnterCriticalSection(&cs);
    for (LONG i = 0; i < ntids; i++)
        if (tids[i] == me) {
            LeaveCriticalSection(&cs);
            return;
        }
    if (ntids < MAXT)
        tids[ntids++] = me;
    LeaveCriticalSection(&cs);
}

static void reset(void)
{
    ntids = 0;
}

static volatile LONG count;
static HANDLE all_done;
static LONG want;

static void hit(void)
{
    seen();
    if (InterlockedIncrement(&count) == want)
        SetEvent(all_done);
}

static void begin(LONG n)
{
    count = 0;
    want = n;
    ResetEvent(all_done);
    reset();
}

static DWORD CALLBACK item(void *p)
{
    hit();
    return 0;
}

static void CALLBACK simple(PTP_CALLBACK_INSTANCE i, void *p)
{
    hit();
}

static void CALLBACK work(PTP_CALLBACK_INSTANCE i, void *p, PTP_WORK w)
{
    hit();
}

static volatile LONG ticks;
static void CALLBACK tick(PTP_CALLBACK_INSTANCE i, void *p, PTP_TIMER t)
{
    seen();
    InterlockedIncrement(&ticks);
}

static void CALLBACK qtimer(void *p, BOOLEAN fired)
{
    if (fired)
        hit();
}

static volatile LONG timeouts;
static void CALLBACK waited(void *p, BOOLEAN timed_out)
{
    if (timed_out)
        InterlockedIncrement(&timeouts);
    hit();
}

static void CALLBACK tpwait(PTP_CALLBACK_INSTANCE i, void *p, PTP_WAIT w, TP_WAIT_RESULT r)
{
    if (r == WAIT_OBJECT_0)
        hit();
}

static int finish(DWORD ms)
{
    return WaitForSingleObject(all_done, ms) == WAIT_OBJECT_0 && count == want;
}

static DWORD CALLBACK nothing(void *p)
{
    return 0;
}

int main(void)
{
    InitializeCriticalSection(&cs);
    all_done = CreateEventA(NULL, TRUE, FALSE, NULL);

    /* 1000 work items: done, on a few threads */
    begin(1000);
    for (int i = 0; i < 1000; i++)
        if (!QueueUserWorkItem(item, NULL, 0))
            printf("QueueUserWorkItem failed\n");
    int ok = finish(20000);
    printf("QueueUserWorkItem: %d, few threads %d\n", ok, ntids <= 16);

    begin(500);
    for (int i = 0; i < 500; i++)
        TrySubmitThreadpoolCallback(simple, NULL, NULL);
    ok = finish(20000);
    printf("TrySubmitThreadpoolCallback: %d, few threads %d\n", ok, ntids <= 16);

    /* one work object submitted 300 times, waited for */
    begin(300);
    PTP_WORK w = CreateThreadpoolWork(work, NULL, NULL);
    for (int i = 0; i < 300; i++)
        SubmitThreadpoolWork(w);
    WaitForThreadpoolWorkCallbacks(w, FALSE);
    printf("SubmitThreadpoolWork: %ld after the wait, few threads %d\n", count, ntids <= 16);
    CloseThreadpoolWork(w);

    /* a periodic thread pool timer: runs, is set, stops when cancelled */
    reset();
    ticks = 0;
    PTP_TIMER t = CreateThreadpoolTimer(tick, NULL, NULL);
    FILETIME due;
    ULARGE_INTEGER rel;
    rel.QuadPart = (ULONGLONG)-(10 * 10000); /* 10 ms from now */
    due.dwLowDateTime = rel.LowPart;
    due.dwHighDateTime = rel.HighPart;
    SetThreadpoolTimer(t, &due, 20, 0);
    ok = IsThreadpoolTimerSet(t);
    printf("timer set: %d\n", ok);
    Sleep(400);
    SetThreadpoolTimer(t, NULL, 0, 0);
    WaitForThreadpoolTimerCallbacks(t, TRUE);
    LONG at_cancel = ticks;
    ok = IsThreadpoolTimerSet(t);
    printf("timer ticked: %d, set after cancel: %d\n", at_cancel >= 5, ok);
    Sleep(150);
    printf("timer still after cancel: %d\n", ticks - at_cancel > 1);
    CloseThreadpoolTimer(t);

    /* 200 one-shot timer-queue timers, then the queue deleted waiting for them */
    begin(200);
    HANDLE q = CreateTimerQueue();
    for (int i = 0; i < 200; i++) {
        HANDLE h;
        if (!CreateTimerQueueTimer(&h, q, qtimer, NULL, 5 + i % 20, 0, 0))
            printf("CreateTimerQueueTimer failed\n");
    }
    ok = finish(20000);
    printf("timer queue: %d, few threads %d\n", ok, ntids <= 16);
    ok = DeleteTimerQueueEx(q, INVALID_HANDLE_VALUE);
    printf("DeleteTimerQueueEx: %d\n", ok);

    /* a periodic default-queue timer, deleted waiting */
    begin(0x7fffffff);
    HANDLE pt;
    CreateTimerQueueTimer(&pt, NULL, qtimer, NULL, 10, 10, 0);
    Sleep(300);
    ok = DeleteTimerQueueTimer(NULL, pt, INVALID_HANDLE_VALUE);
    printf("DeleteTimerQueueTimer: %d\n", ok);
    LONG c0 = count;
    printf("periodic queue timer ran: %d\n", c0 >= 5);
    Sleep(100);
    printf("ran after its deletion: %d\n", count != c0);

    /* 40 registered waits on 40 events, once each; one more that times out */
    HANDLE ev[40], wh[41];
    begin(41);
    timeouts = 0;
    for (int i = 0; i < 40; i++) {
        ev[i] = CreateEventA(NULL, FALSE, FALSE, NULL);
        RegisterWaitForSingleObject(&wh[i], ev[i], waited, NULL, INFINITE, WT_EXECUTEONLYONCE);
    }
    HANDLE never = CreateEventA(NULL, FALSE, FALSE, NULL);
    RegisterWaitForSingleObject(&wh[40], never, waited, NULL, 50, WT_EXECUTEONLYONCE);
    for (int i = 39; i >= 0; i--)
        SetEvent(ev[i]);
    ok = finish(20000);
    printf("registered waits: %d, timed out %ld, few threads %d\n", ok, timeouts, ntids <= 16);
    for (int i = 0; i < 41; i++)
        UnregisterWaitEx(wh[i], INVALID_HANDLE_VALUE);

    /* a wait that goes on: the same event 10 times */
    begin(10);
    HANDLE again;
    RegisterWaitForSingleObject(&again, ev[0], waited, NULL, INFINITE, 0);
    for (int i = 0; i < 10; i++) {
        SetEvent(ev[0]);
        while (count <= i)
            Sleep(1);
    }
    ok = finish(5000);
    printf("repeated wait: %d\n", ok);
    UnregisterWaitEx(again, INVALID_HANDLE_VALUE);

    /* thread pool waits */
    begin(20);
    PTP_WAIT tw[20];
    for (int i = 0; i < 20; i++) {
        tw[i] = CreateThreadpoolWait(tpwait, NULL, NULL);
        SetThreadpoolWait(tw[i], ev[i], NULL);
    }
    for (int i = 0; i < 20; i++)
        SetEvent(ev[i]);
    ok = finish(20000);
    printf("thread pool waits: %d\n", ok);
    for (int i = 0; i < 20; i++)
        CloseThreadpoolWait(tw[i]);

    /* threads that can't get their stacks: NULL and ERROR_NOT_ENOUGH_MEMORY, and the process
     * goes on (256 MB each: the address space runs out) */
    HANDLE big[32];
    int made = 0, failed = 0;
    DWORD err = 0;
    for (int i = 0; i < 32; i++) {
        HANDLE h = CreateThread(NULL, 256 << 20, nothing, NULL, CREATE_SUSPENDED | STACK_SIZE_PARAM_IS_A_RESERVATION, NULL);
        if (!h) {
            err = GetLastError();
            failed = 1;
            break;
        }
        big[made++] = h;
    }
    printf("huge stacks: a thread failed %d with %lu, some made %d\n", failed, err, made > 0);
    for (int i = 0; i < made; i++) {
        ResumeThread(big[i]);
        WaitForSingleObject(big[i], INFINITE);
        CloseHandle(big[i]);
    }
    HANDLE h = CreateThread(NULL, 0, nothing, NULL, 0, NULL);
    ok = h != NULL && WaitForSingleObject(h, 5000) == WAIT_OBJECT_0;
    printf("a thread again: %d\n", ok);
    return 0;
}
