/* libs: -fno-omit-frame-pointer -nostartfiles -Wl,-e,probe_entry */
/* What Unity's own stack capture finds where a thread begins. Unity 5 (32-bit) walks the EBP
 * chain from GetThreadContext (and from its crash handler) until a frame's return address is 0:
 * ret = [ebp + 4], ebp = [ebp]. On Windows the chain ends in ntdll's _RtlUserThreadStart, whose
 * frame returns to 0, so the walk stops there; with a chain that ended in a saved EBP of 0
 * and a nonzero return address (sake's before), it read [0 + 4] and crashed (Alps and the
 * dangerous forest, #201). Checked on the exe's entry (no C runtime start-up: the entry is ours,
 * called as Windows calls it) and on a CreateThread thread, both with frame pointers.
 * Then GetSystemTimes, which mono's thread pool samples and aborts on when it fails (Unity 2022:
 * hroom_trial, #215): TRUE, NULL pointers allowed, kernel time includes idle time, never
 * decreasing.
 * Built for x86 (host test and winref host_framewalk-32). */
#include <stdio.h>
#include <windows.h>

#ifndef _WIN64
static int readable(DWORD p) {
    MEMORY_BASIC_INFORMATION mi;
    if (!VirtualQuery((void *)p, &mi, sizeof mi) || mi.State != MEM_COMMIT) return 0;
    if (mi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) return 0;
    return p + 8 <= (DWORD)mi.BaseAddress + mi.RegionSize;
}

static const char *system_name(DWORD addr) {
    HMODULE m = 0;
    char path[MAX_PATH], *b;
    if (!GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (const char *)addr, &m))
        return "none";
    if (!GetModuleFileNameA(m, path, sizeof path)) return "none";
    b = strrchr(path, '\\');
    b = b ? b + 1 : path;
    CharLowerA(b);
    if (!strcmp(b, "kernel32.dll") || !strcmp(b, "ntdll.dll") || !strcmp(b, "kernelbase.dll")) return "system";
    return m == GetModuleHandleA(NULL) ? "exe" : "other";
}

/* Unity 5's loop, made safe: how the walk from `ebp` ends, and where the first caller is */
static void __attribute__((noinline)) walk(const char *who, DWORD ebp) {
    DWORD first = 0;
    int frames = 0;
    const char *end = "limit";
    while (frames < 256) {
        if (!ebp) { end = "a saved EBP of 0 with a nonzero return address"; break; }
        if (!readable(ebp)) { end = "an unreadable frame"; break; }
        DWORD ret = ((DWORD *)ebp)[1], next = ((DWORD *)ebp)[0];
        if (!ret) { end = "a 0 return address"; break; }
        if (!first) first = ret;
        frames++;
        ebp = next;
    }
    printf("%s: the EBP chain ends at %s; the first caller is in a %s DLL\n", who, end, system_name(first));
}

static DWORD WINAPI __attribute__((noinline)) thread(void *arg) {
    walk("thread", (DWORD)__builtin_frame_address(0));
    return 0;
}

static volatile LONG parked;
static DWORD WINAPI __attribute__((noinline)) park(void *arg) {
    InterlockedExchange(&parked, 1);
    for (;;) Sleep(1000);
    return 0;
}
#endif

static void times(void) {
    FILETIME i0, k0, u0, i1, k1, u1;
    BOOL r = GetSystemTimes(&i0, &k0, &u0);
    printf("GetSystemTimes: %d\n", r);
    printf("NULL pointers: %d %d\n", GetSystemTimes(NULL, NULL, NULL), GetSystemTimes(NULL, &k1, NULL));
    ULONGLONG I0 = ((ULONGLONG)i0.dwHighDateTime << 32) | i0.dwLowDateTime;
    ULONGLONG K0 = ((ULONGLONG)k0.dwHighDateTime << 32) | k0.dwLowDateTime;
    ULONGLONG U0 = ((ULONGLONG)u0.dwHighDateTime << 32) | u0.dwLowDateTime;
    printf("kernel includes idle: %s\n", K0 >= I0 ? "yes" : "no");
    printf("nonzero: %s\n", K0 + U0 > 0 ? "yes" : "no");
    DWORD t = GetTickCount();
    volatile unsigned spin = 0;
    while (GetTickCount() - t < 100) spin++;
    Sleep(50);
    GetSystemTimes(&i1, &k1, &u1);
    ULONGLONG I1 = ((ULONGLONG)i1.dwHighDateTime << 32) | i1.dwLowDateTime;
    ULONGLONG K1 = ((ULONGLONG)k1.dwHighDateTime << 32) | k1.dwLowDateTime;
    ULONGLONG U1 = ((ULONGLONG)u1.dwHighDateTime << 32) | u1.dwLowDateTime;
    printf("never decreasing: %s\n", I1 >= I0 && K1 >= K0 && U1 >= U0 ? "yes" : "no");
    printf("time passed: %s\n", K1 + U1 > K0 + U0 ? "yes" : "no");
    printf("busy time not negative: %s\n", K1 + U1 >= I1 ? "yes" : "no");
}

/* (no C runtime start-up: mingw's pseudo-relocation hook has nothing to do) */
void _pei386_runtime_relocator(void) {}

void __attribute__((noinline)) entry(void) __asm__("probe_entry");
void __attribute__((noinline)) entry(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
#ifndef _WIN64
    walk("entry", (DWORD)__builtin_frame_address(0));
    HANDLE h = CreateThread(NULL, 0, thread, NULL, 0, NULL);
    WaitForSingleObject(h, INFINITE);
    CloseHandle(h);
    /* a thread parked in Sleep, walked from the EBP GetThreadContext gives */
    h = CreateThread(NULL, 0, park, NULL, 0, NULL);
    while (!parked) Sleep(1);
    Sleep(50);
    SuspendThread(h);
    CONTEXT c;
    c.ContextFlags = CONTEXT_CONTROL | CONTEXT_INTEGER;
    if (GetThreadContext(h, &c)) walk("suspended thread", c.Ebp);
    else printf("GetThreadContext failed\n");
    TerminateThread(h, 0);
    CloseHandle(h);
#endif
    times();
    ExitProcess(0);
}
