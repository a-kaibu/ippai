/* Memory handed out again reads zero, and zeroing clears exactly what it is asked to, wherever
 * the bytes were set: a released reservation reserved again at the same address, decommitted
 * pages committed again, HeapAlloc / HeapReAlloc with HEAP_ZERO_MEMORY and calloc over blocks a
 * program dirtied and freed, memset(0) / RtlZeroMemory over partly written ranges cut at odd
 * bytes. sake (mem.rs zero) leaves pages that are zero already unwritten, so that memory the
 * program never used does not become RAM in the browser; this checks it still zeroes the rest.
 * (sake: mem.rs, crt.rs memset, k32.rs RtlZeroMemory) */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned seed = 12345;
static unsigned rnd(void) {
    seed = seed * 1103515245u + 12345u;
    return seed >> 8;
}
/* bytes set in [p, p+n) and a checksum of them */
static void sum(const char *what, const unsigned char *p, size_t n) {
    size_t set = 0;
    unsigned h = 2166136261u;
    for (size_t i = 0; i < n; i++) {
        if (p[i]) set++;
        h = (h ^ p[i]) * 16777619u;
    }
    printf("%s: %lu bytes set, hash %08x\n", what, (unsigned long)set, h);
}
/* scatters `k` nonzero bytes over [p, p+n), a few whole pages too */
static void dirty(unsigned char *p, size_t n, int k) {
    for (int i = 0; i < k; i++) p[rnd() % n] = (unsigned char)(rnd() | 1);
    for (int i = 0; i < 4; i++) memset(p + (rnd() % (n / 4096)) * 4096, 0x5a, 4096);
}

int main(void) {
    const size_t BIG = 8 << 20;
    /* a reservation used here and there, released, reserved again where it was */
    unsigned char *a = VirtualAlloc(NULL, BIG, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    printf("reserve %d\n", a != NULL);
    dirty(a, BIG, 500);
    sum("dirtied", a, BIG);
    printf("release %d\n", VirtualFree(a, 0, MEM_RELEASE));
    unsigned char *b = VirtualAlloc(a, BIG, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    printf("again at the same address %d\n", b == a);
    if (b) sum("reserved again", b, BIG);
    /* decommitted and committed again: pages in the middle, the range cut at page bounds */
    if (b) {
        dirty(b, BIG, 500);
        printf("decommit %d\n", VirtualFree(b + 0x123000, 0x345000, MEM_DECOMMIT));
        printf("commit %d\n", VirtualAlloc(b + 0x123000, 0x345000, MEM_COMMIT, PAGE_READWRITE) == b + 0x123000);
        sum("recommitted part", b + 0x123000, 0x345000);
        VirtualFree(b, 0, MEM_RELEASE);
    }
    /* the process heap: a dirty block freed, zeroed blocks of that size after it */
    HANDLE h = GetProcessHeap();
    for (int round = 0; round < 3; round++) {
        size_t n = (size_t)(1 << 18) * (3 + 5 * round) + 123;
        unsigned char *p = HeapAlloc(h, 0, n);
        dirty(p, n, 300);
        HeapFree(h, 0, p);
        unsigned char *z = HeapAlloc(h, HEAP_ZERO_MEMORY, n);
        sum("zeroed heap block", z, n);
        memset(z, 0x77, n);
        unsigned char *g = HeapReAlloc(h, HEAP_ZERO_MEMORY, z, 2 * n);
        size_t set = 0;
        for (size_t i = 0; i < 2 * n; i++) set += g[i] != (i < n ? 0x77 : 0);
        printf("grown with zeros: %lu bytes wrong\n", (unsigned long)set);
        HeapFree(h, 0, g);
    }
    /* msvcrt: calloc after a dirty free; memset(0) and RtlZeroMemory over partly dirty ranges */
    for (int round = 0; round < 3; round++) {
        size_t n = (size_t)(1 << 18) * (2 + 3 * round) + 77;
        unsigned char *p = malloc(n);
        dirty(p, n, 200);
        free(p);
        unsigned char *c = calloc(n, 1);
        sum("calloc", c, n);
        free(c);
    }
    /* (through the DLLs' own exports, not the compiler's inline memset) */
    void *(__cdecl *ms)(void *, int, size_t) = (void *)GetProcAddress(LoadLibraryA("msvcrt.dll"), "memset");
    void(WINAPI *rz)(void *, SIZE_T) = (void *)GetProcAddress(LoadLibraryA("ntdll.dll"), "RtlZeroMemory");
    unsigned char *m = VirtualAlloc(NULL, 1 << 20, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    for (int round = 0; round < 16; round++) {
        size_t at = rnd() % (512 << 10), n = rnd() % (512 << 10);
        dirty(m, 1 << 20, 100);
        if (round & 1) rz(m + at, n); else ms(m + at, 0, n);
        size_t wrong = 0;
        for (size_t i = at; i < at + n; i++) wrong += m[i] != 0;
        printf("%s %lu+%lu: %lu bytes left\n", round & 1 ? "RtlZeroMemory" : "memset", (unsigned long)at, (unsigned long)n, (unsigned long)wrong);
    }
    /* and a fill that is not zero still fills */
    ms(m + 5, 0xab, 700 << 10);
    size_t wrong = 0;
    for (size_t i = 5; i < 5 + (700 << 10); i++) wrong += m[i] != 0xab;
    printf("memset 0xab: %lu bytes wrong, before %d after %d\n", (unsigned long)wrong, m[4] == 0xab, m[5 + (700 << 10)] == 0xab);
    return 0;
}
