/* libs: -luser32 */
/* SendNotifyMessage as Inno Setup's uninstaller tells its first phase to go: to a window of the
 * calling thread it is handled before the call returns; to another thread's window the call
 * returns at once, even while that thread is not looking at its messages, and the message is
 * handled as a sent one, before the messages posted earlier; a window that is not there is
 * ERROR_INVALID_WINDOW_HANDLE. (sake: user.rs send_notify; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>

static HWND other;
static HANDLE ready, go;
static char order[64];
static volatile int mine;

static LRESULT CALLBACK proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg >= WM_APP && msg < WM_APP + 10) {
        if (GetCurrentThreadId() == (DWORD)GetWindowLongPtrA(h, GWLP_USERDATA))
            mine = msg - WM_APP;
        size_t n = strlen(order);
        order[n] = '0' + (char)(msg - WM_APP);
        order[n + 1] = 0;
        if (msg == WM_APP + 9)
            PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(h, msg, wp, lp);
}

static HWND window(void) {
    HWND h = CreateWindowExA(0, "SakeNotify", "", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, GetModuleHandleA(NULL), NULL);
    SetWindowLongPtrA(h, GWLP_USERDATA, GetCurrentThreadId());
    return h;
}

static DWORD WINAPI thread(void *arg) {
    MSG m;
    other = window();
    PeekMessageA(&m, NULL, 0, 0, PM_NOREMOVE);
    SetEvent(ready);
    /* not looking at messages until told */
    WaitForSingleObject(go, INFINITE);
    while (GetMessageA(&m, NULL, 0, 0) > 0)
        DispatchMessageA(&m);
    return 0;
}

int main(void) {
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "SakeNotify";
    RegisterClassA(&wc);
    HWND me = window();
    BOOL r = SendNotifyMessageA(me, WM_APP + 5, 0, 0);
    printf("own window: %d, handled before returning: %d\n", r, mine == 5);
    r = SendNotifyMessageW(me, WM_APP + 6, 0, 0);
    printf("own window (W): %d, handled: %d\n", r, mine == 6);

    ready = CreateEventA(NULL, FALSE, FALSE, NULL);
    go = CreateEventA(NULL, FALSE, FALSE, NULL);
    HANDLE t = CreateThread(NULL, 0, thread, NULL, 0, NULL);
    WaitForSingleObject(ready, INFINITE);
    order[0] = 0;
    PostMessageA(other, WM_APP + 1, 0, 0);
    DWORD t0 = GetTickCount();
    r = SendNotifyMessageA(other, WM_APP + 2, 0, 0);
    printf("other thread's window: %d, returned at once: %d, handled yet: %d\n", r, GetTickCount() - t0 < 2000, order[0] != 0);
    PostMessageA(other, WM_APP + 9, 0, 0);
    SetEvent(go);
    WaitForSingleObject(t, 10000);
    printf("order: %s (the sent one first)\n", order);

    SetLastError(0);
    r = SendNotifyMessageA((HWND)(ULONG_PTR)0x7654, WM_APP, 0, 0);
    printf("no such window: %d, error %lu\n", r, GetLastError());
    return 0;
}
