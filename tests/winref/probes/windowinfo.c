/* user32 GetWindowInfo as AGS5 (Ags5.dll) centers its dialogs with it: for top-level
 * windows of several styles, a child and a dialog-like popup, the rectangles (window in screen
 * coordinates, client as offsets from it), styles, WS_ACTIVECAPTION, the border widths, whether
 * atomWindowType is the class atom, the creator version, what cbSize comes back as, and the
 * error for a window that is not there. */
#include "../probe.h"

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) { return DefWindowProcW(h, m, w, l); }

static void show(const char *what, HWND h, ATOM atom, DWORD cb) {
    WINDOWINFO wi;
    memset(&wi, 0xcc, sizeof wi);
    wi.cbSize = cb;
    SetLastError(0xdead);
    BOOL r = GetWindowInfo(h, &wi);
    printf("%s: %d", what, r);
    if (!r) { printf(" err %lu\n", GetLastError()); return; }
    printf(" cb %lu win %ld,%ld,%ld,%ld client+%ld,%ld,%ld,%ld style %08lx ex %08lx status %lu borders %u,%u atom %s ver %04x\n",
           wi.cbSize, wi.rcWindow.left, wi.rcWindow.top, wi.rcWindow.right - wi.rcWindow.left, wi.rcWindow.bottom - wi.rcWindow.top,
           wi.rcClient.left - wi.rcWindow.left, wi.rcClient.top - wi.rcWindow.top,
           wi.rcWindow.right - wi.rcClient.right, wi.rcWindow.bottom - wi.rcClient.bottom,
           wi.dwStyle, wi.dwExStyle, wi.dwWindowStatus, wi.cxWindowBorders, wi.cyWindowBorders,
           wi.atomWindowType == atom ? "class" : "other", wi.wCreatorVersion);
}

int main(void) {
    WNDCLASSW wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"wr_wininfo";
    ATOM atom = RegisterClassW(&wc);
    HWND a = CreateWindowExW(0, L"wr_wininfo", L"a", WS_OVERLAPPEDWINDOW, 100, 100, 300, 200, NULL, NULL, wc.hInstance, NULL);
    show("overlapped hidden", a, atom, sizeof(WINDOWINFO));
    ShowWindow(a, SW_SHOW);
    SetActiveWindow(a);
    show("overlapped shown", a, atom, sizeof(WINDOWINFO));
    show("cbSize 0", a, atom, 0);
    HWND p = CreateWindowExW(WS_EX_TOOLWINDOW, L"wr_wininfo", L"p", WS_POPUP | WS_BORDER, 50, 60, 120, 80, NULL, NULL, wc.hInstance, NULL);
    show("popup border", p, atom, sizeof(WINDOWINFO));
    HWND d = CreateWindowExW(WS_EX_DLGMODALFRAME, L"wr_wininfo", L"d", WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME, 200, 150, 250, 120, a, NULL, wc.hInstance, NULL);
    show("dialog frame", d, atom, sizeof(WINDOWINFO));
    HWND c = CreateWindowExW(WS_EX_CLIENTEDGE, L"wr_wininfo", L"c", WS_CHILD | WS_VISIBLE, 10, 20, 50, 40, a, NULL, wc.hInstance, NULL);
    show("child edge", c, atom, sizeof(WINDOWINFO));
    HWND s = CreateWindowExW(0, L"STATIC", L"s", WS_CHILD | WS_VISIBLE, 0, 0, 30, 10, a, NULL, wc.hInstance, NULL);
    show("static child", s, (ATOM)GetClassLongW(s, GCW_ATOM), sizeof(WINDOWINFO));
    DestroyWindow(c);
    show("destroyed", c, atom, sizeof(WINDOWINFO));
    show("null hwnd", NULL, atom, sizeof(WINDOWINFO));
    DestroyWindow(d); DestroyWindow(p); DestroyWindow(a);
    return 0;
}
