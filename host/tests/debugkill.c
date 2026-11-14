/* Debuggers and their debug objects across processes (sake: dbg.rs, kern/debug.rs). A
 * debuggee whose threads keep the shared kernel busy (waits in a loop) is killed when its
 * debugger's debug object closes with DEBUG_KILL_ON_CLOSE: done several times, since the
 * kill once froze every process when it hit a thread holding the kernel's lock (host/vnkern.js
 * now frees a dead holder's lock). Without the flag the debuggee goes on; an attached
 * debugger sees the reported threads suspended until it continues their events; detaching
 * lets the process go on. Expected output as on Windows. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

typedef NTSTATUS (WINAPI *connect_fn)(void);
typedef HANDLE (WINAPI *get_fn)(void);
typedef void (WINAPI *set_fn)(HANDLE);
static connect_fn pDbgUiConnectToDbg;
static get_fn pDbgUiGetThreadDebugObject;
static set_fn pDbgUiSetThreadDebugObject;

static HANDLE go;

static DWORD WINAPI spin(void *arg) {
    (void)arg;
    for (;;) WaitForSingleObject(go, 1);
    return 0;
}

/* the debuggee: busy threads until `go` is set, then exits 7 */
static int child(HANDLE ev) {
    go = ev;
    for (int i = 0; i < 3; i++) CloseHandle(CreateThread(NULL, 0, spin, NULL, 0, NULL));
    for (;;) {
        if (WaitForSingleObject(go, 1) == WAIT_OBJECT_0) ExitProcess(7);
    }
}

static const char *name(DWORD code) {
    switch (code) {
    case EXCEPTION_DEBUG_EVENT: return "exception";
    case CREATE_THREAD_DEBUG_EVENT: return "create thread";
    case CREATE_PROCESS_DEBUG_EVENT: return "create process";
    case EXIT_THREAD_DEBUG_EVENT: return "exit thread";
    case EXIT_PROCESS_DEBUG_EVENT: return "exit process";
    case LOAD_DLL_DEBUG_EVENT: return "load dll";
    case UNLOAD_DLL_DEBUG_EVENT: return "unload dll";
    case OUTPUT_DEBUG_STRING_EVENT: return "output";
    default: return "?";
    }
}

/* Continues events until the loader's breakpoint (which is continued too); 1 if it came */
static int to_breakpoint(int *first_kind) {
    DEBUG_EVENT ev;
    int n = 0;
    while (WaitForDebugEvent(&ev, 10000)) {
        if (n++ == 0) *first_kind = ev.dwDebugEventCode;
        if (ev.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT) {
            CloseHandle(ev.u.CreateProcessInfo.hFile);
        }
        if (ev.dwDebugEventCode == LOAD_DLL_DEBUG_EVENT) CloseHandle(ev.u.LoadDll.hFile);
        int bp = ev.dwDebugEventCode == EXCEPTION_DEBUG_EVENT && ev.u.Exception.ExceptionRecord.ExceptionCode == EXCEPTION_BREAKPOINT;
        ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
        if (bp) return 1;
    }
    return 0;
}

/* Continues whatever comes for `ms` */
static void drain(DWORD ms) {
    DEBUG_EVENT ev;
    while (WaitForDebugEvent(&ev, ms)) ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
}

static BOOL start(const char *exe, HANDLE ev, DWORD flags, PROCESS_INFORMATION *pi) {
    char cmd[MAX_PATH + 64];
    STARTUPINFOA si = { sizeof si };
    snprintf(cmd, sizeof cmd, "\"%s\" child %lu", exe, (unsigned long)(ULONG_PTR)ev);
    return CreateProcessA(NULL, cmd, NULL, NULL, TRUE, flags, NULL, NULL, &si, pi);
}

