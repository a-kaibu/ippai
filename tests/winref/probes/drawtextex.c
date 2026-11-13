/* libs: -lgdi32 -luser32 */
/* DrawTextExA measures and draws as DrawTextA and DrawTextExW do (EXEpress CX's installer wizard
 * writes its pages with DrawTextExA): DT_CALCRECT rectangles and returned heights of the same
 * text, one line and word-wrapped, with and without a DRAWTEXTPARAMS. Equalities only, so the
 * answers do not depend on the font. */
#include "../probe.h"

int main(void) {
    HDC dc = CreateCompatibleDC(NULL);
    SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
    static const char *s[] = { "Install to C:\\Program Files", "a long line of words that wraps in a narrow box" };
    static const WCHAR *w[] = { L"Install to C:\\Program Files", L"a long line of words that wraps in a narrow box" };
    for (int i = 0; i < 2; i++) {
        UINT f = DT_CALCRECT | (i ? DT_WORDBREAK : DT_SINGLELINE);
        RECT a = { 0, 0, 80, 0 }, b = a, c = a, d = a;
        int ha = DrawTextA(dc, s[i], -1, &a, f);
        int hb = DrawTextExA(dc, (char *)s[i], -1, &b, f, NULL);
        int hc = DrawTextExW(dc, (WCHAR *)w[i], -1, &c, f, NULL);
        DRAWTEXTPARAMS p = { sizeof p, 0, 0, 0, 0 };
        int hd = DrawTextExA(dc, (char *)s[i], -1, &d, f, &p);
        printf("text %d: height>0 %d, ExA = A %d, ExW = A %d, with params = A %d, rects same %d %d %d\n", i, ha > 0,
               hb == ha, hc == ha, hd == ha, EqualRect(&a, &b), EqualRect(&a, &c), EqualRect(&a, &d));
        RECT r = { 0, 0, 200, 50 };
        printf("text %d: draw ExA = draw A %d\n", i, DrawTextExA(dc, (char *)s[i], -1, &r, i ? DT_WORDBREAK : 0, NULL) == DrawTextA(dc, s[i], -1, &r, i ? DT_WORDBREAK : 0));
    }
    DeleteDC(dc);
    return 0;
}
