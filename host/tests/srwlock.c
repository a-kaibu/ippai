/* Slim reader/writer locks and SleepConditionVariableSRW: try-acquire semantics, exclusive
 * counting under contention, readers seeing writers' paired updates whole, a blocked
 * acquirer woken by the release (promptly: not by a poll timeout), and a producer/consumer
 * queue on a condition variable. The output is the same on Windows (winref runs this too).
 * Build: i686-w64-mingw32-gcc -O2 srwlock.c -o srwlock.exe */
#include <stdio.h>
#include <windows.h>

static SRWLOCK lock = SRWLOCK_INIT;
static volatile LONG a, b, bad, stop;
static long counter;

static double now(void)
{
    LARGE_INTEGER c, f;
    QueryPerformanceCounter(&c);
    QueryPerformanceFrequency(&f);
    return (double)c.QuadPart * 1000 / f.QuadPart;
}

static DWORD WINAPI adder(void *arg)
{
    for (int i = 0; i < 50000; i++)
    {
        AcquireSRWLockExclusive(&lock);
        long v = counter;
        counter = v + 1;
        ReleaseSRWLockExclusive(&lock);
    }
    return 0;
}

static DWORD WINAPI writer(void *arg)
{
    for (int i = 0; i < 20000; i++)
    {
        AcquireSRWLockExclusive(&lock);
        a++;
        if (i % 64 == 0) SwitchToThread();
        b++;
        ReleaseSRWLockExclusive(&lock);
    }
    return 0;
}

static DWORD WINAPI reader(void *arg)
{
    while (!stop)
    {
        AcquireSRWLockShared(&lock);
        if (a != b) InterlockedIncrement(&bad);
        ReleaseSRWLockShared(&lock);
    }
    return 0;
}

static volatile double released, woke;
static volatile LONG ready;

static DWORD WINAPI blocked(void *arg)
{
    ready = 1;
    AcquireSRWLockExclusive(&lock);
    woke = now();
    ReleaseSRWLockExclusive(&lock);
    return 0;
}

#define QN 16
static CONDITION_VARIABLE not_empty = CONDITION_VARIABLE_INIT, not_full = CONDITION_VARIABLE_INIT;
static int queue[QN], qhead, qcount;
static volatile LONG consumed_sum, consumed_n;

static DWORD WINAPI producer(void *arg)
{
    int base = (int)(INT_PTR)arg;
    for (int i = 1; i <= 2000; i++)
    {
        AcquireSRWLockExclusive(&lock);
        while (qcount == QN) SleepConditionVariableSRW(&not_full, &lock, INFINITE, 0);
        queue[(qhead + qcount++) % QN] = base + i;
        ReleaseSRWLockExclusive(&lock);
        WakeConditionVariable(&not_empty);
    }
    return 0;
}

static DWORD WINAPI consumer(void *arg)
{
    for (;;)
    {
        AcquireSRWLockExclusive(&lock);
        while (qcount == 0 && !stop) SleepConditionVariableSRW(&not_empty, &lock, INFINITE, 0);
        if (qcount == 0)
        {
            ReleaseSRWLockExclusive(&lock);
            return 0;
        }
        int v = queue[qhead];
        qhead = (qhead + 1) % QN;
        qcount--;
        ReleaseSRWLockExclusive(&lock);
        WakeConditionVariable(&not_full);
        InterlockedExchangeAdd(&consumed_sum, v);
        InterlockedIncrement(&consumed_n);
    }
}

int main(void)
{
    HANDLE t[8];
    setvbuf(stdout, NULL, _IONBF, 0);

    /* try-acquire on each state */
    printf("try exclusive, free: %d\n", TryAcquireSRWLockExclusive(&lock));
    printf("try exclusive, held exclusive: %d\n", TryAcquireSRWLockExclusive(&lock));
    printf("try shared, held exclusive: %d\n", TryAcquireSRWLockShared(&lock));
    ReleaseSRWLockExclusive(&lock);
    printf("try shared, free: %d\n", TryAcquireSRWLockShared(&lock));
    printf("try shared, held shared: %d\n", TryAcquireSRWLockShared(&lock));
    printf("try exclusive, held shared: %d\n", TryAcquireSRWLockExclusive(&lock));
    ReleaseSRWLockShared(&lock);
    printf("try exclusive, held shared once: %d\n", TryAcquireSRWLockExclusive(&lock));
    ReleaseSRWLockShared(&lock);
    printf("try exclusive, released: %d\n", TryAcquireSRWLockExclusive(&lock));
    ReleaseSRWLockExclusive(&lock);

    /* exclusive increments from 6 threads */
    for (int i = 0; i < 6; i++) t[i] = CreateThread(NULL, 0, adder, NULL, 0, NULL);
    WaitForMultipleObjects(6, t, TRUE, INFINITE);
    for (int i = 0; i < 6; i++) CloseHandle(t[i]);
    printf("exclusive counter: %ld (want 300000)\n", counter);

    /* readers never see a half-done write */
    stop = 0;
    for (int i = 0; i < 3; i++) t[i] = CreateThread(NULL, 0, writer, NULL, 0, NULL);
    for (int i = 3; i < 6; i++) t[i] = CreateThread(NULL, 0, reader, NULL, 0, NULL);
    WaitForMultipleObjects(3, t, TRUE, INFINITE);
    stop = 1;
    WaitForMultipleObjects(3, t + 3, TRUE, INFINITE);
    for (int i = 0; i < 6; i++) CloseHandle(t[i]);
    printf("writes %ld/%ld, torn reads %ld\n", a, b, bad);

    /* a blocked acquirer is woken by the release, not a poll: the worst of 20 handoffs */
    double worst = 0;
    for (int i = 0; i < 20; i++)
    {
        AcquireSRWLockExclusive(&lock);
        woke = 0;
        ready = 0;
        HANDLE h = CreateThread(NULL, 0, blocked, NULL, 0, NULL);
        while (!ready) Sleep(1);
        Sleep(30);
        released = now();
        ReleaseSRWLockExclusive(&lock);
        WaitForSingleObject(h, INFINITE);
        CloseHandle(h);
        if (woke - released > worst) worst = woke - released;
    }
    printf("blocked acquirer woken within 15 ms: %s\n", worst < 15 ? "yes" : "no");

    /* producer/consumer on SleepConditionVariableSRW */
    stop = 0;
    for (int i = 0; i < 3; i++) t[i] = CreateThread(NULL, 0, producer, (void *)(INT_PTR)(i * 100000), 0, NULL);
    for (int i = 3; i < 6; i++) t[i] = CreateThread(NULL, 0, consumer, NULL, 0, NULL);
    WaitForMultipleObjects(3, t, TRUE, INFINITE);
    AcquireSRWLockExclusive(&lock);
    stop = 1;
    ReleaseSRWLockExclusive(&lock);
    WakeAllConditionVariable(&not_empty);
    WaitForMultipleObjects(3, t + 3, TRUE, INFINITE);
    for (int i = 0; i < 6; i++) CloseHandle(t[i]);
    /* (sum of i + base over i = 1..2000, base 0, 100000, 200000) */
    printf("consumed %ld items, sum %ld (want 6000, %ld)\n", consumed_n, consumed_sum, 3L * 2000 * 2001 / 2 + 2000L * 300000);
    return 0;
}
