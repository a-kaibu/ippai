/* kernel32 synchronization and threads as games use them: events (auto/manual, Pulse), mutex
 * recursion, ownership errors and abandonment, semaphore counts and limits, WaitForMultiple
 * (which index wins, wait-all), named objects (ALREADY_EXISTS, type mismatch), waitable timers,
 * critical sections, Interlocked*, TLS, thread exit codes and suspend counts, DuplicateHandle,
 * the error codes of all of them. Only zero or short timeouts whose outcome does not depend on
 * scheduling. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-70s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)
#define W(h, t) E(WaitForSingleObject(h, t))

static HANDLE g_mutex;
static DWORD WINAPI take_mutex_and_exit(LPVOID p) {
    WaitForSingleObject(g_mutex, INFINITE);
    return 7; /* exits holding it: abandoned */
}
static DWORD WINAPI ret_arg(LPVOID p) { return (DWORD)(ULONG_PTR)p; }
static DWORD WINAPI wait_event(LPVOID p) { return WaitForSingleObject((HANDLE)p, INFINITE) + 100; }

int main(void) {
    /* events */
    HANDLE ea = CreateEventA(NULL, FALSE, FALSE, NULL), em = CreateEventA(NULL, TRUE, TRUE, NULL);
    W(ea, 0);
    E(SetEvent(ea));
    W(ea, 0);
    W(ea, 0); /* auto-reset: consumed */
    W(em, 0);
    W(em, 0); /* manual: stays */
    E(ResetEvent(em));
    W(em, 0);
    E(PulseEvent(em)); /* nobody waits: stays reset */
    W(em, 0);
    E(SetEvent(em));
    E(SetEvent(em));
    W(em, 0);

    /* WaitForMultipleObjects: lowest signaled index wins; wait-all; abandoned/timeouts */
    HANDLE e3[3] = { CreateEventA(NULL, TRUE, FALSE, NULL), CreateEventA(NULL, TRUE, TRUE, NULL), CreateEventA(NULL, TRUE, TRUE, NULL) };
    E(WaitForMultipleObjects(3, e3, FALSE, 0));
    E(WaitForMultipleObjects(3, e3, TRUE, 0));
    E(SetEvent(e3[0]));
    E(WaitForMultipleObjects(3, e3, FALSE, 0));
    E(WaitForMultipleObjects(3, e3, TRUE, 0));
    E(WaitForMultipleObjects(0, e3, FALSE, 0));
    E(WaitForMultipleObjects(65, e3, FALSE, 0));
    HANDLE dup2[2] = { e3[0], e3[0] };
    E(WaitForMultipleObjects(2, dup2, TRUE, 0)); /* the same handle twice in wait-all */
    HANDLE ae[2] = { CreateEventA(NULL, FALSE, TRUE, NULL), CreateEventA(NULL, FALSE, FALSE, NULL) };
    E(WaitForMultipleObjects(2, ae, TRUE, 0)); /* wait-all fails: the first must stay signaled */
    W(ae[0], 0);

    /* mutexes */
    HANDLE m = CreateMutexA(NULL, TRUE, NULL);
    W(m, 0);
    W(m, 0);
    E(ReleaseMutex(m));
    E(ReleaseMutex(m));
    E(ReleaseMutex(m));
    E(ReleaseMutex(m)); /* not owned anymore */
    E(ReleaseMutex(ea)); /* not a mutex */
    g_mutex = CreateMutexA(NULL, FALSE, NULL);
    HANDLE t = CreateThread(NULL, 0, take_mutex_and_exit, NULL, 0, NULL);
    W(t, INFINITE);
    DWORD code = 0;
    E(GetExitCodeThread(t, &code));
    printf("exit code %lu\n", code);
    W(g_mutex, 0); /* abandoned */
    W(g_mutex, 0); /* now ours, recursively */
    E(ReleaseMutex(g_mutex));
    E(ReleaseMutex(g_mutex));
    CloseHandle(t);

    /* semaphores */
    HANDLE s = CreateSemaphoreA(NULL, 1, 3, NULL);
    LONG prev = -1;
    E(ReleaseSemaphore(s, 1, &prev)); printf("prev %ld\n", prev);
    E(ReleaseSemaphore(s, 2, &prev)); printf("prev %ld\n", prev); /* over the maximum */
    E(ReleaseSemaphore(s, 0, &prev));
    E(ReleaseSemaphore(s, -1, &prev));
    W(s, 0); W(s, 0); W(s, 0);
    E(CreateSemaphoreA(NULL, 4, 3, NULL) != NULL);
    E(CreateSemaphoreA(NULL, -1, 3, NULL) != NULL);
    E(CreateSemaphoreA(NULL, 0, 0, NULL) != NULL);

    /* names */
    HANDLE n1 = CreateEventA(NULL, TRUE, FALSE, "masu-winref-ev");
    E(GetLastError());
    HANDLE n2 = CreateEventA(NULL, FALSE, TRUE, "masu-winref-ev"); /* opens the existing one: its flags stay */
    E(n2 != NULL);
    W(n2, 0);
    E(OpenEventA(EVENT_ALL_ACCESS, FALSE, "masu-winref-ev") != NULL);
    E(OpenEventA(EVENT_ALL_ACCESS, FALSE, "masu-winref-none") != NULL);
    E(CreateMutexA(NULL, FALSE, "masu-winref-ev") != NULL); /* another type */
    E(OpenMutexA(MUTEX_ALL_ACCESS, FALSE, "masu-winref-ev") != NULL);
    E(CreateSemaphoreA(NULL, 0, 1, "masu-winref-ev") != NULL);
    HANDLE nm = CreateMutexA(NULL, TRUE, "masu-winref-mx");
    E(CreateMutexA(NULL, TRUE, "masu-winref-mx") != NULL);
    E(GetLastError());
    E(CreateEventA(NULL, TRUE, FALSE, "Local\\masu-winref-ev2") != NULL);
    E(CreateEventA(NULL, TRUE, FALSE, "Global\\masu-winref-ev3") != NULL);
    E(CreateEventA(NULL, TRUE, FALSE, "bad\\name") != NULL);
    E(CreateEventA(NULL, TRUE, FALSE, "") != NULL);
    CloseHandle(n2); CloseHandle(nm);

    /* handles */
    E(CloseHandle(n1));
    E(CloseHandle(n1));
    E(CloseHandle(NULL));
    E(CloseHandle(INVALID_HANDLE_VALUE)); /* the current process pseudo handle */
    W(NULL, 0);
    W((HANDLE)(ULONG_PTR)0x12340, 0);
    HANDLE d = NULL;
    E(DuplicateHandle(GetCurrentProcess(), em, GetCurrentProcess(), &d, 0, FALSE, DUPLICATE_SAME_ACCESS));
    E(ResetEvent(em));
    W(d, 0);
    E(SetEvent(d));
    W(em, 0);
    E(DuplicateHandle(GetCurrentProcess(), em, GetCurrentProcess(), &d, 0, FALSE, DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE));
    E(SetEvent(em));
    W(d, 0);
    E(GetHandleInformation(d, &code)); printf("flags %lu\n", code);

    /* waitable timers (due in the past: signaled at once) */
    HANDLE wt = CreateWaitableTimerA(NULL, TRUE, NULL);
    W(wt, 0);
    LARGE_INTEGER due; due.QuadPart = -1;
    E(SetWaitableTimer(wt, &due, 0, NULL, NULL, FALSE));
    W(wt, 1000);
    W(wt, 0);
    E(CancelWaitableTimer(wt));
    W(wt, 0);
    HANDLE wa = CreateWaitableTimerA(NULL, FALSE, NULL);
    due.QuadPart = 1; /* absolute: 1601 */
    E(SetWaitableTimer(wa, &due, 0, NULL, NULL, FALSE));
    W(wa, 1000);
    W(wa, 0); /* synchronization timer: consumed */

    /* critical sections */
    CRITICAL_SECTION cs;
    InitializeCriticalSection(&cs);
    E(TryEnterCriticalSection(&cs));
    E(TryEnterCriticalSection(&cs));
    printf("recursion %ld lock %ld owner-is-us %d\n", cs.RecursionCount, cs.LockCount, cs.OwningThread == (HANDLE)(ULONG_PTR)GetCurrentThreadId());
    LeaveCriticalSection(&cs);
    LeaveCriticalSection(&cs);
    printf("recursion %ld lock %ld owner %p\n", cs.RecursionCount, cs.LockCount, cs.OwningThread);
    DeleteCriticalSection(&cs);
    E(InitializeCriticalSectionAndSpinCount(&cs, 4000));
    printf("spin %lu\n", (unsigned long)(cs.SpinCount & 0xffffff));
    E(SetCriticalSectionSpinCount(&cs, 10));
    DeleteCriticalSection(&cs);

    /* Interlocked */
    LONG v = 5;
    E(InterlockedIncrement(&v)); E(InterlockedDecrement(&v));
    E(InterlockedExchange(&v, 9)); E(InterlockedExchangeAdd(&v, -4));
    E(InterlockedCompareExchange(&v, 1, 4)); E(InterlockedCompareExchange(&v, 2, 5));
    printf("v %ld\n", v);
    v = 0x7fffffff;
    E(InterlockedIncrement(&v));
    LONGLONG q = 0x100000000LL;
    E(InterlockedCompareExchange64(&q, 7, 0x100000000LL));
    printf("q %lld\n", q);

    /* TLS */
    DWORD k = TlsAlloc();
    E(k != TLS_OUT_OF_INDEXES);
    E(TlsGetValue(k));
    E(TlsSetValue(k, (void *)0x55));
    E(TlsGetValue(k));
    E(TlsFree(k));
    E(TlsGetValue(1088));
    E(TlsSetValue(1088, NULL));
    E(TlsFree(1088));

    /* threads: exit codes, suspend counts, waiting on a thread's event */
    HANDLE tr = CreateThread(NULL, 0, ret_arg, (void *)42, CREATE_SUSPENDED, NULL);
    E(GetExitCodeThread(tr, &code)); printf("code %lu\n", code);
    E(SuspendThread(tr));
    E(ResumeThread(tr));
    E(ResumeThread(tr));
    E(ResumeThread(tr));
    W(tr, INFINITE);
    E(GetExitCodeThread(tr, &code)); printf("code %lu\n", code);
    E(ResumeThread(tr));
    CloseHandle(tr);
    HANDLE ge = CreateEventA(NULL, FALSE, FALSE, NULL);
    HANDLE tw = CreateThread(NULL, 0, wait_event, ge, 0, NULL);
    E(SetEvent(ge));
    W(tw, INFINITE);
    E(GetExitCodeThread(tw, &code)); printf("code %lu\n", code);
    E(GetThreadPriority(GetCurrentThread()));
    E(SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST));
    E(GetThreadPriority(GetCurrentThread()));
    E(SetThreadPriority(GetCurrentThread(), 99));
    E(SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL));
    E(GetCurrentThread() == (HANDLE)(LONG_PTR)-2);
    E(GetCurrentProcess() == (HANDLE)(LONG_PTR)-1);
    W(GetCurrentThread(), 0);
    E(SleepEx(0, TRUE));
    E(SwitchToThread() >= 0);
    return 0;
}
