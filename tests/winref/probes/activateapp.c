/* Who hears WM_ACTIVATEAPP (#189, #204): Delphi programs keep a hidden top-level window of their
 * own (TApplication's), and only its WM_ACTIVATEAPP fires Application.OnActivate; LiveMaker
 * titles drop clicks until it does. The windows a Delphi 5 program has: the application window
 * (hidden, 0x0), a hidden utility window, a message-only window, the main form (owned by the
 * application window) with a child, then a window of a second thread taking the activation and
 * the form taking it back. Per window, the WM_ACTIVATEAPP / WM_NCACTIVATE / WM_ACTIVATE /
 * WM_SETFOCUS / WM_KILLFOCUS it got, in order, with wParam and whose thread lParam names; then
 * whether every WM_ACTIVATEAPP came before the form's WM_ACTIVATE.
 * (Windows activates only from the foreground: without it the probe says skip.) */
#include "../probe.h"

enum { APP, UTIL, MSGONLY, FORM, CHILD, OTHER, NWIN };
static const char *names[NWIN] = { "app", "util", "msgonly", "form", "child", "other" };
static HWND wins[NWIN];
static DWORD main_tid, other_tid;
static char logs[NWIN][1024];
static int seq, app_seq_max, form_activate_seq;
static HANDLE other_ready, other_go, other_done;

static int which(HWND h) {
    for (int i = 0; i < NWIN; i++) if (wins[i] == h) return i;
    return -1;
}
static const char *tidname(LPARAM l) {
    if ((DWORD)l == 0) return "0";
    if ((DWORD)l == main_tid) return "main";
    if ((DWORD)l == other_tid) return "other";
    return "?";
}
static void note(HWND h, const char *s) {
    int i = which(h);
    if (i < 0) return;
    size_t n = strlen(logs[i]);
    snprintf(logs[i] + n, sizeof logs[i] - n, " %s", s);
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    char b[64];
    switch (m) {
    case WM_ACTIVATEAPP:
        snprintf(b, sizeof b, "APP(%d,%s)", w ? 1 : 0, tidname(l));
        note(h, b);
        app_seq_max = ++seq;
        break;
    case WM_NCACTIVATE: snprintf(b, sizeof b, "NC(%d)", w ? 1 : 0); note(h, b); break;
    case WM_ACTIVATE:
        snprintf(b, sizeof b, "ACT(%d)", (int)LOWORD(w));
        note(h, b);
        if (h == wins[FORM] && LOWORD(w) && !form_activate_seq) form_activate_seq = ++seq;
        break;
    case WM_SETFOCUS: note(h, "SETFOCUS"); break;
    case WM_KILLFOCUS: note(h, "KILLFOCUS"); break;
    }
    return DefWindowProcA(h, m, w, l);
}

static void pump(void) {
    MSG msg;
    DWORD end = GetTickCount() + 300;
    while ((int)(end - GetTickCount()) > 0) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&msg);
        MsgWaitForMultipleObjects(0, NULL, FALSE, 20, QS_ALLINPUT);
    }
}

static DWORD WINAPI other(void *arg) {
    (void)arg;
    wins[OTHER] = CreateWindowExA(0, "wr_actapp", "other", WS_OVERLAPPEDWINDOW, 300, 100, 200, 150, NULL, NULL, GetModuleHandleA(NULL), NULL);
    SetEvent(other_ready);
    WaitForSingleObject(other_go, INFINITE);
    ShowWindow(wins[OTHER], SW_SHOW);
    SetForegroundWindow(wins[OTHER]);
    pump();
    SetEvent(other_done);
    /* (answers the form taking the activation back) */
    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0) DispatchMessageA(&msg);
    return 0;
}

int main(void) {
    main_tid = GetCurrentThreadId();
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "wr_actapp";
    RegisterClassA(&wc);
    wins[APP] = CreateWindowExA(0, "wr_actapp", "app", WS_POPUP | WS_CAPTION | WS_CLIPSIBLINGS | WS_SYSMENU | WS_MINIMIZEBOX, 400, 300, 0, 0, NULL, NULL, wc.hInstance, NULL);
    wins[UTIL] = CreateWindowExA(WS_EX_TOOLWINDOW, "wr_actapp", "util", WS_POPUP, 0, 0, 0, 0, NULL, NULL, wc.hInstance, NULL);
    wins[MSGONLY] = CreateWindowExA(0, "wr_actapp", "msgonly", 0, 0, 0, 0, 0, HWND_MESSAGE, NULL, wc.hInstance, NULL);
    wins[FORM] = CreateWindowExA(0, "wr_actapp", "form", WS_OVERLAPPEDWINDOW, 100, 100, 300, 200, wins[APP], NULL, wc.hInstance, NULL);
    wins[CHILD] = CreateWindowExA(0, "wr_actapp", "child", WS_CHILD | WS_VISIBLE, 0, 0, 50, 50, wins[FORM], NULL, wc.hInstance, NULL);
    other_ready = CreateEventA(NULL, TRUE, FALSE, NULL);
    other_go = CreateEventA(NULL, TRUE, FALSE, NULL);
    other_done = CreateEventA(NULL, TRUE, FALSE, NULL);
    HANDLE t = CreateThread(NULL, 0, other, NULL, 0, &other_tid);
    WaitForSingleObject(other_ready, INFINITE);

    ShowWindow(wins[FORM], SW_SHOW);
    SetForegroundWindow(wins[FORM]);
    pump();
    int fg = GetForegroundWindow() == wins[FORM];
    if (!fg) {
        printf("skip the probe cannot take the foreground\n");
        return 0;
    }
    char first[NWIN][1024];
    memcpy(first, logs, sizeof logs);
    int ordered = form_activate_seq && app_seq_max < form_activate_seq;
    memset(logs, 0, sizeof logs);

    SetEvent(other_go);
    while (MsgWaitForMultipleObjects(1, &other_done, FALSE, INFINITE, QS_ALLINPUT) != WAIT_OBJECT_0) pump();
    pump();
    char second[NWIN][1024];
    memcpy(second, logs, sizeof logs);
    memset(logs, 0, sizeof logs);

    SetForegroundWindow(wins[FORM]);
    pump();

    printf("[form shown and made foreground]\n");
    for (int i = 0; i < NWIN; i++) printf("%s:%s\n", names[i], first[i]);
    printf("activateapp before the form's activate %d\n", ordered);
    printf("[the other thread's window takes it]\n");
    for (int i = 0; i < NWIN; i++) printf("%s:%s\n", names[i], second[i]);
    printf("[the form takes it back]\n");
    for (int i = 0; i < NWIN; i++) printf("%s:%s\n", names[i], logs[i]);
    printf("foreground is form %d active is form %d\n", GetForegroundWindow() == wins[FORM], GetActiveWindow() == wins[FORM]);
    PostThreadMessageA(other_tid, WM_QUIT, 0, 0);
    WaitForSingleObject(t, 2000);
    return 0;
}
