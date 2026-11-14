/* A debugger hears of the DLLs its debuggee loads and frees (sake: dbg.rs): LOAD_DLL with
 * a base GetMappedFileName names, UNLOAD_DLL for it when its last reference goes.
 * Expected output as on Windows. libs: -lpsapi */
#include <windows.h>
#include <psapi.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static int child(void) {
    HMODULE m = LoadLibraryA("ole32.dll");
    FreeLibrary(m);
    return 7;
}

static void base_name(HANDLE p, void *base, WCHAR *out) {
    WCHAR m[MAX_PATH], *s;
    if (!GetMappedFileNameW(p, base, m, MAX_PATH)) m[0] = 0;
    s = wcsrchr(m, '\\');
    wcscpy(out, s ? s + 1 : m);
    CharLowerW(out);
}

int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "child")) return child();
    char cmd[MAX_PATH + 16];
    STARTUPINFOA si = { sizeof si };
    PROCESS_INFORMATION pi;
    GetModuleFileNameA(NULL, cmd + 1, MAX_PATH);
    cmd[0] = '"';
    strcat(cmd, "\" child");
    if (!CreateProcessA(NULL, cmd, NULL, NULL, FALSE, DEBUG_PROCESS, NULL, NULL, &si, &pi)) return 1;
    void *ole32 = NULL;
    int ole32_load = 0, ole32_unload = 0, kernel32 = 0, named = 1;
    DWORD code = 0;
    for (;;) {
        DEBUG_EVENT ev;
        if (!WaitForDebugEvent(&ev, 20000)) { printf("timed out\n"); break; }
        if (ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT) { code = ev.u.ExitProcess.dwExitCode; break; }
        if (ev.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT) CloseHandle(ev.u.CreateProcessInfo.hFile);
        if (ev.dwDebugEventCode == LOAD_DLL_DEBUG_EVENT) {
            WCHAR n[MAX_PATH];
            if (ev.u.LoadDll.hFile) CloseHandle(ev.u.LoadDll.hFile);
            base_name(pi.hProcess, ev.u.LoadDll.lpBaseOfDll, n);
            if (!n[0]) named = 0;
            if (!wcscmp(n, L"ole32.dll")) { ole32 = ev.u.LoadDll.lpBaseOfDll; ole32_load++; }
            if (!wcscmp(n, L"kernel32.dll")) kernel32++;
        }
        if (ev.dwDebugEventCode == UNLOAD_DLL_DEBUG_EVENT) {
            if (ev.u.UnloadDll.lpBaseOfDll == ole32) ole32_unload++;
        }
        ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
    }
    printf("child %lu kernel32 %d all named %d\n", code, kernel32, named);
    printf("ole32 loaded %d unloaded %d\n", ole32_load, ole32_unload);
    return 0;
}
