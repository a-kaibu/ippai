/* libs: -luser32 */
/* Enter through IsDialogMessage in a window that is not a dialog (a game's own class with child
 * buttons, as 魔女こいにっき's first-run window-mode window): which WM_COMMAND the window gets
 * with the focus on a plain push button whose id is not IDOK, with an IDOK button, and with a
 * BS_DEFPUSHBUTTON. (sake: user.rs is_dialog_message, default_button, BM_SETSTYLE;
 * compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>

static int ids[8], lps[8], n;
static HWND kids[4];
static LRESULT CALLBACK proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg == WM_COMMAND && n < 8 && HIWORD(wp) == BN_CLICKED) {
        ids[n] = LOWORD(wp);
        lps[n] = lp == 0 ? 0 : (HWND)lp == kids[0] ? 1 : (HWND)lp == kids[1] ? 2 : (HWND)lp == kids[2] ? 3 : 9;
        n++;
    }
    return DefWindowProcA(h, msg, wp, lp);
}
static void pump(HWND d) {
    MSG m;
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE))
        if (!IsDialogMessageA(d, &m)) {
            TranslateMessage(&m);
            DispatchMessageA(&m);
        }
}
static void enter(const char *what, HWND top, HWND to) {
    pump(top);
    n = 0;
    SetFocus(to);
    PostMessageA(to, WM_KEYDOWN, VK_RETURN, 1);
    PostMessageA(to, WM_KEYUP, VK_RETURN, 0xc0000001);
    pump(top);
    printf("%s:", what);
    for (int i = 0; i < n; i++) printf(" %d/%d", ids[i], lps[i]);
    printf("\n");
}

int main(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "SakeProbeEnter";
    RegisterClassA(&wc);
    HWND top = CreateWindowExA(WS_EX_TOPMOST, "SakeProbeEnter", "enter", WS_CAPTION | WS_SYSMENU | WS_VISIBLE, 10, 10, 300, 150, NULL, NULL, wc.hInstance, NULL);
    kids[0] = CreateWindowExA(0, "Button", "check", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX, 30, 40, 200, 20, top, (HMENU)10000, wc.hInstance, NULL);
    kids[1] = CreateWindowExA(0, "Button", "OK", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, 100, 80, 100, 30, top, (HMENU)10001, wc.hInstance, NULL);
    printf("windows %d %d %d\n", top != NULL, kids[0] != NULL, kids[1] != NULL);
    printf("DM_GETDEFID %08lx\n", (unsigned long)SendMessageA(top, DM_GETDEFID, 0, 0));
    enter("plain button, no IDOK", top, kids[1]);
    enter("checkbox, no IDOK", top, kids[0]);
    kids[2] = CreateWindowExA(0, "Button", "1", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, 200, 80, 50, 30, top, (HMENU)IDOK, wc.hInstance, NULL);
    enter("plain button, IDOK button", top, kids[1]);
    SendMessageA(kids[1], BM_SETSTYLE, BS_DEFPUSHBUTTON, FALSE);
    enter("default button focused", top, kids[1]);
    enter("checkbox, default button", top, kids[0]);
    DestroyWindow(top);
    return 0;
}
