/* The handles a debugger is given with CREATE_PROCESS / CREATE_THREAD debug events (sake:
 * dbg.rs, review b05). ContinueDebugEvent after a thread's EXIT_THREAD event closes the
 * thread handle its CREATE_THREAD event gave; after EXIT_PROCESS the process and main-thread
 * handles go too (kernel32 keeps them in the thread's DbgSsReserved list). A debugger of a
 * process that makes threads all along kept one handle more per thread for good in sake.
 * Also: an OutputDebugString's string reaches the debugger with its length, and the
 * exception record of a RaiseException with 15 parameters fills EXCEPTION_DEBUG_INFO without
 * running past it (the guard after the DEBUG_EVENT stays). */
#include "../probe.h"

static DWORD WINAPI quick(void *arg) {
    (void)arg;
    return 3;
}

static LONG CALLBACK swallow(EXCEPTION_POINTERS *e) {
    return e->ExceptionRecord->ExceptionCode == 0xe0001234 ? EXCEPTION_CONTINUE_EXECUTION : EXCEPTION_CONTINUE_SEARCH;
}

static int child(void) {
    for (int i = 0; i < 3; i++) {
        HANDLE t = CreateThread(NULL, 0, quick, NULL, 0, NULL);
        WaitForSingleObject(t, INFINITE);
        CloseHandle(t);
    }
    OutputDebugStringA("debughandles: hello");
    ULONG_PTR params[EXCEPTION_MAXIMUM_PARAMETERS];
    for (int i = 0; i < EXCEPTION_MAXIMUM_PARAMETERS; i++) params[i] = 0x1000 + i;
    AddVectoredExceptionHandler(1, swallow);
    RaiseException(0xe0001234, 0, EXCEPTION_MAXIMUM_PARAMETERS, params);
    return 9;
}

static int open_handle(HANDLE h) {
    DWORD f;
    return GetHandleInformation(h, &f) != 0;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "child")) return child();
    char cmd[MAX_PATH + 16];
    char self[MAX_PATH];
    GetModuleFileNameA(NULL, self, MAX_PATH);
    snprintf(cmd, sizeof cmd, "\"%s\" child", self);
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    if (!CreateProcessA(NULL, cmd, NULL, NULL, FALSE, DEBUG_ONLY_THIS_PROCESS, NULL, NULL, &si, &pi)) {
        printf("skip CreateProcess %lu\n", GetLastError());
        return 0;
    }
    HANDLE threads[16] = { 0 };
    DWORD tids[16] = { 0 };
    int nthreads = 0, exits = 0, closed_after_exit = 0, open_before_continue = 0;
    HANDLE proc = NULL, main_thread = NULL;
    struct { DEBUG_EVENT ev; unsigned char guard[64]; } b;
    int guard_ok = 1;
    for (;;) {
        memset(b.guard, 0xa5, sizeof b.guard);
        if (!WaitForDebugEvent(&b.ev, 20000)) {
            printf("wait failed %lu\n", GetLastError());
            break;
        }
        for (int i = 0; i < (int)sizeof b.guard; i++) guard_ok &= b.guard[i] == 0xa5;
        DEBUG_EVENT *ev = &b.ev;
        DWORD st = DBG_CONTINUE;
        HANDLE exited = NULL;
        switch (ev->dwDebugEventCode) {
        case CREATE_PROCESS_DEBUG_EVENT:
            if (ev->u.CreateProcessInfo.hFile) CloseHandle(ev->u.CreateProcessInfo.hFile);
            proc = ev->u.CreateProcessInfo.hProcess;
            main_thread = ev->u.CreateProcessInfo.hThread;
            break;
        case LOAD_DLL_DEBUG_EVENT:
            if (ev->u.LoadDll.hFile) CloseHandle(ev->u.LoadDll.hFile);
            break;
        case CREATE_THREAD_DEBUG_EVENT:
            if (nthreads < 16) {
                threads[nthreads] = ev->u.CreateThread.hThread;
                tids[nthreads++] = ev->dwThreadId;
            }
            break;
        case EXIT_THREAD_DEBUG_EVENT:
            for (int i = 0; i < nthreads; i++)
                if (tids[i] == ev->dwThreadId) exited = threads[i];
            if (exited) {
                exits++;
                open_before_continue += open_handle(exited);
            }
            break;
        case OUTPUT_DEBUG_STRING_EVENT: {
            char s[64] = { 0 };
            SIZE_T got = 0;
            WORD n = ev->u.DebugString.nDebugStringLength;
            ReadProcessMemory(pi.hProcess, ev->u.DebugString.lpDebugStringData, s, n < 63 ? n : 63, &got);
            printf("output: \"%s\" length %u unicode %u\n", s, n, ev->u.DebugString.fUnicode);
            break;
        }
        case EXCEPTION_DEBUG_EVENT: {
            EXCEPTION_RECORD *r = &ev->u.Exception.ExceptionRecord;
            if (r->ExceptionCode == 0xe0001234) {
                printf("exception: params %lu first %lu last %lx first chance %lu\n", r->NumberParameters, (unsigned long)r->ExceptionInformation[0] - 0x1000,
                       (unsigned long)r->ExceptionInformation[EXCEPTION_MAXIMUM_PARAMETERS - 1], ev->u.Exception.dwFirstChance);
                st = DBG_EXCEPTION_NOT_HANDLED;
            }
            break;
        }
        }
        DWORD code = ev->dwDebugEventCode;
        ContinueDebugEvent(ev->dwProcessId, ev->dwThreadId, st);
        if (exited) closed_after_exit += !open_handle(exited);
        if (code == EXIT_PROCESS_DEBUG_EVENT) break;
    }
    printf("threads %d exits %d open before continue %d closed after continue %d\n", nthreads, exits, open_before_continue, closed_after_exit);
    printf("after exit process: process handle open %d main thread handle open %d\n", open_handle(proc), open_handle(main_thread));
    printf("guard after DEBUG_EVENT intact %d\n", guard_ok);
    DWORD ec = 0;
    WaitForSingleObject(pi.hProcess, 10000);
    GetExitCodeProcess(pi.hProcess, &ec);
    printf("child exit %lu\n", ec);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return 0;
}
