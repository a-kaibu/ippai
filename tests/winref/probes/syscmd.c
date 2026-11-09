/* WM_SYSCOMMAND's SC_MAXIMIZE and SC_RESTORE through DefWindowProc (posted, as KiriKiri's
 * windowEx plugin restores a saved maximized window: AMBITIOUS MISSION after episode 1), and
 * ShowWindow's SW_MAXIMIZE / SW_RESTORE / SW_SHOWNORMAL: IsZoomed, WS_MAXIMIZE in the style,
 * the WM_SIZE kinds sent, whether the client grew, and the rect after restoring. Overlapped,
 * popup and popup with a menu (KiriKiri's TVPMainWindow after its style change). Sizes are not
 * printed (Windows maximizes to the work area, sake to its screen). */
#include "../probe.h"

static int sizes, kinds[8];
static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_SIZE) {
        if (sizes < 8) kinds[sizes] = (int)w;
        sizes++;
    }
    return DefWindowProcA(h, m, w, l);
}

static void pump(void) {
    MSG m;
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&m);
}

static RECT before;
static void state(const char *what, HWND h) {
    RECT w, c;
    GetWindowRect(h, &w);
    GetClientRect(h, &c);
    printf("%s: zoomed %d style %s sizes %d kinds", what, IsZoomed(h) ? 1 : 0, GetWindowLongA(h, GWL_STYLE) & WS_MAXIMIZE ? "max" : "-", sizes);
    for (int i = 0; i < sizes && i < 8; i++) printf(" %d", kinds[i]);
    printf(" bigger %d back %d\n", c.right > 400 && c.bottom > 300,
           w.left == before.left && w.top == before.top && w.right == before.right && w.bottom == before.bottom);
    sizes = 0;
}

static void run(const char *name, DWORD style, HMENU menu) {
    HWND h = CreateWindowExA(0, "syscmd", name, style, 30, 40, 400, 300, NULL, menu, GetModuleHandleA(NULL), NULL);
    ShowWindow(h, SW_SHOW);
    pump();
    GetWindowRect(h, &before);
    sizes = 0;
    printf("[%s]\n", name);
    PostMessageA(h, WM_SYSCOMMAND, SC_MAXIMIZE, 0);
    pump();
    state("posted SC_MAXIMIZE", h);
    SendMessageA(h, WM_SYSCOMMAND, SC_MAXIMIZE, 0);
    state("SC_MAXIMIZE again", h);
    SendMessageA(h, WM_SYSCOMMAND, SC_RESTORE, 0);
    state("SC_RESTORE", h);
    SendMessageA(h, WM_SYSCOMMAND, SC_RESTORE, 0);
    state("SC_RESTORE again", h);
    ShowWindow(h, SW_MAXIMIZE);
    state("SW_MAXIMIZE", h);
    ShowWindow(h, SW_SHOWNORMAL);
    state("SW_SHOWNORMAL", h);
    ShowWindow(h, SW_SHOWMAXIMIZED);
    state("SW_SHOWMAXIMIZED", h);
    ShowWindow(h, SW_RESTORE);
    state("SW_RESTORE", h);
    DestroyWindow(h);
}

int main(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "syscmd";
    RegisterClassA(&wc);
    run("overlapped", WS_OVERLAPPEDWINDOW, NULL);
    run("popup", WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX, NULL);
    HMENU m = CreateMenu();
    AppendMenuA(m, MF_STRING, 1, "file");
    run("popup with a menu", WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN | WS_CLIPSIBLINGS, m);
    return 0;
}
