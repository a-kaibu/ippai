/* x64 programs with memory above 4G (#62): VirtualAlloc at addresses there, an exe and two
 * DLLs based there without relocations (they can't move), calls between them and back, their
 * globals through pointers, VirtualQuery/VirtualFree/VirtualProtect of those addresses. koji
 * addresses memory modulo 4G, so sake keeps each such block at its address's low half (low
 * halves chosen apart here; sake refuses two that meet: sake mem::set_high). Built by
 * build-highmem.sh (clang + lld: hm1.dll at 0x320000000, hm2.dll at 0x7ff450000000, the exe at
 * 0x140000000, all fixed). The output is the same on Windows. */
#include <windows.h>
#include <stdio.h>
#include <stdint.h>

#ifdef HM_DLL1
static volatile int64_t counter = 5;
static int64_t table[4] = {10, 20, 30, 40};
__declspec(dllexport) int64_t *hm1_counter(void) { return (int64_t *)&counter; }
__declspec(dllexport) int64_t hm1_sum(int64_t (*f)(int64_t)) {
    int64_t s = 0;
    for (int i = 0; i < 4; i++) s += f(table[i]); /* (a call back into the exe) */
    return s + counter;
}
BOOL WINAPI DllMain(HINSTANCE h, DWORD r, void *p) { (void)h; (void)r; (void)p; return TRUE; }
#elif defined HM_DLL2
__declspec(dllimport) int64_t *hm1_counter(void);
static const char name[] = "hm2 data";
__declspec(dllexport) const char *hm2_name(void) { return name; }
__declspec(dllexport) int64_t hm2_bump(int64_t by) { return *hm1_counter() += by; }
BOOL WINAPI DllMain(HINSTANCE h, DWORD r, void *p) { (void)h; (void)r; (void)p; return TRUE; }
#else
static int64_t twice(int64_t v) { return 2 * v; }

static void query(void *a, const char *what) {
    MEMORY_BASIC_INFORMATION m;
    memset(&m, 0, sizeof m);
    SIZE_T n = VirtualQuery(a, &m, sizeof m);
    printf("%s: query %d base %d state %lx", what, n == sizeof m, (char *)m.BaseAddress == (char *)((uintptr_t)a & ~(uintptr_t)0xfff), m.State);
    if (m.State != MEM_FREE) printf(" alloc %d protect %lx type %lx", m.AllocationBase != NULL, m.Protect, m.Type);
    printf("\n");
}

int main(void) {
    char *a = (char *)0x230000000ull, *b = (char *)0x531000000ull;
    char *pa = VirtualAlloc(a, 0x20000, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    char *pb = VirtualAlloc(b, 0x10000, MEM_RESERVE, PAGE_READWRITE);
    printf("alloc a %d b %d\n", pa == a, pb == b);
    if (pa != a || pb != b) return 1;
    char *cb = VirtualAlloc(b + 0x2000, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    printf("commit b %d\n", cb == b + 0x2000);
    for (int i = 0; i < 0x20000; i += 0x1000) a[i] = (char)(i >> 12);
    cb[5] = 77;
    int ok = 1;
    for (int i = 0; i < 0x20000; i += 0x1000) ok &= a[i] == (char)(i >> 12);
    volatile int64_t *qa = (int64_t *)(a + 0x10008);
    *qa = 0x1122334455667788ll;
    printf("a reads back %d, %llx, b %d\n", ok, (unsigned long long)*qa, cb[5]);
    query(a + 0x1234, "a");
    query(b, "b reserved");
    query(b + 0x2000, "b committed");
    DWORD old = 0;
    BOOL pr = VirtualProtect(a, 0x1000, PAGE_READONLY, &old);
    printf("protect %d old %lx\n", pr, old);
    query(a, "a protected");
    query((char *)0x610000000ull, "free above 4G");

    HMODULE h1 = LoadLibraryA("hm1.dll"), h2 = LoadLibraryA("hm2.dll");
    printf("hm1 at base %d, hm2 at base %d, exe at base %d\n", h1 == (HMODULE)0x320000000ull, h2 == (HMODULE)0x7ff450000000ull, GetModuleHandleA(NULL) == (HMODULE)0x140000000ull);
    if (!h1 || !h2) return 1;
    int64_t *(*counter)(void) = (void *)GetProcAddress(h1, "hm1_counter");
    int64_t (*sum)(int64_t (*)(int64_t)) = (void *)GetProcAddress(h1, "hm1_sum");
    const char *(*name)(void) = (void *)GetProcAddress(h2, "hm2_name");
    int64_t (*bump)(int64_t) = (void *)GetProcAddress(h2, "hm2_bump");
    printf("procs in their modules %d %d %d %d\n", (char *)counter - (char *)h1 < 0x100000, (char *)sum - (char *)h1 < 0x100000,
           (char *)name - (char *)h2 < 0x100000, (char *)bump - (char *)h2 < 0x100000);
    int64_t *c = counter();
    printf("counter in hm1 %d = %lld\n", (char *)c - (char *)h1 < 0x100000, (long long)*c);
    int64_t bumped = bump(37);
    printf("bump %lld, counter %lld\n", (long long)bumped, (long long)*c);
    printf("sum %lld\n", (long long)sum(twice));
    printf("name in hm2 %d: %s\n", (char *)name() - (char *)h2 < 0x100000, name());
    MEMORY_BASIC_INFORMATION m;
    VirtualQuery(c, &m, sizeof m);
    printf("counter's allocation is hm1 %d, image %d\n", m.AllocationBase == (void *)h1, m.Type == MEM_IMAGE);
    char path[MAX_PATH];
    DWORD n = GetModuleFileNameA(h2, path, MAX_PATH);
    printf("hm2 file %s\n", n > 7 ? path + n - 7 : "?");

    printf("free a %d b %d\n", VirtualFree(a, 0, MEM_RELEASE), VirtualFree(b, 0, MEM_RELEASE));
    query(a, "a freed");
    printf("again at a %d\n", VirtualAlloc(a, 0x1000, MEM_RESERVE, PAGE_READWRITE) == a);
    return 0;
}
#endif
