/* What a thread stores right after SetEvent is there when the thread it woke runs (EntisGLS'
 * window thread, #158: `SetEvent(done); this->signaled = 1;` into an object on the poster's
 * stack). On Windows the signaler runs on while the waiter is readied; sake lets a woken
 * waiter start once its signaler has made its next API call (crates/waits handoff.rs, at most
 * 1 s for a signaler that makes no progress; running on counts). The signaler here spins a little between SetEvent and its store, with no API call:
 * every round the waiter must see the store (after a first round that translates the code: a
 * translation can take longer than the cap). (sake only: on Windows a spin this long is
 * longer than the waiter's wakeup, so this is not a winref probe.)
 * Then a signaler that spins on without API calls until its waiter has run: the waiter must
 * not be held for the cap (kura's run loop counts the spin as progress), as on Windows where
 * it wakes at once.
 */
#include <windows.h>
#include <stdio.h>

static HANDLE ev, ready;
static volatile LONG flag;
static volatile LONG sink;
static int seen;
static volatile LONG woke;
static LARGE_INTEGER t0, t1;

static DWORD WINAPI waiter(void *p) {
    for (int i = 0; i <= 40; i++) {
        SetEvent(ready);
        WaitForSingleObject(ev, INFINITE);
        seen += i > 0 && flag == i + 1;
    }
    return 0;
}

static DWORD WINAPI waiter2(void *p) {
    SetEvent(ready);
    WaitForSingleObject(ev, INFINITE);
    QueryPerformanceCounter(&t1);
    woke = 1;
    return 0;
}

int main(void) {
    ev = CreateEventA(NULL, FALSE, FALSE, NULL);
    ready = CreateEventA(NULL, FALSE, FALSE, NULL);
    HANDLE t = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    for (int i = 0; i <= 40; i++) {
        WaitForSingleObject(ready, INFINITE);
        Sleep(2); /* (the waiter is in its wait by now) */
        SetEvent(ev);
        for (int k = 0; k < 2000; k++)
            sink += k;
        flag = i + 1;
        /* the next API call */
        Sleep(0);
    }
    WaitForSingleObject(t, INFINITE);
    printf("stores seen %d of 40\n", seen);

    t = CreateThread(NULL, 0, waiter2, NULL, 0, NULL);
    WaitForSingleObject(ready, INFINITE);
    Sleep(2);
    QueryPerformanceCounter(&t0);
    SetEvent(ev);
    while (!woke) /* no API call until the waiter has run */
        sink++;
    WaitForSingleObject(t, INFINITE);
    LARGE_INTEGER f;
    QueryPerformanceFrequency(&f);
    double ms = (double)(t1.QuadPart - t0.QuadPart) * 1000.0 / (double)f.QuadPart;
    printf("a spinning signaler lets its waiter go within 300 ms: %s\n", ms < 300 ? "yes" : "no");
    return 0;
}
