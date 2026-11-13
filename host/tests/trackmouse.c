/* TrackMouseEvent (and comctl32's _TrackMouseEvent): KiriKiri 2 and Delphi's VCL ask for
 * TME_LEAVE on every mouse move over their windows; sake had no TrackMouseEvent. A window the
 * cursor is not over gets its WM_MOUSELEAVE at once (posted, nothing stays tracked); a bad
 * cbSize is ERROR_INVALID_PARAMETER, a bad window ERROR_INVALID_WINDOW_HANDLE; TME_QUERY with
 * nothing tracked answers no flags.
 * libs: -luser32 -lcomctl32 */
#include <stdio.h>
#include <windows.h>
#include <commctrl.h>

static int leaves;
static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_MOUSELEAVE) leaves++;
    return DefWindowProcA(h, m, w, l);
}

static int pump(HWND h)
{
    MSG msg;
    int n = 0;
    while (PeekMessageA(&msg, h, WM_MOUSELEAVE, WM_MOUSELEAVE, PM_REMOVE)) {
        n++;
        DispatchMessageA(&msg);
    }
    return n;
}

int main(void)
{
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.lpszClassName = "trackmouse";
    RegisterClassA(&wc);
    /* hidden and far off the screen: never under the cursor */
    HWND h = CreateWindowA("trackmouse", "t", WS_OVERLAPPEDWINDOW, -5000, -5000, 200, 100, NULL, NULL, NULL, NULL);
    TRACKMOUSEEVENT t = {sizeof t, TME_LEAVE, h, HOVER_DEFAULT};

    /* (one call per statement: printf's arguments are evaluated in no set order) */
    BOOL r;
    int n;
    t.cbSize = 0;
    SetLastError(0);
    r = TrackMouseEvent(&t);
    printf("cbSize 0: %d, error %lu\n", r, GetLastError());
    t.cbSize = sizeof t;
    t.hwndTrack = (HWND)0x1234;
    SetLastError(0);
    r = TrackMouseEvent(&t);
    printf("bad window: %d, error %lu\n", r, GetLastError());
    t.hwndTrack = h;

    r = TrackMouseEvent(&t);
    printf("TME_LEAVE away from the window: %d\n", r);
    n = pump(h);
    printf("WM_MOUSELEAVE posted: %d, wndproc saw %d\n", n, leaves);
    TRACKMOUSEEVENT q = {sizeof q, TME_QUERY, 0, 0};
    r = TrackMouseEvent(&q);
    printf("TME_QUERY: %d, flags %lx\n", r, q.dwFlags);
    printf("nothing more: %d\n", pump(h));

    t.dwFlags = TME_LEAVE;
    r = _TrackMouseEvent(&t);
    n = pump(h);
    printf("_TrackMouseEvent: %d, posted %d\n", r, n);
    t.dwFlags = TME_LEAVE | TME_CANCEL;
    r = TrackMouseEvent(&t);
    n = pump(h);
    printf("cancel: %d, posted %d\n", r, n);
    DestroyWindow(h);
    return 0;
}
