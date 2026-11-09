/* libs: -luser32 */
/* What an edit control tells its parent when its text is set with WM_SETTEXT (as a helper
 * thread filling in msi's dialogs, or a launcher's own code, does): a single-line edit sends
 * EN_UPDATE and EN_CHANGE, a multi-line one neither; EM_SETSEL and WM_GETTEXT send
 * nothing. (sake: user.rs control proc; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>

static int codes[16], n;

static LRESULT CALLBACK proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg == WM_COMMAND && lp && n < 16 && HIWORD(wp) >= 0x100)
        codes[n++] = (LOWORD(wp) << 16) | HIWORD(wp);
    return DefWindowProcA(h, msg, wp, lp);
}

static void report(const char *what) {
    printf("%s:", what);
    for (int i = 0; i < n; i++)
        printf(" %d/%x", codes[i] >> 16, codes[i] & 0xffff);
    printf("\n");
    n = 0;
}

int main(void) {
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "SakeEditNotify";
    RegisterClassA(&wc);
    HWND top = CreateWindowExA(0, "SakeEditNotify", "edit", WS_OVERLAPPEDWINDOW, 10, 10, 300, 200, NULL, NULL, wc.hInstance, NULL);
    HWND one = CreateWindowExA(0, "Edit", "", WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL, 10, 10, 200, 20, top, (HMENU)11, wc.hInstance, NULL);
    HWND multi = CreateWindowExA(0, "Edit", "", WS_CHILD | WS_VISIBLE | ES_MULTILINE, 10, 40, 200, 60, top, (HMENU)12, wc.hInstance, NULL);
    report("created");
    SetWindowTextA(one, "Kota");
    report("single-line WM_SETTEXT");
    SetWindowTextA(one, "Kota");
    report("single-line same text");
    SetWindowTextA(multi, "a\r\nb");
    report("multi-line WM_SETTEXT");
    char b[32];
    GetWindowTextA(one, b, sizeof b);
    SendMessageA(one, EM_SETSEL, 0, -1);
    report("WM_GETTEXT, EM_SETSEL");
    printf("text \"%s\"\n", b);
    DestroyWindow(top);
    return 0;
}
