/* What 自動送り (web/src/autoadvance.ts) gives a game: a window over the whole screen that logs
 * its clicks and keys, as tests/autoadvance-browser.mjs reads them from the page's console:
 *   "auto down X,Y" / "auto up X,Y"                        WM_LBUTTONDOWN / UP
 *   "auto key VK rep R ext E ctrl C lctrl L async A"       WM_KEYDOWN (R: lParam bit 30, E: bit 24;
 *                                                          C, L: GetKeyState(VK_CONTROL / VK_LCONTROL) down;
 *                                                          A: GetAsyncKeyState(VK_CONTROL) down)
 *   "auto keyup VK ctrl C"                                 WM_KEYUP
 * and every 500 ms "auto poll ctrl C" (GetAsyncKeyState(VK_CONTROL), a game polling it from its
 * frame loop). Quits after 300 s.
 * Build: i686-w64-mingw32-gcc -O2 autoadvance.c -o autoadvance.exe -mwindows */
#include <windows.h>
#include <stdio.h>

static int down(int vk) { return (GetKeyState(vk) & 0x8000) != 0; }

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    switch (msg)
    {
    case WM_LBUTTONDOWN:
    case WM_LBUTTONUP:
        printf("auto %s %d,%d\n", msg == WM_LBUTTONDOWN ? "down" : "up", (short)LOWORD(lp), (short)HIWORD(lp));
        fflush(stdout);
        return 0;
    case WM_KEYDOWN:
        printf("auto key %u rep %u ext %u ctrl %d lctrl %d async %d\n", (unsigned)wp, (unsigned)(lp >> 30) & 1,
               (unsigned)(lp >> 24) & 1, down(VK_CONTROL), down(VK_LCONTROL), (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0);
        fflush(stdout);
        return 0;
    case WM_KEYUP:
        printf("auto keyup %u ctrl %d\n", (unsigned)wp, down(VK_CONTROL));
        fflush(stdout);
        return 0;
    case WM_TIMER:
        if (wp == 2)
        {
            printf("auto poll ctrl %d\n", (GetAsyncKeyState(VK_CONTROL) & 0x8000) != 0);
            fflush(stdout);
            return 0;
        }
        DestroyWindow(hwnd);
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    WNDCLASSA wc = { 0, wndproc, 0, 0, inst, 0, LoadCursor(0, IDC_ARROW), (HBRUSH)(COLOR_WINDOW + 1), 0, "autoadvance" };
    HWND hwnd;
    MSG msg;
    RegisterClassA(&wc);
    hwnd = CreateWindowA("autoadvance", "autoadvance", WS_POPUP | WS_VISIBLE, 0, 0, GetSystemMetrics(SM_CXSCREEN),
                         GetSystemMetrics(SM_CYSCREEN), 0, 0, inst, 0);
    SetFocus(hwnd);
    printf("auto ready\n");
    fflush(stdout);
    SetTimer(hwnd, 1, 300000, NULL);
    SetTimer(hwnd, 2, 500, NULL);
    while (GetMessageA(&msg, 0, 0, 0))
    {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    return 0;
}
