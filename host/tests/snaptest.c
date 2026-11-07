/* Save states (docs/savestate.md): a program whose output does not depend on when a state is
 * taken. Its threads stand in every kind of wait a state stops them in (events, critical
 * sections, condition variables, Sleep, a thread created suspended), its main thread prints one
 * line per step. tests/snap.sh takes a state in the middle and checks that the run from it
 * prints the rest, the same as a run without one. Build: i686-w64-mingw32-gcc -O2 snaptest.c */
#include <stdio.h>
#include <stdlib.h>
#include <windows.h>

#define STEPS 60

static HANDLE go, done;
static CRITICAL_SECTION cs;
static CONDITION_VARIABLE cv;
static SRWLOCK srw = SRWLOCK_INIT;
static CONDITION_VARIABLE cv2;
static int queue[8], qn;     /* under cs */
static unsigned answer;      /* under cs */
static unsigned shared;      /* under srw */
static int quit;
static DWORD tls;
static volatile LONG ticks;  /* the sleeper's: not printed (it depends on time) */
static volatile LONG late_ran;
static unsigned *heap;

static unsigned mix(unsigned h, unsigned v)
{
    h ^= v;
    h *= 0x01000193u;
    return h ^ (h >> 13);
}

/* event ping-pong: computes over the heap block */
static DWORD WINAPI pinger(void *arg)
{
    TlsSetValue(tls, (void *)0x1234);
    for (;;)
    {
        WaitForSingleObject(go, INFINITE);
        if (quit) break;
        if (TlsGetValue(tls) != (void *)0x1234) printf("pinger: tls lost\n");
        unsigned h = 0;
        for (int i = 0; i < 4096; i++) h = mix(h, heap[i] += i);
        AcquireSRWLockExclusive(&srw);
        shared = mix(shared, h);
        ReleaseSRWLockExclusive(&srw);
        SetEvent(done);
    }
    return 0;
}

/* condition variable consumer */
static DWORD WINAPI consumer(void *arg)
{
    EnterCriticalSection(&cs);
    for (;;)
    {
        while (qn == 0 && !quit) SleepConditionVariableCS(&cv, &cs, INFINITE);
        if (qn == 0) break;
        int v = queue[--qn];
        answer = mix(answer, v * 7 + 1);
        WakeAllConditionVariable(&cv2);
    }
    LeaveCriticalSection(&cs);
    return 0;
}

/* sleeps, and waits with timeouts */
static DWORD WINAPI sleeper(void *arg)
{
    HANDLE never = CreateEventA(NULL, TRUE, FALSE, NULL);
    while (!quit)
    {
        Sleep(7);
        WaitForSingleObject(never, 3);
        InterlockedIncrement(&ticks);
    }
    return 0;
}

/* created suspended, started late */
static DWORD WINAPI late(void *arg)
{
    InterlockedExchange(&late_ran, (LONG)(INT_PTR)arg);
    return 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    tls = TlsAlloc();
    TlsSetValue(tls, (void *)0x5678);
    heap = calloc(4096, sizeof *heap);
    go = CreateEventA(NULL, FALSE, FALSE, NULL);
    done = CreateEventA(NULL, FALSE, FALSE, NULL);
    InitializeCriticalSection(&cs);
    InitializeConditionVariable(&cv);
    InitializeConditionVariable(&cv2);
    HANDLE th[3] = {
        CreateThread(NULL, 0, pinger, NULL, 0, NULL),
        CreateThread(NULL, 0, consumer, NULL, 0, NULL),
        CreateThread(NULL, 0, sleeper, NULL, 0, NULL),
    };
    HANDLE lt = CreateThread(NULL, 0, late, (void *)42, CREATE_SUSPENDED, NULL);
    DWORD t0 = GetTickCount(), prev = t0;
    LARGE_INTEGER f, c0, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c0);
    int jumps = 0;
    for (int k = 0; k < STEPS; k++)
    {
        SetEvent(go);
        WaitForSingleObject(done, INFINITE);
        EnterCriticalSection(&cs);
        queue[qn++] = k;
        queue[qn++] = k * k;
        WakeConditionVariable(&cv);
        while (qn) SleepConditionVariableCS(&cv2, &cs, INFINITE);
        unsigned a = answer;
        LeaveCriticalSection(&cs);
        AcquireSRWLockShared(&srw);
        unsigned s = shared;
        ReleaseSRWLockShared(&srw);
        if (k == 40) ResumeThread(lt);
        if (k == 45)
        {
            WaitForSingleObject(lt, INFINITE);
            printf("late thread: %ld\n", late_ran);
        }
        if (TlsGetValue(tls) != (void *)0x5678) printf("main: tls lost\n");
        printf("step %d: %08x %08x\n", k, a, s);
        Sleep(50);
        /* the clocks go on from where they were (a restored run's do not jump) */
        DWORD now = GetTickCount();
        if (now < prev || now - prev > 2000) jumps++;
        prev = now;
    }
    QueryPerformanceCounter(&c);
    double ms = (double)(c.QuadPart - c0.QuadPart) * 1000 / f.QuadPart;
    DWORD all = GetTickCount() - t0;
    printf("clocks: %s\n", jumps == 0 && all >= STEPS * 50 && ms >= STEPS * 50 && all < 60000 && ms < 60000 ? "ok" : "jumped");
    quit = 1;
    SetEvent(go);
    EnterCriticalSection(&cs);
    WakeAllConditionVariable(&cv);
    LeaveCriticalSection(&cs);
    WaitForMultipleObjects(3, th, TRUE, INFINITE);
    printf("sleeper: %s\n", ticks > 0 ? "ran" : "did not run");
    printf("end\n");
    return 0;
}
