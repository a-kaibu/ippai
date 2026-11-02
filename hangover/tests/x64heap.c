#include <windows.h>
#include <stdio.h>
static volatile LONG bad, ops;
static DWORD WINAPI worker(void *arg) {
    unsigned char me = (unsigned char)(ULONG_PTR)arg;
    unsigned char *p[64] = {0}; int sz[64] = {0};
    unsigned x = me * 2654435761u;
    HANDLE h = GetProcessHeap();
    for (int k = 0; k < 30000; k++) {
        x = x * 1103515245 + 12345;
        int i = (x >> 8) & 63;
        if (p[i]) {
            for (int j = 0; j < sz[i]; j++) if (p[i][j] != (unsigned char)(me + j)) { InterlockedIncrement(&bad); break; }
            HeapFree(h, 0, p[i]);
        }
        sz[i] = 8 + ((x >> 16) % 120);
        p[i] = HeapAlloc(h, 0, sz[i]);
        for (int j = 0; j < sz[i]; j++) p[i][j] = me + j;
        InterlockedIncrement(&ops);
    }
    return 0;
}
int main(void) {
    HANDLE t[6];
    for (int i = 0; i < 6; i++) t[i] = CreateThread(NULL, 0, worker, (void *)(ULONG_PTR)(i * 37 + 1), 0, NULL);
    WaitForMultipleObjects(6, t, TRUE, INFINITE);
    printf("heap: ops %ld, corrupted %ld\n", ops, bad);
    return 0;
}
