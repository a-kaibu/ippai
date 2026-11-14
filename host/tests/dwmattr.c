/* DwmGetWindowAttribute agrees with DwmIsCompositionEnabled: with composition on (Windows 8 and
 * later always) DWMWA_EXTENDED_FRAME_BOUNDS succeeds with a rectangle around the window's
 * client area; with it off (sake, as Windows 7 with the basic theme) every attribute fails with
 * DWM_E_COMPOSITIONDISABLED and the buffer is left alone. Unity 6 took a success that wrote
 * nothing and placed its window's client area at x -1037 (逆討伐 体験版). (sake: misc.rs)
 * libs: -ldwmapi */
#include <windows.h>
#include <dwmapi.h>
#include <stdio.h>

int main(void)
{
    HWND w = CreateWindowExA(0, "STATIC", "dwm", WS_OVERLAPPEDWINDOW, 100, 100, 400, 300, NULL, NULL, NULL, NULL);
    BOOL on = FALSE;
    HRESULT hc = DwmIsCompositionEnabled(&on);
    RECT r = { 0x1111, 0x2222, 0x3333, 0x4444 }, wr;
    GetWindowRect(w, &wr);
    HRESULT h = DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, &r, sizeof r);
    int consistent;
    if (on)
        consistent = h == S_OK && r.left >= wr.left - 1 && r.right <= wr.right + 1 && r.right - r.left > 300 && r.bottom - r.top > 200;
    else
        consistent = h == (HRESULT)0x80263001 && r.left == 0x1111 && r.top == 0x2222 && r.right == 0x3333 && r.bottom == 0x4444;
    printf("DwmIsCompositionEnabled %08lx, extended frame bounds agree %d\n", (unsigned long)hc, consistent);
    BOOL cloaked = 7;
    h = DwmGetWindowAttribute(w, DWMWA_CLOAKED, &cloaked, sizeof cloaked);
    printf("cloaked agrees %d\n", on ? h == S_OK && cloaked == 0 : h == (HRESULT)0x80263001 && cloaked == 7);
    DestroyWindow(w);
    return 0;
}
