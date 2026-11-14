/* Processes talking to each other, as DRM runtimes (ソフト電池: the game, its wrapper, the
 * runtime's service and meter) and launchers do: CreateProcess (suspended, exit codes,
 * TerminateProcess, OpenProcess by id, a missing program), inherited anonymous pipes as the
 * child's stdin/stdout, a named pipe served by one process and opened by another, named events
 * and mutexes (abandoned by a child that exits holding it), and window messages across
 * processes (FindWindow, SendMessage, WM_COPYDATA, PostMessage). The parent prints; children
 * answer only through pipes, events and exit codes. */
#include "../probe.h"
#include <ctype.h>

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-70s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)
#define PIPE "\\\\.\\pipe\\masu_procs_probe"

static int child(const char *mode) {
    char buf[64];
    DWORD n;
    if (!strncmp(mode, "exit ", 5)) return atoi(mode + 5);
    if (!strcmp(mode, "upper")) {
        HANDLE in = GetStdHandle(STD_INPUT_HANDLE), out = GetStdHandle(STD_OUTPUT_HANDLE);
        while (ReadFile(in, buf, sizeof buf, &n, NULL) && n) {
            for (DWORD i = 0; i < n; i++) buf[i] = (char)toupper((unsigned char)buf[i]);
            WriteFile(out, buf, n, &n, NULL);
        }
        return GetLastError() == ERROR_BROKEN_PIPE ? 3 : 4;
    }
    if (!strcmp(mode, "pipe")) {
        if (!WaitNamedPipeA(PIPE, 10000)) return 10;
        HANDLE p = CreateFileA(PIPE, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
        if (p == INVALID_HANDLE_VALUE) return 11;
        DWORD mode = PIPE_READMODE_MESSAGE;
        if (!SetNamedPipeHandleState(p, &mode, NULL, NULL)) return 12;
        if (!WriteFile(p, "ping", 4, &n, NULL)) return 13;
        memset(buf, 0, sizeof buf);
        if (!ReadFile(p, buf, sizeof buf, &n, NULL)) return 14;
        return strcmp(buf, "pong") ? 15 : 0;
    }
    if (!strcmp(mode, "event")) {
        HANDLE e = OpenEventA(EVENT_MODIFY_STATE | SYNCHRONIZE, FALSE, "masu_procs_event");
        if (!e) return 20;
        HANDLE back = OpenEventA(SYNCHRONIZE, FALSE, "masu_procs_back");
        SetEvent(e);
        return back && WaitForSingleObject(back, 10000) == WAIT_OBJECT_0 ? 21 : 22;
    }
    if (!strcmp(mode, "mutex")) {
        HANDLE m = OpenMutexA(SYNCHRONIZE, FALSE, "masu_procs_mutex");
        return m ? (int)WaitForSingleObject(m, 0) : 30;
    }
    if (!strcmp(mode, "window")) {
        HWND w = FindWindowA("masu_procs_class", "masu procs");
        if (!w) return 40;
        COPYDATASTRUCT cd = { 7, 5, "copy" };
        LRESULT r = SendMessageA(w, WM_COPYDATA, 0, (LPARAM)&cd);
        PostMessageA(w, WM_USER + 2, 5, 6);
        return (int)SendMessageA(w, WM_USER + 1, 21, 0) + (r == 99 ? 0 : 1000);
    }
    return 99;
}

static char self[MAX_PATH];
static HANDLE spawn(const char *mode, DWORD flags, STARTUPINFOA *si, BOOL inherit, PROCESS_INFORMATION *pi) {
    char cmd[MAX_PATH + 64];
    STARTUPINFOA def = { sizeof def };
    snprintf(cmd, sizeof cmd, "\"%s\" child %s", self, mode);
    SetLastError(0xdead);
    BOOL ok = CreateProcessA(NULL, cmd, NULL, NULL, inherit, flags, NULL, NULL, si ? si : &def, pi);
    printf("spawn %s: %d err %lu\n", mode, ok, GetLastError());
    return ok ? pi->hProcess : NULL;
}
static void finish(PROCESS_INFORMATION *pi) {
    DWORD code = 0xdead;
    E(WaitForSingleObject(pi->hProcess, 20000));
    E(GetExitCodeProcess(pi->hProcess, &code));
    printf("exit code %lu\n", code);
    CloseHandle(pi->hThread);
    CloseHandle(pi->hProcess);
}

static char copied[16];
static LONG posted;
static LRESULT CALLBACK wndproc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_USER + 1) return w * 2;
    if (m == WM_USER + 2) { posted = (LONG)(w * 10 + l); return 0; }
    if (m == WM_COPYDATA) {
        COPYDATASTRUCT *cd = (COPYDATASTRUCT *)l;
        snprintf(copied, sizeof copied, "%lu %lu %.4s", (unsigned long)cd->dwData, cd->cbData, (char *)cd->lpData);
        return 99;
    }
    return DefWindowProcA(h, m, w, l);
}