int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "child")) return child((HANDLE)(ULONG_PTR)strtoul(argv[2], NULL, 10));
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    pDbgUiConnectToDbg = (connect_fn)GetProcAddress(nt, "DbgUiConnectToDbg");
    pDbgUiGetThreadDebugObject = (get_fn)GetProcAddress(nt, "DbgUiGetThreadDebugObject");
    pDbgUiSetThreadDebugObject = (set_fn)GetProcAddress(nt, "DbgUiSetThreadDebugObject");
    char exe[MAX_PATH];
    GetModuleFileNameA(NULL, exe, MAX_PATH);
    SECURITY_ATTRIBUTES sa = { sizeof sa, NULL, TRUE };
    HANDLE ev = CreateEventA(&sa, TRUE, FALSE, NULL);
    PROCESS_INFORMATION pi;
    DWORD code;

    printf("no debug object: %d\n", pDbgUiGetThreadDebugObject() == NULL);
    SetLastError(0);
    DEBUG_EVENT de;
    BOOL waited = WaitForDebugEvent(&de, 0);
    printf("wait without one: %d error %lu\n", waited, GetLastError());

    /* killed with the debug object, again and again */
    for (int i = 0; i < 4; i++) {
        BOOL ok = start(exe, ev, DEBUG_PROCESS, &pi);
        int first = -1;
        int bp = ok && to_breakpoint(&first);
        drain(100 * (i + 1));
        HANDLE dbg = pDbgUiGetThreadDebugObject();
        CloseHandle(dbg);
        pDbgUiSetThreadDebugObject(NULL);
        DWORD w = WaitForSingleObject(pi.hProcess, 10000);
        GetExitCodeProcess(pi.hProcess, &code);
        printf("kill %d: started %d first %s breakpoint %d object %d ended %d code %08lx\n", i, ok, name(first), bp, dbg != NULL, w == WAIT_OBJECT_0, code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }

    /* DebugSetProcessKillOnExit(FALSE): the debuggee outlives its debug object */
    {
        BOOL ok = start(exe, ev, DEBUG_ONLY_THIS_PROCESS, &pi);
        int first = -1;
        int bp = ok && to_breakpoint(&first);
        drain(100);
        BOOL nokill = DebugSetProcessKillOnExit(FALSE);
        CloseHandle(pDbgUiGetThreadDebugObject());
        pDbgUiSetThreadDebugObject(NULL);
        DWORD w = WaitForSingleObject(pi.hProcess, 300);
        SetEvent(ev);
        DWORD w2 = WaitForSingleObject(pi.hProcess, 10000);
        GetExitCodeProcess(pi.hProcess, &code);
        printf("no kill: breakpoint %d set %d alive %d ended %d code %lu\n", bp, nokill, w == WAIT_TIMEOUT, w2 == WAIT_OBJECT_0, code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        ResetEvent(ev);
    }

    /* attaching: the reported threads wait suspended for their event, detaching lets go */
    {
        BOOL ok = start(exe, ev, 0, &pi);
        Sleep(300);
        BOOL at = DebugActiveProcess(pi.dwProcessId);
        DEBUG_EVENT e;
        BOOL got = WaitForDebugEvent(&e, 10000);
        int kind = got ? (int)e.dwDebugEventCode : -1;
        DWORD before = got && kind == CREATE_PROCESS_DEBUG_EVENT ? SuspendThread(e.u.CreateProcessInfo.hThread) : (DWORD)-1;
        if (before != (DWORD)-1) ResumeThread(e.u.CreateProcessInfo.hThread);
        HANDLE main_thread = kind == CREATE_PROCESS_DEBUG_EVENT ? e.u.CreateProcessInfo.hThread : NULL;
        if (kind == CREATE_PROCESS_DEBUG_EVENT) CloseHandle(e.u.CreateProcessInfo.hFile);
        if (got) ContinueDebugEvent(e.dwProcessId, e.dwThreadId, DBG_CONTINUE);
        DWORD after = main_thread ? SuspendThread(main_thread) : (DWORD)-1;
        if (after != (DWORD)-1) ResumeThread(main_thread);
        drain(300);
        BOOL stop = DebugActiveProcessStop(pi.dwProcessId);
        SetEvent(ev);
        DWORD w = WaitForSingleObject(pi.hProcess, 10000);
        GetExitCodeProcess(pi.hProcess, &code);
        printf("attach: started %d attached %d first %s suspended %d then %ld detached %d ended %d code %lu\n", ok, at, name(kind), before > 0 && before != (DWORD)-1, (long)after, stop, w == WAIT_OBJECT_0, code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        ResetEvent(ev);
    }

    /* a debug object closed with the flag cleared lets go; one of another thread is not ours */
    {
        HANDLE dbg = NULL;
        typedef NTSTATUS (WINAPI *create_fn)(HANDLE *, ACCESS_MASK, void *, ULONG);
        create_fn pNtCreateDebugObject = (create_fn)GetProcAddress(nt, "NtCreateDebugObject");
        struct { ULONG len; HANDLE root; void *name; ULONG attr; void *sd, *qos; } oa = { sizeof oa, 0, 0, 0, 0, 0 };
        NTSTATUS st = pNtCreateDebugObject(&dbg, 0x1f000f, &oa, 2);
        printf("bad flags: %08lx\n", st);
        st = pNtCreateDebugObject(&dbg, 0x1f000f, &oa, 0);
        pDbgUiSetThreadDebugObject(dbg);
        BOOL ok = start(exe, ev, DEBUG_PROCESS, &pi);
        int first = -1;
        int bp = ok && to_breakpoint(&first);
        drain(100);
        CloseHandle(dbg);
        pDbgUiSetThreadDebugObject(NULL);
        DWORD w = WaitForSingleObject(pi.hProcess, 300);
        SetEvent(ev);
        DWORD w2 = WaitForSingleObject(pi.hProcess, 10000);
        GetExitCodeProcess(pi.hProcess, &code);
        printf("own object: created %08lx breakpoint %d alive %d ended %d code %lu\n", st, bp, w == WAIT_TIMEOUT, w2 == WAIT_OBJECT_0, code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    return 0;
}
