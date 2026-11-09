/* When a window's menu draws a bar: GetClientRect against the frame AdjustWindowRectEx gives
 * without one. 俺の姿が､透明に! (KiriKiri Z) gives TVPMainWindow a menu with no items; on Windows
 * its client is the whole 1920x1080 inside the 1936x1119 window, and sake gave it 1920x1060 (a
 * bar for the empty menu). Also: items added later (before and after DrawMenuBar), the last one
 * deleted, SetMenu of an empty menu and of NULL, and the WM_SIZEs each sends. */
#include "../probe.h"

static int sizes;
static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_SIZE) sizes++;
    return DefWindowProcA(h, m, w, l);
}

static const DWORD style = WS_OVERLAPPEDWINDOW;
static int frame_h(void) {
    RECT r = { 0, 0, 0, 0 };
    AdjustWindowRectEx(&r, style, FALSE, 0);
    return r.bottom - r.top;
}
/* 1 if the window's client is shorter than its frame alone leaves */
static void show(const char *what, HWND h) {
    RECT w, c;
    GetWindowRect(h, &w);
    GetClientRect(h, &c);
    int bar = (w.bottom - w.top) - (c.bottom - c.top) - frame_h();
    RECT a = { 0, 0, 0, 0 };
    AdjustWindowRectEx(&a, style, TRUE, 0);
    int full = (a.bottom - a.top) - frame_h();
    HMENU m = GetMenu(h);
    printf("%s: menu %d items %d bar %s width %d sizes %d\n", what, m != NULL, m ? GetMenuItemCount(m) : -1,
           bar == 0 ? "none" : bar == full ? "one line" : bar > 0 ? "taller" : "negative", (int)(c.right - c.left) == (int)(w.right - w.left) - (a.right - a.left), sizes);
    sizes = 0;
}

int main(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "menubar";
    RegisterClassA(&wc);
    HWND none = CreateWindowExA(0, "menubar", "none", style, 10, 10, 400, 300, NULL, NULL, wc.hInstance, NULL);
    show("no menu", none);

    HMENU empty = CreateMenu();
    HWND e = CreateWindowExA(0, "menubar", "empty", style, 10, 10, 400, 300, NULL, empty, wc.hInstance, NULL);
    show("empty menu at create", e);
    AppendMenuA(empty, MF_STRING, 1, "File");
    show("an item added", e);
    DrawMenuBar(e);
    show("then DrawMenuBar", e);
    DeleteMenu(empty, 0, MF_BYPOSITION);
    show("the item deleted", e);
    DrawMenuBar(e);
    show("then DrawMenuBar", e);
    SetWindowPos(e, NULL, 0, 0, 500, 300, SWP_NOMOVE | SWP_NOZORDER);
    show("resized", e);

    HMENU one = CreateMenu();
    AppendMenuA(one, MF_STRING, 1, "File");
    HWND o = CreateWindowExA(0, "menubar", "one", style, 10, 10, 400, 300, NULL, one, wc.hInstance, NULL);
    show("one item at create", o);
    HMENU empty2 = CreateMenu();
    SetMenu(o, empty2);
    show("SetMenu empty", o);
    SetMenu(o, one);
    show("SetMenu one item", o);
    SetMenu(o, NULL);
    show("SetMenu NULL", o);
    SetMenu(none, empty2);
    show("SetMenu empty on no menu", none);

    DestroyWindow(none);
    DestroyWindow(e);
    DestroyWindow(o);
    return 0;
}
