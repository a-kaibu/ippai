/* libs: -luser32 */
#include <windows.h>
#include <stdio.h>
static void drain(void) { MSG m; while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) { TranslateMessage(&m); DispatchMessageA(&m); } }
int main(void) {
    MSG m;
    HANDLE event = CreateEventA(NULL, FALSE, FALSE, NULL);
    drain();
    DWORD began = GetTickCount();
    DWORD r = WAIT_TIMEOUT;
    for (int i = 0; i < 25; i++) {
        DWORD next = MsgWaitForMultipleObjects(0, NULL, FALSE, 1, QS_ALLINPUT);
        if (next != WAIT_TIMEOUT) r = next;
    }
    printf("empty queue: timeout %d, short waits consume time %d\n", r == WAIT_TIMEOUT, GetTickCount() - began >= 10);
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "SakeComposedStartup";
    RegisterClassA(&wc);
    HWND window = CreateWindowExA(0, wc.lpszClassName, "startup", WS_OVERLAPPEDWINDOW, 20, 20, 320, 240, NULL, NULL, wc.hInstance, NULL);
    printf("created hidden caption: %d\n", window != NULL && !IsWindowVisible(window));
    r = MsgWaitForMultipleObjects(1, &event, TRUE, 0, QS_ALLINPUT);
    printf("notification alone: timeout %d\n", r == WAIT_TIMEOUT);
    SetEvent(event);
    r = MsgWaitForMultipleObjects(1, &event, TRUE, 50, QS_ALLINPUT);
    printf("notification and event: completed %d, consumed event %d\n", r == WAIT_OBJECT_0, WaitForSingleObject(event, 0) == WAIT_TIMEOUT);
    BOOL found = PeekMessageA(&m, window, 0x31f, 0x31f, PM_REMOVE);
    printf("composition notification: %d, enabled %d, reserved zero %d\n", found != 0, found && m.wParam == TRUE, found && m.lParam == 0);
    printf("one notification: %d\n", !PeekMessageA(&m, window, 0x31f, 0x31f, PM_REMOVE));
    DestroyWindow(window);
    drain();
    CloseHandle(event);
    return 0;
}
