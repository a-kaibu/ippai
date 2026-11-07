/* psapi's module list and module facts, as mono (Unity) uses them: a P/Invoke whose DLL is
 * "__Internal" or not mapped is looked up by GetProcAddress on every module EnumProcessModules
 * lists (Unity 2021's corlib finds SetThreadErrorMode that way). Only what holds on any Windows
 * is printed (the runner has many more modules than sake). libs: -lpsapi */
#include "../probe.h"
#include <psapi.h>

int main(void) {
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
    return 0;
}
