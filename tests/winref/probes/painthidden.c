/* WM_PAINT and windows that are not on screen: a top-level window created without WS_VISIBLE
 * and a visible child inside it, invalidated, then UpdateWindow, RedrawWindow(RDW_UPDATENOW)
 * and the queue (PeekMessage) — what they deliver before and after ShowWindow. Malie's
 * twitter.dll (初恋＊シンドローム 体験版) creates its tweet window hidden and calls UpdateWindow;
 * its WM_PAINT needs a picture that is loaded only when the window is shown.
 * libs: -lgdi32 */
#include "../probe.h"

static int paints[3];
static HWND top, child;

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_PAINT) {
        paints[h == top ? 0 : h == child ? 1 : 2]++;
        PAINTSTRUCT ps;
        BeginPaint(h, &ps);
        EndPaint(h, &ps);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

static void show(const char *what) {
    RECT r;
    BOOL ut = GetUpdateRect(top, &r, FALSE);
    BOOL uc = GetUpdateRect(child, &r, FALSE);
    printf("%s: paints top %d child %d; update top %d child %d; visible %d %d\n", what, paints[0], paints[1], ut, uc,
           IsWindowVisible(top), IsWindowVisible(child));
    paints[0] = paints[1] = paints[2] = 0;
}

static void pump(void) {
    MSG m;
    int n = 0;
    while (n < 50 && PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) {
        DispatchMessageA(&m);
        n++;
    }
}

int main(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "painthidden";
    wc.hbrBackground = (HBRUSH)GetStockObject(WHITE_BRUSH);
    RegisterClassA(&wc);
    top = CreateWindowExA(0, "painthidden", "t", WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                          10, 10, 200, 150, NULL, NULL, wc.hInstance, NULL);
    child = CreateWindowExA(0, "painthidden", "c", WS_CHILD | WS_VISIBLE, 5, 5, 50, 40, top, NULL, wc.hInstance, NULL);
    show("created hidden");
    UpdateWindow(top);
    UpdateWindow(child);
    show("UpdateWindow");
    InvalidateRect(top, NULL, TRUE);
    InvalidateRect(child, NULL, TRUE);
    UpdateWindow(top);
    UpdateWindow(child);
    show("invalidated, UpdateWindow");
    RedrawWindow(top, NULL, NULL, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
    show("RedrawWindow UPDATENOW");
    pump();
    show("PeekMessage");
    ShowWindow(top, SW_SHOWNOACTIVATE);
    show("ShowWindow");
    UpdateWindow(top);
    show("shown, UpdateWindow");
    pump();
    show("shown, PeekMessage");
    InvalidateRect(top, NULL, TRUE);
    UpdateWindow(top);
    show("shown, invalidated, UpdateWindow");
    ShowWindow(top, SW_HIDE);
    pump();
    InvalidateRect(top, NULL, TRUE);
    UpdateWindow(top);
    pump();
    show("hidden again, invalidated, UpdateWindow + PeekMessage");
    DestroyWindow(top);
    return 0;
}
