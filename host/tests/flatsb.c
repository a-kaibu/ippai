/* libs: -lcomctl32 */
/* comctl32's flat scroll bars: InitializeFlatSB and the FlatSB_ calls on a window's bars, before
 * and after initializing (Delphi launchers call them for every list; they were unimplemented
 * and returned 0). Only the FlatSB_ calls are used on a bar, never mixed with user32's.
 * (sake: user.rs FlatSB_) */
#include <windows.h>
#include <commctrl.h>
#include <stdio.h>

static void bar(HWND w, int b, const char *what) {
    SCROLLINFO si = { sizeof si, SIF_RANGE | SIF_PAGE | SIF_POS };
    si.nMin = 0; si.nMax = 99; si.nPage = 10; si.nPos = 20;
    int r = FlatSB_SetScrollInfo(w, b, &si, FALSE);
    SCROLLINFO g = { sizeof g, SIF_ALL };
    BOOL ok = FlatSB_GetScrollInfo(w, b, &g);
    printf("%s bar %d: set %d get %d range %d..%d page %u pos %d\n", what, b, r, ok, g.nMin, g.nMax, g.nPage, g.nPos);
    int old = FlatSB_SetScrollPos(w, b, 200, FALSE);
    printf("  setpos old %d now %d (clamped to max-page+1)\n", old, FlatSB_GetScrollPos(w, b));
    old = FlatSB_SetScrollPos(w, b, -5, FALSE);
    printf("  setpos old %d now %d\n", old, FlatSB_GetScrollPos(w, b));
    ok = FlatSB_SetScrollRange(w, b, 5, 50, FALSE);
    int lo = -1, hi = -1;
    BOOL ok2 = FlatSB_GetScrollRange(w, b, &lo, &hi);
    printf("  range %d %d: %d..%d pos %d\n", ok, ok2, lo, hi, FlatSB_GetScrollPos(w, b));
    printf("  show %d enable %d\n", FlatSB_ShowScrollBar(w, b, TRUE) != 0, FlatSB_EnableScrollBar(w, b, ESB_ENABLE_BOTH) != 0);
}

int main(void) {
    HWND w = CreateWindowExA(0, "STATIC", "flatsb", WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_HSCROLL, 0, 0, 300, 200, NULL, NULL, NULL, NULL);
    printf("window %d\n", w != NULL);
    bar(w, SB_VERT, "plain");
    BOOL init = InitializeFlatSB(w);
    printf("init %d\n", init != 0);
    bar(w, SB_HORZ, "flat");
    bar(w, SB_VERT, "flat");
    printf("setprop style %d\n", FlatSB_SetScrollProp(w, WSB_PROP_VSTYLE, FSB_REGULAR_MODE, TRUE) != 0);
    printf("uninit %lx\n", (unsigned long)UninitializeFlatSB(w));
    DestroyWindow(w);
    return 0;
}
