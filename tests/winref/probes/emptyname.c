/* Kernel objects created with an empty name ("", not NULL): Delphi's TEvent and many Delphi
 * timer components pass PChar('') for "no name". Windows makes each one a new unnamed object
 * (no ERROR_ALREADY_EXISTS, a SetEvent on one does not touch the other), as for NULL. sake used
 * to share one object under the name "", so 幻視同盟 evaluation_mini.exe's two timer threads
 * waited on the same auto-reset event and the SetEvent that should have restarted one woke the
 * other: the title never drew. Then the same for mutexes, semaphores, waitable timers and
 * pagefile sections, and OpenEvent("") / OpenMutex("") (nothing to open). */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-72s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static HANDLE ev[2];
static volatile LONG woke[2];

/* a timer thread as the Delphi engine has it: wait on its own event, count the wake-ups */
static DWORD WINAPI waiter(void *p) {
    int i = (int)(INT_PTR)p;
    for (;;) {
        DWORD r = WaitForSingleObject(ev[i], 5000);
        if (r != WAIT_OBJECT_0) return r;
        if (InterlockedIncrement(&woke[i]) >= 3) return 0;
    }
}

int main(void) {
    SetLastError(0xdead);
    HANDLE a = CreateEventA(NULL, FALSE, FALSE, "");
    printf("first event %d err %lu\n", a != NULL, GetLastError());
    SetLastError(0xdead);
    HANDLE b = CreateEventA(NULL, FALSE, FALSE, "");
    printf("second event %d err %lu other handle %d\n", b != NULL, GetLastError(), a != b);
    E(SetEvent(a));
    E(WaitForSingleObject(b, 0));
    E(WaitForSingleObject(a, 0));
    SetLastError(0xdead);
    HANDLE w = CreateEventW(NULL, TRUE, TRUE, L"");
    printf("wide event %d err %lu\n", w != NULL, GetLastError());
    E(WaitForSingleObject(b, 0));
    E(WaitForSingleObject(w, 0));
    E(OpenEventA(EVENT_ALL_ACCESS, FALSE, "") != NULL);
    E(OpenEventW(EVENT_ALL_ACCESS, FALSE, L"") != NULL);
    CloseHandle(w);

    /* two threads, each on its own "" event: three SetEvents on each wake only its own thread */
    ev[0] = a; ev[1] = b;
    HANDLE t[2] = { CreateThread(NULL, 0, waiter, (void *)0, 0, NULL), CreateThread(NULL, 0, waiter, (void *)1, 0, NULL) };
    for (int k = 0; k < 3; k++) {
        SetEvent(ev[1]);
        for (int n = 0; n < 200 && woke[1] <= k; n++) Sleep(5);
    }
    DWORD r1 = WaitForSingleObject(t[1], 5000);
    printf("thread 1 done %lu woke %ld, thread 0 woke %ld\n", r1, woke[1], woke[0]);
    for (int k = 0; k < 3; k++) {
        SetEvent(ev[0]);
        for (int n = 0; n < 200 && woke[0] <= k; n++) Sleep(5);
    }
    DWORD r0 = WaitForSingleObject(t[0], 5000);
    printf("thread 0 done %lu woke %ld\n", r0, woke[0]);
    CloseHandle(t[0]); CloseHandle(t[1]); CloseHandle(a); CloseHandle(b);

    /* the other kinds */
    SetLastError(0xdead);
    HANDLE m1 = CreateMutexA(NULL, TRUE, "");
    printf("mutex %d err %lu\n", m1 != NULL, GetLastError());
    SetLastError(0xdead);
    HANDLE m2 = CreateMutexA(NULL, TRUE, "");
    printf("second mutex %d err %lu\n", m2 != NULL, GetLastError());
    E(OpenMutexA(MUTEX_ALL_ACCESS, FALSE, "") != NULL);
    CloseHandle(m1); CloseHandle(m2);
    SetLastError(0xdead);
    HANDLE s1 = CreateSemaphoreA(NULL, 1, 1, "");
    HANDLE s2 = CreateSemaphoreA(NULL, 0, 1, "");
    printf("semaphores %d %d err %lu\n", s1 != NULL, s2 != NULL, GetLastError());
    E(WaitForSingleObject(s2, 0));
    E(WaitForSingleObject(s1, 0));
    CloseHandle(s1); CloseHandle(s2);
    SetLastError(0xdead);
    HANDLE wt1 = CreateWaitableTimerA(NULL, TRUE, "");
    HANDLE wt2 = CreateWaitableTimerA(NULL, TRUE, "");
    printf("timers %d %d err %lu\n", wt1 != NULL, wt2 != NULL, GetLastError());
    LARGE_INTEGER due = { .QuadPart = -10000 };
    printf("set timer %d\n", SetWaitableTimer(wt1, &due, 0, NULL, NULL, FALSE));
    E(WaitForSingleObject(wt1, 2000));
    E(WaitForSingleObject(wt2, 0));
    CloseHandle(wt1); CloseHandle(wt2);
    SetLastError(0xdead);
    HANDLE f1 = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x1000, "");
    printf("first section %d err %lu\n", f1 != NULL, GetLastError());
    SetLastError(0xdead);
    HANDLE f2 = CreateFileMappingA(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 0x1000, "");
    printf("second section %d err %lu\n", f2 != NULL, GetLastError());
    DWORD *v1 = MapViewOfFile(f1, FILE_MAP_ALL_ACCESS, 0, 0, 0), *v2 = MapViewOfFile(f2, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    if (v1 && v2) {
        v1[0] = 0x1234;
        printf("second view %lx\n", v2[0]);
    }
    E(OpenFileMappingA(FILE_MAP_ALL_ACCESS, FALSE, "") != NULL);
    return 0;
}
