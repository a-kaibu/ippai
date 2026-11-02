#include <windows.h>
#include <stdio.h>
#define Q 4096
static unsigned char *ring[Q]; static int ringsz[Q];
static volatile LONG head, tail, bad, done_producers, ops;
static SRWLOCK lock = SRWLOCK_INIT;
static DWORD WINAPI producer(void *arg) {
    unsigned x = (unsigned)(ULONG_PTR)arg * 2654435761u;
    for (int k = 0; k < 40000; k++) {
        x = x * 1103515245 + 12345;
        int sz = 8 + ((x >> 16) % 200);
        unsigned char *p = HeapAlloc(GetProcessHeap(), 0, sz);
        for (int j = 0; j < sz; j++) p[j] = (unsigned char)(sz + j);
        for (;;) {
            AcquireSRWLockExclusive(&lock);
            if (head - tail < Q) { ring[head % Q] = p; ringsz[head % Q] = sz; head++; ReleaseSRWLockExclusive(&lock); break; }
            ReleaseSRWLockExclusive(&lock); Sleep(0);
        }
    }
    InterlockedIncrement(&done_producers);
    return 0;
}
static DWORD WINAPI consumer(void *arg) {
    for (;;) {
        unsigned char *p = NULL; int sz = 0;
        AcquireSRWLockExclusive(&lock);
        if (tail < head) { p = ring[tail % Q]; sz = ringsz[tail % Q]; tail++; }
        ReleaseSRWLockExclusive(&lock);
        if (!p) { if (done_producers == 3 && tail == head) return 0; Sleep(0); continue; }
        for (int j = 0; j < sz; j++) if (p[j] != (unsigned char)(sz + j)) { InterlockedIncrement(&bad); break; }
        HeapFree(GetProcessHeap(), 0, p);
        InterlockedIncrement(&ops);
    }
}
int main(void) {
    HANDLE t[6];
    for (int i = 0; i < 3; i++) t[i] = CreateThread(NULL, 0, producer, (void *)(ULONG_PTR)(i + 1), 0, NULL);
    for (int i = 3; i < 6; i++) t[i] = CreateThread(NULL, 0, consumer, NULL, 0, NULL);
    WaitForMultipleObjects(6, t, TRUE, INFINITE);
    printf("heap2: ops %ld, corrupted %ld\n", ops, bad);
    return 0;
}
