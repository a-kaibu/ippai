/* libs: -ladvapi32 -luser32 */
/* Window-procedure hooks around sent messages, and CreateWellKnownSid's numbering.
 * pgsv.dll (Softhouse-chara's AGS5 engine: その古城に勇者砲あり！, 悪魔聖女 体験版) sets a
 * WH_CALLWNDPROC hook on the game window's thread and sends its own message 0x7771 to that
 * window: the hook answers it (the window procedure doesn't know it); unanswered, PGSV_Start
 * goes on with a NULL instance and faults. */
#include <windows.h>
#include <sddl.h>
#include <stdio.h>
static int failed;
#define CHECK(label, expr) do { int ok_ = !!(expr); printf("%s: %s\n", label, ok_ ? "ok" : "FAIL"); failed |= !ok_; } while (0)

static LRESULT CALLBACK wndproc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == 0x7771) return 0;  /* (not this window's message: pgsv's hook answers it) */
    if (m == WM_USER + 5) return 1234;
    return DefWindowProcW(h, m, w, l);
}
static int seen, seen_ret, seen_wparam = -1;
static LRESULT seen_result;
static HWND hwnd_;
static HHOOK hook, hook_ret;
static LRESULT CALLBACK callwnd(int code, WPARAM w, LPARAM l) {
    CWPSTRUCT *c = (CWPSTRUCT *)l;
    if (code == HC_ACTION && c->hwnd == hwnd_ && c->message == 0x7771) {
        seen++;
        seen_wparam = w != 0;
        *(DWORD *)c->lParam = 0x5a5a0000 | (DWORD)c->wParam;
    }
    return CallNextHookEx(hook, code, w, l);
}
static LRESULT CALLBACK callwndret(int code, WPARAM w, LPARAM l) {
    CWPRETSTRUCT *c = (CWPRETSTRUCT *)l;
    if (code == HC_ACTION && c->hwnd == hwnd_ && c->message == WM_USER + 5) {
        seen_ret++;
        seen_result = c->lResult;
    }
    return CallNextHookEx(hook_ret, code, w, l);
}
static DWORD WINAPI other(void *p) {
    DWORD v = 0;
    SendMessageW(hwnd_, 0x7771, 9, (LPARAM)&v);
    *(DWORD *)p = v;
    return 0;
}

int main(void) {
    /* WELL_KNOWN_SID_TYPE: WinServiceSid 12 = S-1-5-6, WinAnonymousSid 13 = S-1-5-7 */
    for (int t = 0; t <= 28; t++) {
        if (t == 20 || t == 21) continue;  /* (remote logon / logon ids: per logon) */
        BYTE sid[SECURITY_MAX_SID_SIZE];
        DWORD n = sizeof sid;
        LPWSTR s = NULL;
        SetLastError(0xdead);
        BOOL ok = CreateWellKnownSid((WELL_KNOWN_SID_TYPE)t, NULL, sid, &n);
        if (ok) ConvertSidToStringSidW(sid, &s);
        printf("well-known %d: %d %ls len %lu\n", t, ok, s ? s : L"-", ok ? n : 0);
        if (s) LocalFree(s);
    }

    /* WH_CALLWNDPROC / WH_CALLWNDPROCRET around sent messages */
    {
        WNDCLASSW wc = { 0 };
        wc.lpfnWndProc = wndproc;
        wc.hInstance = GetModuleHandleW(NULL);
        wc.lpszClassName = L"accounts-hook";
        RegisterClassW(&wc);
        hwnd_ = CreateWindowW(L"accounts-hook", L"", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, wc.hInstance, NULL);
        hook = SetWindowsHookExW(WH_CALLWNDPROC, callwnd, NULL, GetWindowThreadProcessId(hwnd_, NULL));
        hook_ret = SetWindowsHookExW(WH_CALLWNDPROCRET, callwndret, NULL, GetCurrentThreadId());
        CHECK("hooks set", hook && hook_ret);
        DWORD v = 0;
        SendMessageW(hwnd_, 0x7771, 7, (LPARAM)&v);
        printf("hook answered %08lx seen %d from this thread %d\n", v, seen, seen_wparam);
        LRESULT r = SendMessageW(hwnd_, WM_USER + 5, 0, 0);
        printf("returned %ld; ret hook seen %d result %ld\n", (long)r, seen_ret, (long)seen_result);
        /* posted messages pass no WH_CALLWNDPROC hook */
        PostMessageW(hwnd_, 0x7771, 8, (LPARAM)&v);
        MSG m;
        while (PeekMessageW(&m, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&m);
        printf("after a posted one: seen %d\n", seen);
        /* sent from another thread: the hook runs on the window's thread, wParam 0 */
        DWORD got = 0;
        HANDLE t = CreateThread(NULL, 0, other, &got, 0, NULL);
        while (MsgWaitForMultipleObjects(1, &t, FALSE, 5000, QS_SENDMESSAGE) == WAIT_OBJECT_0 + 1)
            PeekMessageW(&m, NULL, 0, 0, PM_NOREMOVE);
        CloseHandle(t);
        printf("other thread: %08lx seen %d from this thread %d\n", got, seen, seen_wparam);
        UnhookWindowsHookEx(hook);
        UnhookWindowsHookEx(hook_ret);
        v = 0;
        SendMessageW(hwnd_, 0x7771, 7, (LPARAM)&v);
        printf("unhooked: %08lx seen %d\n", v, seen);
        DestroyWindow(hwnd_);
    }
    printf("%s\n", failed ? "FAILED" : "all ok");
    return failed;
}
