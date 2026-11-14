/* Tool help snapshots of this process (CreateToolhelp32Snapshot, Module32First/Next) and the
 * snapshot handle's type as NtQueryObject names it. Enigma Protector (anemoi's SiglusEngine.exe)
 * lists its own modules and asks the snapshot handle's type before it decides "A debugger is
 * detected": on Windows the list starts with the exe and has kernel32.dll and ntdll.dll, and a
 * snapshot is a Section object. Only facts that hold on every Windows are printed (no bases,
 * paths or counts). */
#include "../probe.h"
#include <tlhelp32.h>
#include <winternl.h>

typedef LONG(WINAPI *QueryObject)(HANDLE, int, void *, ULONG, ULONG *);

static void type_of(const char *label, HANDLE h) {
    QueryObject q = (QueryObject)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryObject");
    union { char b[512]; void *align; } u;
    ULONG n = 0;
    LONG st = q(h, 2, u.b, sizeof u.b, &n);
    /* OBJECT_TYPE_INFORMATION starts with the type's UNICODE_STRING */
    UNICODE_STRING *s = (UNICODE_STRING *)u.b;
    char name[64] = "";
    if (st == 0)
        WideCharToMultiByte(CP_ACP, 0, s->Buffer, s->Length / 2, name, sizeof name - 1, NULL, NULL);
    printf("%s type: status %08lx %s\n", label, (unsigned long)st, name);
}

int main(void) {
    char exe[MAX_PATH], *base;
    GetModuleFileNameA(NULL, exe, sizeof exe);
    base = strrchr(exe, '\\') ? strrchr(exe, '\\') + 1 : exe;

    HANDLE h = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, 0);
    printf("snapshot: %s\n", h == INVALID_HANDLE_VALUE ? "invalid" : "handle");
    type_of("snapshot", h);

    MODULEENTRY32 m;
    m.dwSize = sizeof m - 1;
    SetLastError(0xdead);
    int r = Module32First(h, &m);
    printf("short dwSize: %d err %lu\n", r, GetLastError());

    m.dwSize = sizeof m;
    int ok = Module32First(h, &m), n = 0, k32 = 0, nt = 0;
    printf("first: %d is the exe %d hModule is its base %d base is the exe's %d pid ok %d size>0 %d\n", ok,
           _stricmp(m.szModule, base) == 0, m.hModule == (HMODULE)m.modBaseAddr,
           m.hModule == GetModuleHandleA(NULL), m.th32ProcessID == GetCurrentProcessId(), m.modBaseSize > 0);
    printf("first path is the exe's: %d\n", _stricmp(m.szExePath, exe) == 0);
    while (ok) {
        n++;
        if (!_stricmp(m.szModule, "kernel32.dll")) k32 = m.hModule == GetModuleHandleA("kernel32.dll");
        if (!_stricmp(m.szModule, "ntdll.dll")) nt = m.hModule == GetModuleHandleA("ntdll.dll");
        ok = Module32Next(h, &m);
    }
    printf("after the last: err %lu; listed kernel32 %d ntdll %d more than 2 %d\n", GetLastError(), k32, nt, n > 2);
    /* from the start again */
    m.dwSize = sizeof m;
    printf("first again is the exe: %d\n", Module32First(h, &m) && _stricmp(m.szModule, base) == 0);

    MODULEENTRY32W w;
    w.dwSize = sizeof w;
    WCHAR wbase[MAX_PATH];
    MultiByteToWideChar(CP_ACP, 0, base, -1, wbase, MAX_PATH);
    printf("first W is the exe: %d\n", Module32FirstW(h, &w) && _wcsicmp(w.szModule, wbase) == 0);
    /* (no heap lists in a module snapshot) and this process's memory read back */
    HEAPLIST32 hl;
    hl.dwSize = sizeof hl;
    SetLastError(0xdead);
    int hr = Heap32ListFirst(h, &hl);
    printf("heap list first: %d err %lu\n", hr, GetLastError());
    static const char secret[8] = "sake!ok";
    char got[8] = "";
    SIZE_T nread = 0;
    int rr = Toolhelp32ReadProcessMemory(GetCurrentProcessId(), secret, got, sizeof got, &nread);
    printf("read own memory: %d %lu same %d\n", rr, (unsigned long)nread, !memcmp(got, secret, 8));
    printf("close: %d\n", CloseHandle(h));
    /* (a closed snapshot: no handle any more) */
    type_of("closed", h);
    SetLastError(0xdead);
    m.dwSize = sizeof m;
    r = Module32First(h, &m);
    printf("closed first: %d err %lu\n", r, GetLastError());

    /* a process list: a Section too */
    HANDLE p = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    type_of("process snapshot", p);
    CloseHandle(p);
    return 0;
}
