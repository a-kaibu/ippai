/* Blits between DCs of one top-level window: two GetDC handles of the same window, a child's
 * DC from its parent's, and AlphaBlend / TransparentBlt / StretchBlt of a window DC onto
 * itself. Every call returns (sake locked the shared window surface twice and hung the thread
 * with the GDI lock held: every other thread then stopped at its next GDI call).
 * libs: -lgdi32 -lmsimg32 */
#include "../probe.h"

int main(void) {
    HWND p = CreateWindowExA(0, "STATIC", "parent", WS_POPUP | WS_VISIBLE, 0, 0, 96, 64, NULL, NULL, NULL, NULL);
    HWND c = CreateWindowExA(0, "STATIC", "", WS_CHILD | WS_VISIBLE, 48, 32, 32, 24, p, NULL, NULL, NULL);
    HDC a = GetDC(p), b = GetDC(p), k = GetDC(c);
    printf("two DCs %d\n", a != b);
    HBRUSH red = CreateSolidBrush(RGB(255, 0, 0));
    RECT r = { 0, 0, 16, 16 };
    printf("FillRect %d\n", FillRect(a, &r, red) != 0);
    printf("BitBlt other DC %d\n", BitBlt(b, 20, 0, 16, 16, a, 0, 0, SRCCOPY) != 0);
    printf("BitBlt child from parent %d\n", BitBlt(k, 0, 0, 16, 16, a, 0, 0, SRCCOPY) != 0);
    printf("BitBlt parent from child %d\n", BitBlt(a, 0, 40, 16, 16, k, 0, 0, SRCCOPY) != 0);
    printf("StretchBlt other DC %d\n", StretchBlt(b, 40, 0, 8, 8, a, 0, 0, 16, 16, SRCCOPY) != 0);
    BLENDFUNCTION bf = { AC_SRC_OVER, 0, 128, 0 };
    printf("AlphaBlend same DC %d\n", AlphaBlend(a, 60, 0, 16, 16, a, 0, 0, 16, 16, bf) != 0);
    printf("AlphaBlend other DC %d\n", AlphaBlend(b, 60, 20, 16, 16, a, 0, 0, 16, 16, bf) != 0);
    printf("TransparentBlt same DC %d\n", TransparentBlt(a, 0, 20, 16, 16, a, 0, 0, 16, 16, RGB(0, 0, 0)) != 0);
    printf("TransparentBlt child %d\n", TransparentBlt(k, 0, 0, 8, 8, a, 0, 0, 16, 16, RGB(0, 0, 0)) != 0);
    DeleteObject(red);
    ReleaseDC(c, k);
    ReleaseDC(p, b);
    ReleaseDC(p, a);
    DestroyWindow(p);
    puts("done");
    return 0;
}
