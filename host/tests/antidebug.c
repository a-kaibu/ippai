/* What Enigma Protector (anemoi's SiglusEngine.exe) asks before it decides "A debugger is
 * detected", answered as Windows does with no debugger attached (tests/winref/probes debugsvc
 * and toolhelp are the same questions against Windows' own answers):
 * - int 2d (the debug service) raises STATUS_BREAKPOINT with the context just after it; a
 *   debugger would swallow it and resume a byte further;
 * - a Toolhelp module snapshot lists the exe first, then ntdll and kernel32, and is a Section
 *   object to NtQueryObject; once closed the handle is gone;
 * - \\.\C: opens with no access asked, \\.\NTICE (SoftICE) is not found. */
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include <tlhelp32.h>
#include <winternl.h>

/* returns 1 when resumed just after int 2d, 0 when a byte further (Wine's ntdll exception.c) */
#ifdef _WIN64
static const BYTE code[] = {
    0x53, 0x57, 0x48, 0x89, 0xc8, 0xcd, 0x2d, 0xeb, 0x0f, 0x1f, 0x00, 0x48, 0x31, 0xc0, 0xeb, 0x0e,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x48, 0x31, 0xc0, 0x48, 0xff, 0xc0, 0x5f, 0x5b, 0xc3,
};
#define AFTER 7
#else
static const BYTE code[] = {
    0x53, 0x57, 0x8b, 0x44, 0x24, 0x0c, 0xcd, 0x2d, 0xeb, 0x0f, 0x1f, 0x00, 0x31, 0xc0, 0xeb, 0x0c,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x31, 0xc0, 0x40, 0x5f, 0x5b, 0xc3,
};
#define AFTER 8
#endif

static BYTE *mem;
static int count, after, eax_param;

static LONG CALLBACK veh(EXCEPTION_POINTERS *e) {
    EXCEPTION_RECORD *r = e->ExceptionRecord;
    if (r->ExceptionCode != EXCEPTION_BREAKPOINT) return EXCEPTION_CONTINUE_SEARCH;
    count++;
#ifdef _WIN64
    after = e->ContextRecord->Rip == (DWORD_PTR)mem + AFTER;
    eax_param = r->NumberParameters >= 1 && r->ExceptionInformation[0] == e->ContextRecord->Rax;
#else
    after = e->ContextRecord->Eip == (DWORD_PTR)mem + AFTER;
    eax_param = r->NumberParameters >= 1 && r->ExceptionInformation[0] == e->ContextRecord->Eax;
#endif
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* (an SEH frame of the kind Enigma sets: reached when no vectored handler takes it) */
static int filtered;
static LONG WINAPI last_chance(EXCEPTION_POINTERS *e) {
    if (e->ExceptionRecord->ExceptionCode != EXCEPTION_BREAKPOINT) return EXCEPTION_CONTINUE_SEARCH;
    filtered++;
    return EXCEPTION_CONTINUE_EXECUTION;
}

typedef LONG(WINAPI *QueryObject)(HANDLE, int, void *, ULONG, ULONG *);

static void type_of(const char *label, HANDLE h) {
    QueryObject q = (QueryObject)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryObject");
    union { char b[512]; void *align; } u;
    ULONG n = 0;
    LONG st = q(h, 2, u.b, sizeof u.b, &n);
    UNICODE_STRING *s = (UNICODE_STRING *)u.b;
    char name[64] = "";
    if (st == 0) WideCharToMultiByte(CP_ACP, 0, s->Buffer, s->Length / 2, name, sizeof name - 1, NULL, NULL);
    printf("%s: status %08lx %s\n", label, (unsigned long)st, name);
}

int main(void) {
    mem = VirtualAlloc(NULL, 4096, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    memcpy(mem, code, sizeof code);
    DWORD(CDECL * f)(DWORD_PTR) = (void *)mem;
    void *v = AddVectoredExceptionHandler(1, veh);
    /* break, prompt and an unknown service raise on every Windows */
    static const DWORD services[] = { 0, 2, 6 };
    for (int i = 0; i < 3; i++) {
        count = after = eax_param = 0;
        DWORD ret = f(services[i]);
        printf("int 2d service %lu: exceptions %d resumed after it %lu context after it %d eax param %d\n", services[i], count, ret, after, eax_param);
    }
    RemoveVectoredExceptionHandler(v);
    SetUnhandledExceptionFilter(last_chance);
    DWORD ret = f(0);
    printf("int 2d, no handler but the filter: %d resumed after it %lu\n", filtered, ret);

    char exe[MAX_PATH], *base;
    GetModuleFileNameA(NULL, exe, sizeof exe);
    base = strrchr(exe, '\\') ? strrchr(exe, '\\') + 1 : exe;
    HANDLE h = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, 0);
    printf("snapshot: %s\n", h == INVALID_HANDLE_VALUE ? "invalid" : "a handle");
    type_of("snapshot type", h);
    MODULEENTRY32 m;
    m.dwSize = sizeof m - 1;
    SetLastError(0xdead);
    int ok = Module32First(h, &m);
    printf("short dwSize: %d err %lu\n", ok, GetLastError());
    m.dwSize = sizeof m;
    ok = Module32First(h, &m);
    printf("first is the exe: %d at its base %d\n", ok && !_stricmp(m.szModule, base), (HMODULE)m.modBaseAddr == GetModuleHandleA(NULL));
    int n = 0, k32 = 0, nt = 0;
    while (ok) {
        n++;
        if (!_stricmp(m.szModule, "kernel32.dll")) k32 = m.hModule == GetModuleHandleA("kernel32.dll");
        if (!_stricmp(m.szModule, "ntdll.dll")) nt = m.hModule == GetModuleHandleA("ntdll.dll");
        ok = Module32Next(h, &m);
    }
    DWORD err = GetLastError();
    printf("listed ntdll %d kernel32 %d, more than 2 %d, then err %lu\n", nt, k32, n > 2, err);
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
    type_of("closed type", h);
    SetLastError(0xdead);
    ok = Module32First(h, &m);
    printf("closed first: %d err %lu\n", ok, GetLastError());

    /* a volume opens with no access; a kernel debugger's device is not there */
    static const char *const devs[] = { "\\\\.\\C:", "\\\\.\\NTICE", "\\\\.\\SICE" };
    for (int i = 0; i < 3; i++) {
        SetLastError(0xdead);
        HANDLE d = CreateFileA(devs[i], i ? GENERIC_READ : 0, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
        err = GetLastError();
        printf("%s: %s err %lu\n", devs[i], d == INVALID_HANDLE_VALUE ? "fails" : "opens", d == INVALID_HANDLE_VALUE ? err : 0);
        if (d != INVALID_HANDLE_VALUE) CloseHandle(d);
    }
    return 0;
}