int main(int argc, char **argv) {
    if (argc > 2 && !strcmp(argv[1], "child")) {
        char mode[64];
        snprintf(mode, sizeof mode, "%s%s%s", argv[2], argc > 3 ? " " : "", argc > 3 ? argv[3] : "");
        return child(mode);
    }
    GetModuleFileNameA(NULL, self, sizeof self);
    PROCESS_INFORMATION pi;
    DWORD code;

    /* exit codes, suspended start, ids */
    spawn("exit 42", CREATE_SUSPENDED, NULL, FALSE, &pi);
    code = 0xdead;
    E(GetExitCodeProcess(pi.hProcess, &code));
    printf("before %lu\n", code);
    E(GetProcessId(pi.hProcess) == pi.dwProcessId);
    E(pi.dwProcessId != GetCurrentProcessId());
    HANDLE op = OpenProcess(PROCESS_QUERY_INFORMATION | SYNCHRONIZE, FALSE, pi.dwProcessId);
    E(op != NULL);
    E(WaitForSingleObject(op, 0));
    E(ResumeThread(pi.hThread));
    E(WaitForSingleObject(op, 20000));
    code = 0xdead;
    E(GetExitCodeProcess(op, &code));
    printf("through OpenProcess %lu\n", code);
    CloseHandle(op);
    finish(&pi);
    spawn("exit 1", CREATE_SUSPENDED, NULL, FALSE, &pi);
    E(TerminateProcess(pi.hProcess, 9));
    finish(&pi);
    STARTUPINFOA si0 = { sizeof si0 };
    SetLastError(0xdead);
    E(CreateProcessA("C:\\no\\such.exe", NULL, NULL, NULL, FALSE, 0, NULL, NULL, &si0, &pi));
    E(OpenProcess(SYNCHRONIZE, FALSE, 0xfffffff0) != NULL);

    /* inherited pipes as stdin and stdout */
    SECURITY_ATTRIBUTES sa = { sizeof sa, NULL, TRUE };
    HANDLE inr, inw, outr, outw;
    E(CreatePipe(&inr, &inw, &sa, 0));
    E(CreatePipe(&outr, &outw, &sa, 0));
    SetHandleInformation(inw, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(outr, HANDLE_FLAG_INHERIT, 0);
    STARTUPINFOA si = { sizeof si };
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = inr;
    si.hStdOutput = outw;
    si.hStdError = outw;
    spawn("upper", 0, &si, TRUE, &pi);
    CloseHandle(inr);
    CloseHandle(outw);
    DWORD n;
    E(WriteFile(inw, "hello, ", 7, &n, NULL));
    E(WriteFile(inw, "child", 5, &n, NULL));
    CloseHandle(inw);
    char out[64] = { 0 };
    DWORD total = 0;
    while (total < sizeof out - 1 && ReadFile(outr, out + total, sizeof out - 1 - total, &n, NULL) && n) total += n;
    printf("child wrote '%s' then err %lu\n", out, GetLastError());
    CloseHandle(outr);
    finish(&pi);

    /* a named pipe across processes */
    HANDLE s = CreateNamedPipeA(PIPE, PIPE_ACCESS_DUPLEX, PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE, 1, 512, 512, 0, NULL);
    E(s != INVALID_HANDLE_VALUE);
    spawn("pipe", 0, NULL, FALSE, &pi);
    SetLastError(0xdead);
    BOOL c = ConnectNamedPipe(s, NULL);
    printf("connect %d err %lu\n", c || GetLastError() == ERROR_PIPE_CONNECTED, c ? 0 : GetLastError() == ERROR_PIPE_CONNECTED ? 0 : GetLastError());
    char buf[64] = { 0 };
    E(ReadFile(s, buf, sizeof buf, &n, NULL));
    printf("server got '%s'\n", buf);
    E(WriteFile(s, "pong", 5, &n, NULL));
    finish(&pi);
    E(ReadFile(s, buf, sizeof buf, &n, NULL)); /* the client exited */
    E(DisconnectNamedPipe(s));
    CloseHandle(s);

    /* named events */
    HANDLE ev = CreateEventA(NULL, FALSE, FALSE, "masu_procs_event");
    HANDLE back = CreateEventA(NULL, TRUE, FALSE, "masu_procs_back");
    spawn("event", 0, NULL, FALSE, &pi);
    E(WaitForSingleObject(ev, 20000));
    E(SetEvent(back));
    finish(&pi);

    /* a named mutex: held here, then abandoned by a child */
    HANDLE mx = CreateMutexA(NULL, TRUE, "masu_procs_mutex");
    spawn("mutex", 0, NULL, FALSE, &pi);
    finish(&pi);
    E(ReleaseMutex(mx));
    spawn("mutex", 0, NULL, FALSE, &pi);
    finish(&pi);
    E(WaitForSingleObject(mx, 0));
    E(ReleaseMutex(mx));
    CloseHandle(mx);

    /* window messages from another process */
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "masu_procs_class";
    RegisterClassA(&wc);
    HWND w = CreateWindowA("masu_procs_class", "masu procs", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, wc.hInstance, NULL);
    E(w != NULL);
    spawn("window", 0, NULL, FALSE, &pi);
    for (;;) {
        DWORD r = MsgWaitForMultipleObjects(1, &pi.hProcess, FALSE, 20000, QS_ALLINPUT);
        if (r != WAIT_OBJECT_0 + 1) break;
        MSG msg;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
    }
    MSG msg;
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
    finish(&pi);
    printf("copied '%s' posted %ld\n", copied, posted);
    DestroyWindow(w);
    return 0;
}
