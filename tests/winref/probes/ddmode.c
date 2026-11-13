/* IDirectDraw::SetDisplayMode switches the screen: GetSystemMetrics answers the mode, and the
 * screen before the first of several sets comes back with RestoreDisplayMode or when the object
 * that set it is released. Melty Blood Re-ACT (mbp.exe) sizes its exclusive window to the
 * desktop, then sets 640x480 and flips 640x480 into it; sake kept the desktop's size and the game
 * sat in the top-left corner. Only comparisons are printed (the runner's desktop size varies);
 * a machine whose display refuses 640x480 or 800x600 prints skip. In exclusive (fullscreen) mode
 * the cooperative window is sized to the mode (Wine's ddraw tests: test_coop_level_mode_set).
 * libs: -lddraw -ldxguid
 * arch: 32 (DirectDraw games are 32-bit; sake has no x64 DirectDraw) */
#include "../probe.h"
#include <ddraw.h>

static int sw, sh;
static const char *screen(int w, int h) {
    int x = GetSystemMetrics(SM_CXSCREEN), y = GetSystemMetrics(SM_CYSCREEN);
    if (x == w && y == h) return "the mode";
    if (x == sw && y == sh) return "the desktop";
    return "other";
}

int main(void) {
    sw = GetSystemMetrics(SM_CXSCREEN);
    sh = GetSystemMetrics(SM_CYSCREEN);
    IDirectDraw *dd = NULL;
    HRESULT hr = DirectDrawCreate(NULL, &dd, NULL);
    if (FAILED(hr)) { printf("skip DirectDrawCreate %#lx\n", hr); return 0; }
    IDirectDraw_SetCooperativeLevel(dd, NULL, DDSCL_NORMAL);
    hr = IDirectDraw_SetDisplayMode(dd, 640, 480, 32);
    if (FAILED(hr)) { printf("skip SetDisplayMode 640x480 %#lx\n", hr); IDirectDraw_Release(dd); return 0; }
    printf("after 640x480: %s\n", screen(640, 480));
    DDSURFACEDESC d = { sizeof d };
    hr = IDirectDraw_GetDisplayMode(dd, &d);
    printf("GetDisplayMode %#lx %lux%lu\n", hr, d.dwWidth, d.dwHeight);
    hr = IDirectDraw_SetDisplayMode(dd, 800, 600, 32);
    if (FAILED(hr)) { printf("skip SetDisplayMode 800x600 %#lx\n", hr); IDirectDraw_RestoreDisplayMode(dd); IDirectDraw_Release(dd); return 0; }
    printf("after 800x600: %s\n", screen(800, 600));
    printf("RestoreDisplayMode %#lx\n", IDirectDraw_RestoreDisplayMode(dd));
    printf("after restore: %s\n", screen(800, 600));
    hr = IDirectDraw_SetDisplayMode(dd, 640, 480, 32);
    printf("again 640x480 %#lx: %s\n", hr, screen(640, 480));
    printf("Release %lu\n", IDirectDraw_Release(dd));
    printf("after release: %s\n", screen(640, 480));

    /* exclusive: a popup window as big as the desktop, as mbp.exe makes it */
    HWND w = CreateWindowA("static", "ddmode", WS_POPUP | WS_VISIBLE, 0, 0, sw, sh, NULL, NULL, NULL, NULL);
    if (FAILED(DirectDrawCreate(NULL, &dd, NULL))) { DestroyWindow(w); return 0; }
    printf("exclusive %#lx\n", IDirectDraw_SetCooperativeLevel(dd, w, DDSCL_EXCLUSIVE | DDSCL_FULLSCREEN));
    hr = IDirectDraw_SetDisplayMode(dd, 640, 480, 16);
    RECT r;
    GetWindowRect(w, &r);
    printf("exclusive 640x480 %#lx: %s, window %ldx%ld at %ld,%ld\n", hr, screen(640, 480),
           r.right - r.left, r.bottom - r.top, r.left, r.top);
    IDirectDraw_RestoreDisplayMode(dd);
    printf("exclusive restore: %s\n", screen(640, 480));
    IDirectDraw_SetCooperativeLevel(dd, w, DDSCL_NORMAL);
    IDirectDraw_Release(dd);
    DestroyWindow(w);
    return 0;
}
