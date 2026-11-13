/* libs: -lgdi32 */
/* GetDeviceCaps of a memory DC: a fresh one (its 1x1 monochrome stock bitmap selected), then
 * with a monochrome bitmap, an 8-bit and a 32-bit DIB section, and with the stock bitmap back.
 * Flash's player (裏雀荘ニ咲ク花 体験版) asks a fresh CreateCompatibleDC(0) for PLANES and
 * BITSPIXEL and draws 1-bit dithered frames when told 1. (sake: gdi.rs GetDeviceCaps) */
#include <windows.h>
#include <stdio.h>

static void caps(HDC dc, const char *what) {
    printf("%s: planes %d bitspixel %d numcolors %d rc_palette %d\n", what, GetDeviceCaps(dc, PLANES), GetDeviceCaps(dc, BITSPIXEL),
           GetDeviceCaps(dc, NUMCOLORS), (GetDeviceCaps(dc, RASTERCAPS) & RC_PALETTE) != 0);
}

static HBITMAP dib(int bpp) {
    struct { BITMAPINFOHEADER h; RGBQUAD pal[256]; } bi = { { sizeof(BITMAPINFOHEADER), 8, 8, 1, (WORD)bpp, BI_RGB } };
    void *bits;
    return CreateDIBSection(NULL, (BITMAPINFO *)&bi, DIB_RGB_COLORS, &bits, NULL, 0);
}

int main(void) {
    HDC screen = GetDC(NULL);
    caps(screen, "screen");
    HDC dc = CreateCompatibleDC(NULL);
    caps(dc, "fresh memory dc");
    HDC dc2 = CreateCompatibleDC(screen);
    caps(dc2, "memory dc of the screen");
    HBITMAP mono = CreateBitmap(8, 8, 1, 1, NULL);
    HGDIOBJ stock = SelectObject(dc, mono);
    caps(dc, "monochrome bitmap");
    HBITMAP d8 = dib(8), d32 = dib(32);
    SelectObject(dc, d8);
    caps(dc, "8-bit dib");
    SelectObject(dc, d32);
    caps(dc, "32-bit dib");
    SelectObject(dc, stock);
    caps(dc, "stock bitmap back");
    HBITMAP compat = CreateCompatibleBitmap(dc, 4, 4);
    BITMAP b;
    GetObjectA(compat, sizeof b, &b);
    printf("compatible bitmap of a fresh dc: %d bpp\n", b.bmBitsPixel);
    DeleteObject(compat);
    DeleteDC(dc);
    DeleteDC(dc2);
    DeleteObject(mono);
    DeleteObject(d8);
    DeleteObject(d32);
    ReleaseDC(NULL, screen);
    return 0;
}
