/* A dialog template's menu (DLGTEMPLATE(EX) menu, by number or by name): the dialog manager loads
 * it, gives the dialog its menu bar and sizes the window to the template's client area plus that
 * bar. タイムリープ (fwing) makes such a captionless popup dialog the parent of its D3D9 window and
 * moves it SM_CYMENU above the screen so the bar is hidden: sake dropped the menu, the client
 * area began 20 pixels higher, the game's frames were shown off by the bar and the window gray
 * (#192). */
#include "../probe.h"

static INT_PTR CALLBACK dproc(HWND h, UINT m, WPARAM w, LPARAM l) { return m == WM_INITDIALOG; }

static void show(int id) {
    HWND d = CreateDialogParamA(GetModuleHandleA(NULL), MAKEINTRESOURCEA(id), NULL, dproc, 0);
    if (!d) {
        printf("dialog %d: none (%lu)\n", id, GetLastError());
        return;
    }
    RECT wr, cr;
    GetWindowRect(d, &wr);
    GetClientRect(d, &cr);
    POINT o = { 0, 0 };
    ClientToScreen(d, &o);
    HMENU m = GetMenu(d);
    int n = m ? GetMenuItemCount(m) : -1;
    char t[32] = "";
    if (m) GetMenuStringA(m, 0, t, sizeof t, MF_BYPOSITION);
    printf("dialog %d: menu %d items %d first '%s' client top %ld left %ld frame w %ld h %ld\n", id, m != NULL, n, t,
           o.y - wr.top, o.x - wr.left, (wr.right - wr.left) - cr.right, (wr.bottom - wr.top) - cr.bottom);
    /* moved like タイムリープ's: the window SM_CYMENU above the screen, 640x480 + the bar */
    int my = GetSystemMetrics(SM_CYMENU);
    int fh = (wr.bottom - wr.top) - cr.bottom;
    MoveWindow(d, 0, -my, 640, 480 + fh, FALSE);
    GetClientRect(d, &cr);
    o.x = o.y = 0;
    ClientToScreen(d, &o);
    printf("  moved: client %ldx%ld at screen %ld,%ld (SM_CYMENU %d, client top - SM_CYMENU %ld)\n", cr.right, cr.bottom, o.x, o.y, my, o.y + my);
    /* a child window filling the client area sits where the client begins */
    HWND c = CreateWindowExA(0, "STATIC", "", WS_CHILD, 0, 0, 640, 480, d, NULL, NULL, NULL);
    RECT kr;
    GetWindowRect(c, &kr);
    printf("  child at %ld,%ld of the client\n", kr.left - o.x, kr.top - o.y);
    DestroyWindow(d);
}

/* タイムリープ's order: its game window, top-level and shown, made WS_CHILD and put in the dialog */
static void reparent(void) {
    HWND g = CreateWindowExA(0, "STATIC", "", WS_POPUP | WS_VISIBLE, 0, 0, 640, 480, NULL, NULL, NULL, NULL);
    HWND d = CreateDialogParamA(GetModuleHandleA(NULL), MAKEINTRESOURCEA(10), NULL, dproc, 0);
    MoveWindow(d, 0, -GetSystemMetrics(SM_CYMENU), 640, 480 + GetSystemMetrics(SM_CYMENU), TRUE);
    HWND old = SetParent(g, d);
    SetWindowLongA(g, GWL_STYLE, WS_CHILD | WS_VISIBLE);
    ShowWindow(d, SW_SHOW);
    RECT r, c;
    GetWindowRect(g, &r);
    GetClientRect(d, &c);
    POINT o = { 0, 0 };
    ClientToScreen(d, &o);
    printf("reparented: old parent desktop %d parent dialog %d visible %d at %ld,%ld of the client %ldx%ld, top-level %d\n",
           old == GetDesktopWindow(), GetParent(g) == d, IsWindowVisible(g), r.left - o.x, r.top - o.y, r.right - r.left, r.bottom - r.top,
           GetAncestor(g, GA_ROOT) == g);
    DestroyWindow(d);
}

int main(void) {
    for (int id = 10; id <= 13; id++) show(id);
    reparent();
    return 0;
}
