/* psapi's module list and module facts, as mono (Unity) uses them: a P/Invoke whose DLL is
 * "__Internal" or not mapped is looked up by GetProcAddress on every module EnumProcessModules
 * lists (Unity 2021's corlib finds SetThreadErrorMode that way). Only what holds on any Windows
 * is printed (the runner has many more modules than sake). Also GetPerformanceInfo and the
 * NtQuerySystemInformation classes it agrees with. libs: -lpsapi */
#include "../probe.h"
#include <psapi.h>

typedef struct { USHORT Length, MaximumLength; PWSTR Buffer; } US_;

int main(void) {
    HMODULE hMod1;
    HANDLE self = GetCurrentProcess();
    HMODULE exe = GetModuleHandleA(0), k32 = GetModuleHandleA("kernel32.dll"), nt = GetModuleHandleA("ntdll.dll");
    const DWORD ps = sizeof(HMODULE);
    DWORD need = 0;
    BOOL r = EnumProcessModules(self, 0, 0, &need);
    printf("count only: %d, whole handles %d, at least 3 %d\n", r, need % ps == 0, need >= 3 * ps);
    HMODULE *all = calloc(need / ps + 4, ps);
    DWORD need2 = 0;
    r = EnumProcessModules(self, all, need, &need2);
    int n = need2 / ps, has_k32 = 0, has_nt = 0;
    for (int i = 0; i < n; i++)
        has_k32 |= all[i] == k32, has_nt |= all[i] == nt;
    printf("list: %d, same size %d, first is the exe %d, kernel32 %d, ntdll %d\n", r, need2 == need, all[0] == exe, has_k32, has_nt);
    HMODULE two[2] = {0, (HMODULE)0x1234};
    need2 = 0;
    r = EnumProcessModules(self, two, ps, &need2);
    printf("room for one: %d, first %d, second untouched %d, needed all %d\n", r, two[0] == exe, two[1] == (HMODULE)0x1234, need2 == need);
    r = EnumProcessModulesEx(self, all, need, &need2, LIST_MODULES_DEFAULT);
    printf("Ex default: %d, same %d\n", r, need2 == need);
    SetLastError(0);
    r = EnumProcessModules(0, all, need, &need2);
    printf("no process: %d, error %lu\n", r, GetLastError());

    /* mono's search: the first module that has the function */
    FARPROC f = 0;
    for (int i = 0; i < n && !f; i++)
        f = GetProcAddress(all[i], "SetThreadErrorMode");
    printf("SetThreadErrorMode found %d, kernel32's %d\n", f != 0, f == GetProcAddress(k32, "SetThreadErrorMode"));

    MODULEINFO mi;
    memset(&mi, 0x55, sizeof mi);
    r = GetModuleInformation(self, exe, &mi, sizeof mi);
    IMAGE_NT_HEADERS *nh = (IMAGE_NT_HEADERS *)((char *)exe + ((IMAGE_DOS_HEADER *)exe)->e_lfanew);
    printf("exe info: %d, base %d, size %d, entry %d\n", r, mi.lpBaseOfDll == exe, mi.SizeOfImage == nh->OptionalHeader.SizeOfImage,
           mi.EntryPoint == (char *)exe + nh->OptionalHeader.AddressOfEntryPoint);
    r = GetModuleInformation(self, 0, &mi, sizeof mi);
    printf("module 0 info: %d, the exe %d\n", r, mi.lpBaseOfDll == exe);
    SetLastError(0);
    r = GetModuleInformation(self, exe, &mi, sizeof mi - 1);
    printf("info, short: %d, error %lu\n", r, GetLastError());
    SetLastError(0);
    r = GetModuleInformation(self, (HMODULE)0x10000, &mi, sizeof mi);
    printf("info, no module: %d, error %lu\n", r, GetLastError());

    char full[MAX_PATH], base[MAX_PATH], ex[MAX_PATH];
    GetModuleFileNameA(0, full, MAX_PATH);
    const char *slash = strrchr(full, '\\');
    DWORD len = GetModuleBaseNameA(self, 0, base, MAX_PATH);
    printf("exe base name: %lu chars, the file name %d\n", len, strcmp(base, slash + 1) == 0 && len == strlen(slash + 1));
    len = GetModuleBaseNameA(self, k32, base, MAX_PATH);
    printf("kernel32 base name: %lu, '%s' ignoring case %d\n", len, "kernel32.dll", _stricmp(base, "kernel32.dll") == 0);
    memset(base, 'x', 8);
    len = GetModuleBaseNameA(self, exe, base, 4);
    printf("base name in 4: %lu, copied %d, byte 4 untouched %d\n", len, memcmp(base, slash + 1, 4) == 0, base[4] == 'x');
    WCHAR wb[MAX_PATH];
    len = GetModuleBaseNameW(self, exe, wb, MAX_PATH);
    printf("base name W: %lu chars, ends %d\n", len, wb[len] == 0);
    len = GetModuleFileNameExA(self, 0, ex, MAX_PATH);
    printf("file name Ex: %lu, same as GetModuleFileName %d\n", len, strcmp(ex, full) == 0 && len == strlen(full));
    len = GetModuleFileNameExW(self, exe, wb, MAX_PATH);
    printf("file name Ex W: %lu chars, same length %d\n", len, len == strlen(full));
    SetLastError(0);
    len = GetModuleBaseNameA(self, (HMODULE)0x10000, base, MAX_PATH);
    printf("base name, no module: %lu, error %lu\n", len, GetLastError());

    /* the same, cut short: GetModuleFileNameEx ends with a 0 (unlike GetModuleFileNameA) */
    memset(ex, 'x', 8);
    SetLastError(0xdead);
    len = GetModuleFileNameExA(self, 0, ex, 4);
    printf("file name Ex in 4: %lu, error %lx, ends at 3 %d\n", len, GetLastError(), ex[3] == 0 && !memcmp(ex, full, 3));

    HANDLE me = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, GetCurrentProcessId());
    HANDLE qi = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    printf("OpenProcess self: %d %d\n", me != 0, qi != 0);
    need2 = 0;
    r = EnumProcessModules(me, all, need, &need2);
    printf("list through it: %d, same %d\n", r, need2 == need);
    SetLastError(0);
    r = EnumProcessModules(qi, all, need, &need2);
    printf("without VM_READ: %d, error %lu\n", r, GetLastError());
    SetLastError(0);
    len = GetModuleBaseNameA(qi, 0, base, MAX_PATH);
    printf("base name without VM_READ: %lu, error %lu\n", len, GetLastError());
    SetLastError(0);
    hMod1 = (HMODULE)0x1234;
    r = EnumProcessModules(me, &hMod1, ps, 0);
    printf("no count: %d, error %lu, list written %d\n", r, GetLastError(), hMod1 == exe);
    CloseHandle(me);
    CloseHandle(qi);
    SetLastError(0);
    me = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, 0x7ffffff0);
    printf("OpenProcess of no process: %d, error %lu\n", me != 0, GetLastError());
    DWORD ids[4], used = 0;
    r = EnumProcesses(ids, sizeof ids, &used);
    printf("EnumProcesses: %d, ours listed %d\n", r, used >= 4 && (ids[0] == GetCurrentProcessId() || ids[1] == GetCurrentProcessId() || ids[2] == GetCurrentProcessId() || ids[3] == GetCurrentProcessId() || used == sizeof ids));

    PERFORMANCE_INFORMATION pi;
    SetLastError(0);
    r = GetPerformanceInfo(&pi, sizeof pi - 1);
    printf("performance, short: %d, error %lu\n", r, GetLastError());
    memset(&pi, 0x55, sizeof pi);
    r = GetPerformanceInfo(&pi, sizeof pi);
    printf("performance: %d, cb %d, page size %lu, one process at least %d, threads %d\n", r, pi.cb == sizeof pi,
           (unsigned long)pi.PageSize, pi.ProcessCount >= 1, pi.ThreadCount >= 1);
    MEMORYSTATUSEX ms = {sizeof ms};
    GlobalMemoryStatusEx(&ms);
    printf("physical pages agree within 1%% %d, commit limit >= physical %d\n",
           (double)pi.PhysicalTotal * 4096 > ms.ullTotalPhys * 0.99 && (double)pi.PhysicalTotal * 4096 < ms.ullTotalPhys * 1.01, pi.CommitLimit >= pi.PhysicalTotal);

    typedef LONG (WINAPI *QSI)(int, void *, ULONG, ULONG *);
    QSI qsi = (QSI)GetProcAddress(nt, "NtQuerySystemInformation");
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    static unsigned char __attribute__((aligned(8))) sb[1 << 16];
    ULONG got = 0;
    const ULONG basic = sizeof(void *) == 8 ? 64 : 44;
    LONG st = qsi(0, sb, basic + 4, &got);
    printf("basic, long buffer: %lx, needed %lu\n", (unsigned long)st, got);
    st = qsi(0, sb, basic, &got);
    ULONG *b = (ULONG *)sb;
    void **bp = (void **)(sb + 24);
    printf("basic: %lx %lu, page %lu, pages = GetPerformanceInfo's %d, granularity %d, lowest %d, highest %d, cpus %d, mask %d\n", (unsigned long)st, got, b[2],
           b[3] == pi.PhysicalTotal, (DWORD)(ULONG_PTR)bp[0] == si.dwAllocationGranularity, bp[1] == si.lpMinimumApplicationAddress,
           bp[2] == si.lpMaximumApplicationAddress, sb[24 + 4 * sizeof(void *)] == si.dwNumberOfProcessors, (DWORD_PTR)bp[3] == si.dwActiveProcessorMask);
    st = qsi(2, sb, 312, &got);
    printf("performance class: %lx, %lu bytes, available ~ %d\n", (unsigned long)st, got, b[11] > 0);
    st = qsi(5, 0, 0, &got);
    printf("processes, no buffer: %lx, needed some %d\n", (unsigned long)st, got > 0);
    st = qsi(5, sb, sizeof sb, &got);
    int found = 0;
    for (unsigned char *e = sb;; e += *(ULONG *)e) {
        HANDLE pid = *(HANDLE *)(e + (sizeof(void *) == 8 ? 0x50 : 0x44));
        if ((DWORD)(ULONG_PTR)pid == GetCurrentProcessId()) {
            US_ *n = (US_ *)(e + 0x38);
            char a[MAX_PATH];
            int k = WideCharToMultiByte(CP_ACP, 0, n->Buffer, n->Length / 2, a, MAX_PATH, 0, 0);
            a[k] = 0;
            ULONG th = *(ULONG *)(e + 4);
            unsigned char *t0 = e + (sizeof(void *) == 8 ? 0x100 : 0xb8);
            HANDLE *cid = (HANDLE *)(t0 + (sizeof(void *) == 8 ? 0x28 : 0x20));
            int mine = 0;
            for (ULONG i = 0; i < th; i++) {
                HANDLE *ci = (HANDLE *)((unsigned char *)cid + i * (sizeof(void *) == 8 ? 0x50 : 0x40));
                mine |= (DWORD)(ULONG_PTR)ci[1] == GetCurrentThreadId() && (DWORD)(ULONG_PTR)ci[0] == GetCurrentProcessId();
            }
            printf("processes: %lx, ours: name is the exe %d, threads >= 1 %d, this thread listed %d, handles > 0 %d\n", (unsigned long)st,
                   !_stricmp(a, slash + 1), th >= 1, mine, *(ULONG *)(e + (sizeof(void *) == 8 ? 0x60 : 0x4c)) > 0);
            found = 1;
        }
        if (!*(ULONG *)e) break;
    }
    printf("ours found %d\n", found);
    return 0;
}
