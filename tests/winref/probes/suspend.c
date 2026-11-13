/* SuspendThread / ResumeThread of a running thread by another one, as a Delphi engine's
 * multi-core renderer does (TThread.Suspend from the main thread, Resume every frame): the
 * suspend counts each call returns, that a suspended thread does not run (a spinning one and one
 * in a Sleep loop), that it goes on once its count is 0, a thread suspending itself, the limit
 * (MAXIMUM_SUSPEND_COUNT, 127) and a thread that ended. Only facts that do not depend on
 * scheduling: SuspendThread is asynchronous, so the probe waits (Sleep) before it looks. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-60s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static volatile LONG spins, sleeps, self_runs, stop;

static DWORD WINAPI spinner(LPVOID p) {
    while (!stop) InterlockedIncrement(&spins);
    return 1;
}
static DWORD WINAPI sleeper(LPVOID p) {
    while (!stop) { InterlockedIncrement(&sleeps); Sleep(1); }
    return 2;
}
static DWORD WINAPI self_suspender(LPVOID p) {
    InterlockedIncrement(&self_runs);
    DWORD r = SuspendThread(GetCurrentThread());
    InterlockedIncrement(&self_runs);
    return 100 + r;
}

/* whether the counter moves in 100 ms (after 50 ms for a SuspendThread to take effect) */
static int moves(volatile LONG *c) {
    Sleep(50);
    LONG a = *c;
    Sleep(100);
    return *c != a;
}

int main(void) {
    DWORD code;
    HANDLE t = CreateThread(NULL, 0, spinner, NULL, 0, NULL);
    printf("spinner runs: %d\n", moves(&spins));
    E(SuspendThread(t));
    printf("suspended: moves %d\n", moves(&spins));
    E(SuspendThread(t));
    E(ResumeThread(t));
    printf("count 1: moves %d\n", moves(&spins));
    E(ResumeThread(t));
    printf("resumed: moves %d\n", moves(&spins));
    E(ResumeThread(t)); /* not suspended */
    /* the Delphi pattern: created suspended, Resume, Suspend from outside, Resume */
    HANDLE d = CreateThread(NULL, 0, spinner, NULL, CREATE_SUSPENDED, NULL);
    E(ResumeThread(d));
    E(SuspendThread(d));
    E(SuspendThread(t));
    printf("both suspended: moves %d\n", moves(&spins));
    E(ResumeThread(d));
    printf("one resumed: moves %d\n", moves(&spins));
    E(ResumeThread(t));

    /* a thread in a Sleep loop stops too */
    HANDLE s = CreateThread(NULL, 0, sleeper, NULL, 0, NULL);
    printf("sleeper runs: %d\n", moves(&sleeps));
    E(SuspendThread(s));
    printf("sleeper suspended: moves %d\n", moves(&sleeps));
    E(ResumeThread(s));
    printf("sleeper resumed: moves %d\n", moves(&sleeps));

    /* the limit */
    HANDLE l = CreateThread(NULL, 0, spinner, NULL, CREATE_SUSPENDED, NULL);
    int n = 0;
    while (SuspendThread(l) != (DWORD)-1 && n < 200) n++;
    printf("suspends until the limit: %d err %lu\n", n, GetLastError());
    E(ResumeThread(l));
    for (int i = 0; i < n; i++) ResumeThread(l);
    E(ResumeThread(l)); /* 0: the creation's count was one of them */
    E(ResumeThread(l));

    /* a thread that suspends itself waits for ResumeThread */
    HANDLE m = CreateThread(NULL, 0, self_suspender, NULL, 0, NULL);
    Sleep(100);
    printf("self-suspended after %ld\n", self_runs);
    E(ResumeThread(m));
    E(WaitForSingleObject(m, 5000));
    E(GetExitCodeThread(m, &code)); printf("code %lu, runs %ld\n", code, self_runs);

    stop = 1;
    E(WaitForSingleObject(t, 5000));
    E(WaitForSingleObject(d, 5000));
    E(WaitForSingleObject(s, 5000));
    E(WaitForSingleObject(l, 5000));
    E(GetExitCodeThread(t, &code)); printf("code %lu\n", code);
    /* a thread that ended */
    E(SuspendThread(t));
    E(ResumeThread(t));
    E(SuspendThread((HANDLE)0x1234));
    E(ResumeThread((HANDLE)0x1234));
    return 0;
}
