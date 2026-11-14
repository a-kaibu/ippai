/* WS_EX_CLIENTEDGE's sunken edge round a child's client area, as WM_PRINT(PRF_NONCLIENT) draws
 * a window's non-client area into a DC: 40x30 and 6x5 children with the edge and one without;
 * what is not the edge stays as the DIB was. Their window and client sizes too. The old KiriKiri 2's drawing child and
 * アトリエさくら's have this edge; sake drew none, a 2-pixel black frame (#215).
 * libs: -lgdi32 */
#include "../probe.h"

static unsigned *bits;
static const int W = 48, H = 36;

static void show(HWND parent, const char *name, DWORD ex, int w, int h) {
    HWND c = CreateWindowExA(ex, "STATIC", "", WS_CHILD | WS_VISIBLE, 4, 4, w, h, parent, NULL, NULL, NULL);
    RECT wr, cr;
    GetWindowRect(c, &wr);
    GetClientRect(c, &cr);
    printf("%s: window %ldx%ld client %ldx%ld\n", name, wr.right - wr.left, wr.bottom - wr.top, cr.right, cr.bottom);
    BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), W, -H, 1, 32, BI_RGB } };
    HDC dc = CreateCompatibleDC(NULL);
    HBITMAP bm = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)&bits, NULL, 0);
    HGDIOBJ old = SelectObject(dc, bm);
    for (int i = 0; i < W * H; i++) bits[i] = 0x123456;
    LRESULT r = DefWindowProcA(c, WM_PRINT, (WPARAM)dc, PRF_NONCLIENT);
    GdiFlush();
    printf("%s: WM_PRINT %ld\n", name, (long)r);
    probe_image(name, bits, W, H, W * 4, 0);
    SelectObject(dc, old);
    DeleteObject(bm);
    DeleteDC(dc);
    DestroyWindow(c);
}

int main(void) {
    HWND p = CreateWindowExA(0, "STATIC", "parent", WS_POPUP, 0, 0, 200, 200, NULL, NULL, NULL, NULL);
    show(p, "clientedge", WS_EX_CLIENTEDGE, 40, 30);
    show(p, "clientedge_small", WS_EX_CLIENTEDGE, 6, 5);
    show(p, "none", 0, 40, 30);
    DestroyWindow(p);
    return 0;
}
