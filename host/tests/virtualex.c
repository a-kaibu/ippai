/* VirtualAllocEx, VirtualProtectEx, VirtualFreeEx and VirtualQueryEx on the process itself
 * (GetCurrentProcess and a real handle from OpenProcess) agree with the plain calls; a bad
 * handle fails. ChakraCore (moratorium by WendyBell) protects its JIT pages with
 * VirtualProtectEx(GetCurrentProcess(), ...) and quits when it returns 0. (sake: k32.rs) */
#include <windows.h>
#include <stdio.h>

static void query(HANDLE p, void *a, const char *what) {
    MEMORY_BASIC_INFORMATION m;
    SIZE_T n = VirtualQueryEx(p, a, &m, sizeof m);
    printf("  %s: query %d state %lx protect %lx", what, n == sizeof m, m.State, m.Protect);
    /* a free region's size is wherever the next allocation happens to be */
    if (m.State != MEM_FREE) printf(" size %lx", (unsigned long)m.RegionSize);
    printf("\n");
}

static void run(HANDLE p, const char *name) {
    printf("%s\n", name);
    char *a = VirtualAllocEx(p, NULL, 0x3000, MEM_RESERVE, PAGE_NOACCESS);
    printf("  reserve %d\n", a != NULL);
    query(p, a, "reserved");
    char *c = VirtualAllocEx(p, a + 0x1000, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    printf("  commit at %d\n", c == a + 0x1000);
    c[0] = 1;
    query(p, c, "committed");
    DWORD old = 0xdead;
    BOOL ok = VirtualProtectEx(p, c, 0x1000, PAGE_EXECUTE_READ, &old);
    printf("  protect %d old %lx\n", ok, old);
    query(p, c, "protected");
    old = 0xdead;
    ok = VirtualProtectEx(p, c, 0x1000, PAGE_READWRITE, &old);
    printf("  protect back %d old %lx byte %d\n", ok, old, c[0]);
    ok = VirtualFreeEx(p, c, 0x1000, MEM_DECOMMIT);
    printf("  decommit %d\n", ok);
    query(p, c, "decommitted");
    SetLastError(0);
    ok = VirtualFreeEx(p, a + 0x1000, 0, MEM_RELEASE);
    printf("  release inside %d error %lu\n", ok, GetLastError());
    ok = VirtualFreeEx(p, a, 0, MEM_RELEASE);
    printf("  release %d\n", ok);
    query(p, a, "released");
    char *d = VirtualAllocEx(p, NULL, 0x1000, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    d[0x123] = 7;
    printf("  reserve+commit %d byte %d\n", d != NULL, d[0x123]);
    VirtualFreeEx(p, d, 0, MEM_RELEASE);
}

int main(void) {
    run(GetCurrentProcess(), "current process");
    HANDLE h = OpenProcess(PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    printf("open %d\n", h != NULL);
    if (h) {
        run(h, "opened handle");
        CloseHandle(h);
    }
    DWORD old;
    SetLastError(0);
    void *a = VirtualAllocEx(NULL, NULL, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    printf("null handle alloc %d error %lu\n", a != NULL, GetLastError());
    SetLastError(0);
    BOOL ok = VirtualProtectEx(NULL, (void *)0x10000, 0x1000, PAGE_READWRITE, &old);
    printf("null handle protect %d error %lu\n", ok, GetLastError());
    SetLastError(0);
    ok = VirtualFreeEx(NULL, (void *)0x10000, 0, MEM_RELEASE);
    printf("null handle free %d error %lu\n", ok, GetLastError());
    return 0;
}
