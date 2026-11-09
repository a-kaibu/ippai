/* A window asked bigger than sake's screen (KiriKiri Z's 1920x1080 client, 俺の姿が､透明に!)
 * grows the screen instead of being kept to it (user.rs grown_screen): the window gets its whole
 * client area, and the screen's metrics, work area and monitor say the new size. Not Windows'
 * answer (a monitor has a size; Windows keeps the window to it: winref probe user's "big",
 * which runs with IPPAI_SCREEN fixed): sake's screen is no monitor.
 * Build: i686-w64-mingw32-gcc -O2 bigwindow.c -o bigwindow.exe */
#include <stdio.h>
#include <windows.h>

static void show(const char *label, HWND h)
{
    RECT c, w, work;
    GetClientRect(h, &c);
    GetWindowRect(h, &w);
    SystemParametersInfoA(SPI_GETWORKAREA, 0, &work, 0);
    MONITORINFO mi = {sizeof(mi)};
    GetMonitorInfoA(MonitorFromWindow(h, MONITOR_DEFAULTTOPRIMARY), &mi);
    printf("%s: client %ldx%ld, window inside the screen %d, screen %dx%d, work %ldx%ld, monitor %ldx%ld\n", label, c.right, c.bottom,
           w.left >= 0 && w.top >= 0 && w.right <= GetSystemMetrics(SM_CXSCREEN) && w.bottom <= GetSystemMetrics(SM_CYSCREEN),
           GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), work.right - work.left, work.bottom - work.top,
           mi.rcMonitor.right - mi.rcMonitor.left, mi.rcMonitor.bottom - mi.rcMonitor.top);
}

int main(void)
{
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "ippai-bigwindow";
    RegisterClassA(&wc);
    printf("screen at start: %dx%d\n", GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    DWORD style = WS_OVERLAPPEDWINDOW & ~(WS_THICKFRAME | WS_MAXIMIZEBOX);
    RECT r = {0, 0, 1920, 1080};
    AdjustWindowRect(&r, style, FALSE);
    HWND h = CreateWindowExA(0, "ippai-bigwindow", "big", style, CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top, NULL, NULL, wc.hInstance, NULL);
    show("1920x1080 created", h);
    ShowWindow(h, SW_SHOW);
    /* (KiriKiri sets its inner size again once shown) */
    RECT r2 = {0, 0, 2560, 1440};
    AdjustWindowRect(&r2, style, FALSE);
    SetWindowPos(h, NULL, 0, 0, r2.right - r2.left, r2.bottom - r2.top, SWP_NOMOVE | SWP_NOZORDER);
    show("2560x1440 set", h);
    /* put back where a bigger desktop had it (saved settings from a real PC): moved alone */
    SetWindowPos(h, NULL, 391, 239, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
    show("moved to 391,239", h);
    /* smaller again: the screen stays */
    SetWindowPos(h, NULL, 0, 0, 800, 600, SWP_NOMOVE | SWP_NOZORDER);
    show("800x600 set", h);
    /* a popup is never kept to the screen and never grows it */
    HWND p = CreateWindowExA(0, "ippai-bigwindow", "popup", WS_POPUP, 0, 0, 5000, 5000, NULL, NULL, wc.hInstance, NULL);
    show("5000x5000 popup", p);
    DestroyWindow(p);
    DestroyWindow(h);
    return 0;
}
