#include <windows.h>
#include <stdio.h>
#define N 64
typedef struct { SLIST_ENTRY e; volatile LONG owner; } item;
static SLIST_HEADER head;
static item *items;
static volatile LONG errors, ops;
static DWORD WINAPI worker(void *arg) {
    LONG me = (LONG)(ULONG_PTR)arg;
    for (int k = 0; k < 20000; k++) {
        item *it = (item *)InterlockedPopEntrySList(&head);
        if (!it) continue;
        if (InterlockedCompareExchange(&it->owner, me, 0) != 0) InterlockedIncrement(&errors);
        it->owner = 0;
        InterlockedPushEntrySList(&head, &it->e);
        InterlockedIncrement(&ops);
    }
    return 0;
}
int main(void) {
    InitializeSListHead(&head);
    items = _aligned_malloc(sizeof(item) * N, 16);
    for (int i = 0; i < N; i++) { items[i].owner = 0; InterlockedPushEntrySList(&head, &items[i].e); }
    HANDLE t[4];
    for (int i = 0; i < 4; i++) t[i] = CreateThread(NULL, 0, worker, (void *)(ULONG_PTR)(i + 1), 0, NULL);
    WaitForMultipleObjects(4, t, TRUE, INFINITE);
    int n = 0; while (InterlockedPopEntrySList(&head)) n++;
    printf("slist: ops %ld, double-owned %ld, entries at end %d/%d\n", ops, errors, n, N);
    return 0;
}
