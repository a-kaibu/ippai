/* kernel32 memory: Heap/Global/Local sizes, flags and error codes, in-place reallocations,
 * VirtualAlloc/VirtualQuery, IsBad*Ptr. Only facts independent of addresses are printed. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-64s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

int main(void) {
    HANDLE ph = GetProcessHeap();
    static const SIZE_T sz[] = { 0, 1, 15, 16, 17, 511, 512, 4096, 100000, 1 << 20 };
    for (unsigned i = 0; i < sizeof sz / sizeof *sz; i++) {
        void *p = HeapAlloc(ph, HEAP_ZERO_MEMORY, sz[i]);
        int zero = 1;
        for (SIZE_T k = 0; k < sz[i]; k++) zero &= ((unsigned char *)p)[k] == 0;
        printf("HeapAlloc %lu: size %ld zero %d align16 %d\n", (unsigned long)sz[i], (long)HeapSize(ph, 0, p), zero,
               ((ULONG_PTR)p & 15) == 0 || ((ULONG_PTR)p & 7) == 0);
        memset(p, 0xab, sz[i]);
        void *q = HeapReAlloc(ph, HEAP_REALLOC_IN_PLACE_ONLY, p, sz[i] / 2);
        printf("  in-place shrink %d size %ld\n", q == p, q ? (long)HeapSize(ph, 0, q) : -1L);
        void *r = HeapReAlloc(ph, HEAP_ZERO_MEMORY, p, sz[i] + 64);
        int z2 = 1, kept = 1;
        for (SIZE_T k = sz[i] / 2; k < sz[i] / 2 + 64 && r; k++) z2 &= 1;
        for (SIZE_T k = 0; k < sz[i] / 2 && r; k++) kept &= ((unsigned char *)r)[k] == 0xab;
        for (SIZE_T k = sz[i]; k < sz[i] + 64 && r; k++) z2 &= ((unsigned char *)r)[k] == 0;
        printf("  grow size %ld kept %d zeroed %d\n", r ? (long)HeapSize(ph, 0, r) : -1L, kept, z2);
        E(HeapFree(ph, 0, r));
    }
    E(HeapFree(ph, 0, NULL));
    E(HeapAlloc(ph, 0, (SIZE_T)-64) == NULL);
    {
        HANDLE h = HeapCreate(0, 0x10000, 0x20000);
        void *p = HeapAlloc(h, 0, 0x8000);
        printf("fixed heap 0x8000 %d\n", p != NULL);
        void *big = HeapAlloc(h, 0, 0x40000);
        printf("fixed heap too big %d err %lu\n", big != NULL, GetLastError());
        E(HeapDestroy(h));
    }
    {
        HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, 100);
        E(GlobalSize(g));
        E(GlobalFlags(g));
        char *p = GlobalLock(g);
        E(GlobalFlags(g));
        printf("zero %d\n", p[0] == 0 && p[99] == 0);
        E(GlobalHandle(p) == g);
        E(GlobalUnlock(g));
        E(GlobalUnlock(g));
        HGLOBAL g2 = GlobalReAlloc(g, 5000, GMEM_MOVEABLE);
        E(GlobalSize(g2));
        E(GlobalFree(g2) == NULL);
        HGLOBAL f = GlobalAlloc(GMEM_FIXED, 33);
        E(GlobalLock(f) == (void *)f);
        E(GlobalSize(f));
        E(GlobalFlags(f));
        E(GlobalFree(f) == NULL);
        HLOCAL l = LocalAlloc(LMEM_MOVEABLE, 10);
        E(LocalSize(l));
        E(LocalFlags(l));
        E(LocalFree(l) == NULL);
        E(GlobalAlloc(GMEM_FIXED, 0) != NULL);
        E(GlobalAlloc(GMEM_MOVEABLE, 0) != NULL);
    }
    {
        char *p = VirtualAlloc(NULL, 70000, MEM_RESERVE, PAGE_NOACCESS);
        MEMORY_BASIC_INFORMATION mi;
        VirtualQuery(p, &mi, sizeof mi);
        printf("reserve: region %lu state %lx protect %lx alloc %lx type %lx\n", (unsigned long)mi.RegionSize,
               mi.State, mi.Protect, mi.AllocationProtect, mi.Type);
        E(VirtualAlloc(p + 4096, 100, MEM_COMMIT, PAGE_READWRITE) == p + 4096);
        VirtualQuery(p + 4096, &mi, sizeof mi);
        printf("commit: region %lu state %lx protect %lx base off %ld\n", (unsigned long)mi.RegionSize, mi.State,
               mi.Protect, (long)((char *)mi.BaseAddress - p));
        VirtualQuery(p + 8192, &mi, sizeof mi);
        printf("after: region %lu state %lx protect %lx\n", (unsigned long)mi.RegionSize, mi.State, mi.Protect);
        DWORD old;
        E(VirtualProtect(p + 4096, 10, PAGE_READONLY, &old));
        printf("old %lx\n", old);
        E(IsBadReadPtr(p + 4096, 4));
        E(IsBadWritePtr(p + 4096, 4));
        E(IsBadReadPtr(p, 4));
        E(IsBadReadPtr(NULL, 4));
        E(VirtualFree(p + 4096, 4096, MEM_DECOMMIT));
        E(VirtualFree(p, 4096, MEM_RELEASE));
        E(VirtualFree(p, 0, MEM_RELEASE));
        E(VirtualFree(p, 0, MEM_RELEASE));
        SYSTEM_INFO si;
        GetSystemInfo(&si);
        printf("page %lu granularity %lu\n", si.dwPageSize, si.dwAllocationGranularity);
    }
    {
        /* a moveable block's handle: the address of the pointer in a {flags, pointer} entry */
        const ULONG_PTR ps = sizeof(void *);
        HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE | GMEM_DISCARDABLE, 10);
        ULONG_PTR *e = (ULONG_PTR *)g - 1;
        printf("moveable: handle mod %lu entry flags %lx ptr is block %d\n", (unsigned long)((ULONG_PTR)g & (2 * ps - 1)),
               (unsigned long)e[0], (void *)e[1] == GlobalLock(g));
        printf("  locked flags %lx GlobalFlags %x\n", (unsigned long)e[0], GlobalFlags(g));
        for (int i = 0; i < 300; i++) GlobalLock(g);
        printf("  300 more: flags %lx GlobalFlags %x\n", (unsigned long)e[0], GlobalFlags(g));
        while (GlobalUnlock(g)) {}
        printf("  unlocked flags %lx err %lu\n", (unsigned long)e[0], GetLastError());
        E(GlobalReAlloc(g, 0, GMEM_MOVEABLE) == g);
        printf("  discarded flags %lx ptr %d\n", (unsigned long)e[0], e[1] != 0);
        E(GlobalSize(g));
        E(GlobalFlags(g));
        E(GlobalLock(g) == NULL);
        E(GlobalReAlloc(g, 20, GMEM_MOVEABLE) == g);
        printf("  back flags %lx\n", (unsigned long)e[0]);
        E(GlobalFree(g) == NULL);
        printf("  freed: flags used %d aligned %d ptr %d\n", (int)(e[0] & 1), (e[0] & (2 * ps - 1)) == 0, e[1] != 0);
        HGLOBAL d = GlobalAlloc(GMEM_MOVEABLE | GMEM_DDESHARE, 0);
        printf("ddeshare empty flags %lx\n", (unsigned long)((ULONG_PTR *)d)[-1]);
        E(GlobalFlags(d));
        E(LocalFlags(d));
        E(GlobalFree(d) == NULL);
        HLOCAL l = LocalAlloc(LMEM_MOVEABLE | LMEM_DISCARDABLE, 4);
        E(LocalFlags(l));
        E(LocalUnlock(l));
        E(LocalFree(l) == NULL);
        void *f = GlobalAlloc(GMEM_FIXED, 8);
        E(GlobalUnlock(f));
        E(LocalUnlock(f));
        E(GlobalHandle(f) == f);
        E(GlobalReAlloc(f, 8, GMEM_MODIFY | GMEM_DISCARDABLE) == f);
        E(GlobalFree(f) == NULL);
        E(GlobalHandle(NULL));
    }
    return 0;
}
