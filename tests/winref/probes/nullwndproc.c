/* A window whose procedure is set to NULL (SetWindowLongPtr GWLP_WNDPROC 0): what SetWindowLongPtr
 * and GetWindowLongPtr say, and that messages to it (sent, posted and dispatched, and DestroyWindow's
 * WM_DESTROY / WM_NCDESTROY) are answered without calling address 0. The Flash projector
 * (オヤジ狩り少女 体験版) does this before DestroyWindow. */
#include "../probe.h"

static int calls;
static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    calls++;
    return DefWindowProcW(h, m, w, l);
}

int main(void) {
    WNDCLASSW wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"nullwndproc";
    printf("register %d\n", RegisterClassW(&wc) != 0);
    HWND h = CreateWindowW(L"nullwndproc", L"x", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, wc.hInstance, NULL);
    printf("window %d calls>0 %d\n", h != NULL, calls > 0);
    SetLastError(0xdead);
    LONG_PTR old = SetWindowLongPtrW(h, GWLP_WNDPROC, 0);
    printf("set: old is proc %d err %lu\n", old != 0, GetLastError());
    SetLastError(0xdead);
    LONG_PTR now = GetWindowLongPtrW(h, GWLP_WNDPROC);
    printf("get: zero %d err %lu\n", now == 0, GetLastError());
    calls = 0;
    printf("send WM_USER: %ld\n", (long)SendMessageW(h, WM_USER, 1, 2));
    printf("send WM_GETTEXTLENGTH: %ld\n", (long)SendMessageW(h, WM_GETTEXTLENGTH, 0, 0));
    PostMessageW(h, WM_USER + 1, 0, 0);
    MSG m;
    int got = 0;
    while (PeekMessageW(&m, h, WM_USER + 1, WM_USER + 1, PM_REMOVE)) {
        got++;
        printf("dispatch: %ld\n", (long)DispatchMessageW(&m));
    }
    printf("posted %d\n", got);
    SetLastError(0xdead);
    BOOL ok = DestroyWindow(h);
    DWORD err = GetLastError();
    printf("destroy %d err %lu is window %d old proc calls %d\n", ok, err, IsWindow(h), calls);
    return 0;
}
