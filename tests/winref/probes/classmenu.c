/* A class menu: WNDCLASS lpszMenuName (a resource id or a name) gives every top-level window of
 * the class its own copy of that menu when CreateWindow's hMenu is NULL, and nothing to a child.
 * The script engine of エピデミック 〜感染拡大〜 (Black Package TRY) keeps GetMenu(main window) as its
 * menu 0, inserts its items into it and fails ("EnableMenuItemに失敗しました") when that is NULL:
 * sake ignored lpszMenuName, so GetMenu gave NULL. */
#include "../probe.h"

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) { return DefWindowProcA(h, m, w, l); }

static void items(const char *what, HMENU m) {
    printf("%s: menu %d", what, m != NULL);
    if (m) {
        int n = GetMenuItemCount(m);
        printf(" items %d", n);
        for (int i = 0; i < n; i++) {
            char t[64] = "";
            GetMenuStringA(m, i, t, sizeof t, MF_BYPOSITION);
            HMENU sub = GetSubMenu(m, i);
            printf(" [%s id %d state %x sub %d", t, (int)GetMenuItemID(m, i), GetMenuState(m, i, MF_BYPOSITION) & 0xff, sub ? GetMenuItemCount(sub) : -1);
            printf("]");
        }
    }
    printf("\n");
}

static ATOM reg(const char *name, LPCSTR menu) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = name;
    wc.lpszMenuName = menu;
    return RegisterClassA(&wc);
}

int main(void) {
    HINSTANCE inst = GetModuleHandleA(NULL);
    reg("cm_id", MAKEINTRESOURCEA(1));
    reg("cm_name", "NAMEDMENU");
    reg("cm_missing", MAKEINTRESOURCEA(77));
    HWND a = CreateWindowExA(0, "cm_id", "a", WS_OVERLAPPEDWINDOW, 0, 0, 300, 200, NULL, NULL, inst, NULL);
    HWND b = CreateWindowExA(0, "cm_id", "b", WS_POPUP, 0, 0, 300, 200, NULL, NULL, inst, NULL);
    items("overlapped", GetMenu(a));
    items("popup", GetMenu(b));
    printf("own copies: %d\n", GetMenu(a) != GetMenu(b));
    /* the menu bar takes client height (as with SetMenu) */
    RECT w, c;
    GetWindowRect(a, &w);
    GetClientRect(a, &c);
    RECT f = { 0, 0, 0, 0 }, fm = { 0, 0, 0, 0 };
    AdjustWindowRectEx(&f, WS_OVERLAPPEDWINDOW, FALSE, 0);
    AdjustWindowRectEx(&fm, WS_OVERLAPPEDWINDOW, TRUE, 0);
    printf("client takes the bar: %d\n", (w.bottom - w.top) - (c.bottom - c.top) == fm.bottom - fm.top);
    /* the script engine's pattern: items inserted by position into the class menu, then enabled */
    HMENU m = GetMenu(a);
    InsertMenuA(m, 2, MF_BYPOSITION | MF_GRAYED, 104, "added");
    printf("enable by position 2: %x\n", EnableMenuItem(m, 2, MF_BYPOSITION | MF_ENABLED));
    printf("enable by position 3: %x\n", EnableMenuItem(m, 3, MF_BYPOSITION | MF_ENABLED));
    printf("enable 102 by command (in the popup): %x\n", EnableMenuItem(m, 102, MF_BYCOMMAND | MF_ENABLED));
    items("after", m);
    /* hMenu given: that menu, not the class's */
    HMENU own = CreateMenu();
    AppendMenuA(own, MF_STRING, 301, "own");
    HWND d = CreateWindowExA(0, "cm_id", "d", WS_OVERLAPPEDWINDOW, 0, 0, 300, 200, NULL, own, inst, NULL);
    printf("given menu kept: %d\n", GetMenu(d) == own);
    /* by name */
    HWND e = CreateWindowExA(0, "cm_name", "e", WS_OVERLAPPEDWINDOW, 0, 0, 300, 200, NULL, NULL, inst, NULL);
    items("named", GetMenu(e));
    /* a child gets no menu (hMenu is its id) */
    HWND ch = CreateWindowExA(0, "cm_id", "ch", WS_CHILD, 0, 0, 10, 10, a, (HMENU)5, inst, NULL);
    printf("child: menu %d id %d\n", GetMenu(ch) != NULL, GetDlgCtrlID(ch));
    /* a menu resource that isn't there: still a window, no menu */
    HWND g = CreateWindowExA(0, "cm_missing", "g", WS_OVERLAPPEDWINDOW, 0, 0, 300, 200, NULL, NULL, inst, NULL);
    printf("missing: window %d menu %d\n", g != NULL, g && GetMenu(g) != NULL);
    /* DestroyWindow destroys the class menu it made */
    HMENU am = GetMenu(a);
    DestroyWindow(a);
    printf("after destroy: menu valid %d\n", IsMenu(am));
    return 0;
}
