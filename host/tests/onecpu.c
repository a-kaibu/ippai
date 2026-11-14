/* A process that pins itself to one processor (SetProcessAffinityMask(.., 1)) and then shares
 * a buffer between threads without a lock, as 二代目は☆魔法少女 (AIMS, #199) unpacks LZSS
 * into one global ring buffer from its loader thread and its main thread: on one processor
 * the threads' code between their calls never overlaps (a switch comes only at the end of a
 * quantum or in a call that waits). Also: a thread that spins on a flag without calls still
 * lets the thread that sets it run, and a waiting thread wakes. Build: i686-w64-mingw32-gcc -O2 onecpu.c -o onecpu.exe */
#include <stdio.h>
#include <windows.h>

#define ROUNDS 3000
#define SIZE 512

static volatile unsigned char ring[SIZE];
static volatile LONG overlaps, go;
static volatile int flag;

static DWORD WINAPI unpack(void *arg)
{
    unsigned char id = (unsigned char)(INT_PTR)arg;
    while (!go) Sleep(0);
    for (int r = 0; r < ROUNDS; r++) {
        for (int i = 0; i < SIZE; i++) ring[i] = id;
        for (int i = 0; i < SIZE; i++)
            if (ring[i] != id) { InterlockedIncrement(&overlaps); break; }
        if (r % 8 == 0) GetTickCount(); /* a call between files */
    }
    return 0;
}

static DWORD WINAPI setter(void *arg)
{
    (void)arg;
    Sleep(20);
    flag = 1;
    return 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    DWORD_PTR proc = 0, sys = 0;
    printf("pinned: %d\n", SetProcessAffinityMask(GetCurrentProcess(), 1) != 0);
    GetProcessAffinityMask(GetCurrentProcess(), &proc, &sys);
    printf("process mask: %lu\n", (unsigned long)proc);
    HANDLE t[3];
    for (int k = 0; k < 3; k++) t[k] = CreateThread(NULL, 0, unpack, (void *)(INT_PTR)(k + 1), 0, NULL);
    go = 1;
    WaitForMultipleObjects(3, t, TRUE, INFINITE);
    for (int k = 0; k < 3; k++) CloseHandle(t[k]);
    printf("3 threads x %d rounds: overlaps %ld\n", ROUNDS, overlaps);
    /* a spin without calls: the setter still runs */
    HANDLE s = CreateThread(NULL, 0, setter, NULL, 0, NULL);
    while (!flag) ;
    printf("the spinner saw the flag\n");
    printf("setter ended: %lu\n", WaitForSingleObject(s, 5000));
    CloseHandle(s);
    return 0;
}
