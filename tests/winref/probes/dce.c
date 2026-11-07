/* user32's window DCs (gdi.rs get_dce/release_dce) as games and their frameworks use them:
 * cache DCs lent by GetDC and reset by ReleaseDC (DCX_NORESETATTRS keeps the attributes, a
 * released DC answers nothing), CS_OWNDC and CS_CLASSDC DCs that keep their state, regions
 * GetDCEx takes over, BeginPaint's clip box in each kind, a CS_PARENTDC child drawing into its
 * parent, ScrollWindowEx's update rectangle, ReleaseDC's answers and destroyed windows.
 * Sizes are printed relative to the client rectangle (the frame depends on the theme).
 * libs: -lgdi32 */
#include "../probe.h"

#ifndef DCX_USESTYLE
#define DCX_USESTYLE 0x00010000
#endif

static int same(const RECT *a, const RECT *b) { return EqualRect(a, b); }

static void clipbox(const char *what, HDC dc, HWND h) {
    RECT r, c;
    SetRectEmpty(&r);
    int k = GetClipBox(dc, &r);
    GetClientRect(h, &c);
    if (same(&r, &c)) printf("%s: clip %d client\n", what, k);
    else printf("%s: clip %d (%ld,%ld)-(%ld,%ld)\n", what, k, r.left, r.top, r.right, r.bottom);
}

static HWND make(const char *cls, DWORD style, HWND parent, int x, int y, int w, int h) {
    return CreateWindowA(cls, NULL, style, x, y, w, h, parent, NULL, GetModuleHandleA(NULL), NULL);
}

int main(void) {
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = DefWindowProcA;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hbrBackground = GetStockObject(WHITE_BRUSH);
    const char *names[] = {"probe_cache", "probe_own", "probe_class", "probe_parent"};
    UINT styles[] = {0, CS_OWNDC, CS_CLASSDC, CS_PARENTDC};
    for (int i = 0; i < 4; i++) {
        wc.style = styles[i];
        wc.lpszClassName = names[i];
        RegisterClassA(&wc);
    }
    HWND cache = make("probe_cache", WS_OVERLAPPED | WS_VISIBLE, NULL, 0, 0, 120, 120);
    HWND own = make("probe_own", WS_OVERLAPPED | WS_VISIBLE, NULL, 0, 150, 120, 120);
    HWND cls1 = make("probe_class", WS_OVERLAPPED | WS_VISIBLE, NULL, 150, 0, 120, 120);
    HWND cls2 = make("probe_class", WS_OVERLAPPED | WS_VISIBLE, NULL, 150, 150, 120, 120);
    HWND parent = make("probe_cache", WS_OVERLAPPED | WS_VISIBLE, NULL, 300, 0, 120, 120);
    HWND pchild = make("probe_parent", WS_CHILD | WS_VISIBLE, parent, 40, 40, 2, 2);

    /* cache DCs */
    HDC dc = GetDC(cache);
    int def = GetROP2(dc);
    SetROP2(dc, R2_WHITE);
    printf("cache: rop %d window %d\n", GetROP2(dc), WindowFromDC(dc) == cache);
    int a1 = ReleaseDC(cache, dc), a2 = ReleaseDC(cache, dc), a3 = ReleaseDC(cache, NULL);
    printf("release %d again %d null %d\n", a1, a2, a3);
    a1 = GetROP2(dc);
    a2 = SetROP2(dc, R2_BLACK);
    a3 = GetDeviceCaps(dc, HORZRES);
    printf("released: rop %d set %d horzres %d window %d\n", a1, a2, a3, WindowFromDC(dc) == cache);
    HDC again = GetDC(cache);
    printf("again: reset %d same handle %d\n", GetROP2(again) == def, again == dc);
    ReleaseDC(cache, again);
    dc = GetDCEx(cache, 0, DCX_USESTYLE | DCX_NORESETATTRS);
    SetROP2(dc, R2_WHITE);
    ReleaseDC(cache, dc);
    HDC dcs[8];
    int kept = -1;
    for (int i = 0; i < 8; i++) {
        dcs[i] = GetDCEx(cache, 0, DCX_USESTYLE);
        if (dcs[i] == dc) kept = GetROP2(dcs[i]);
    }
    /* (a plain GetDCEx resets what the DCX_NORESETATTRS release kept; where the DC is in the
     * cache depends on the machine) */
    printf("noresetattrs: rop kept %d\n", kept == R2_WHITE);
    for (int i = 7; i >= 0; i--) ReleaseDC(cache, dcs[i]);
    HDC scr = GetDC(NULL);
    printf("screen: horzres>0 %d", GetDeviceCaps(scr, HORZRES) > 0);
    ReleaseDC(NULL, scr);
    printf(" released %d\n", GetDeviceCaps(scr, HORZRES));

    /* regions given to GetDCEx */
    RECT r;
    SetRect(&r, 10, 10, 20, 20);
    MapWindowPoints(cache, NULL, (POINT *)&r, 2);
    HRGN rgn = CreateRectRgnIndirect(&r);
    dc = GetDCEx(cache, rgn, DCX_INTERSECTRGN | DCX_USESTYLE);
    clipbox("cache intersectrgn", dc, cache);
    printf("region kept while used %d", GetRgnBox(rgn, &r) != ERROR);
    ReleaseDC(cache, dc);
    printf(", deleted on release %d\n", GetRgnBox(rgn, &r) == ERROR);

    /* own DCs */
    dc = GetDC(own);
    SetROP2(dc, R2_WHITE);
    a1 = ReleaseDC(own, dc);
    a2 = ReleaseDC(own, dc);
    printf("own: release %d %d window %d", a1, a2, WindowFromDC(dc) == own);
    a1 = GetDC(own) == dc;
    a2 = GetROP2(dc);
    printf(" same %d rop %d wrong window release %d\n", a1, a2, ReleaseDC(cache, dc));
    SetRect(&r, 10, 10, 20, 20);
    MapWindowPoints(own, NULL, (POINT *)&r, 2);
    rgn = CreateRectRgnIndirect(&r);
    dc = GetDCEx(own, rgn, DCX_INTERSECTRGN | DCX_USESTYLE);
    ReleaseDC(own, dc);
    clipbox("own intersectrgn after release", dc, own);
    dc = GetDCEx(own, 0, DCX_USESTYLE);
    clipbox("own GetDCEx again", dc, own);
    printf("own region kept %d", GetRgnBox(rgn, &r) != ERROR);
    dc = GetDCEx(own, 0, DCX_EXCLUDERGN | DCX_USESTYLE);
    printf(", deleted by the next %d\n", GetRgnBox(rgn, &r) == ERROR);
    clipbox("own after excludergn", dc, own);
    ReleaseDC(own, dc);

    /* class DCs */
    HDC c1 = GetDC(cls1);
    SetROP2(c1, R2_BLACK);
    HDC c2 = GetDC(cls2);
    printf("class: same %d rop %d window2 %d\n", c1 == c2, GetROP2(c2) == R2_BLACK, WindowFromDC(c2) == cls2);
    ReleaseDC(cls1, c1);
    printf("class: after release window2 %d\n", WindowFromDC(c2) == cls2);

    /* BeginPaint */
    PAINTSTRUCT ps;
    RedrawWindow(cache, NULL, 0, RDW_VALIDATE | RDW_NOFRAME | RDW_NOERASE);
    SetRect(&r, 10, 10, 20, 20);
    RedrawWindow(cache, &r, 0, RDW_INVALIDATE | RDW_NOERASE);
    dc = BeginPaint(cache, &ps);
    clipbox("cache paint", dc, cache);
    printf("paint rect (%ld,%ld)-(%ld,%ld)\n", ps.rcPaint.left, ps.rcPaint.top, ps.rcPaint.right, ps.rcPaint.bottom);
    EndPaint(cache, &ps);
    printf("after EndPaint rop %d\n", GetROP2(dc));
    RedrawWindow(own, NULL, 0, RDW_VALIDATE | RDW_NOFRAME | RDW_NOERASE);
    RedrawWindow(own, &r, 0, RDW_INVALIDATE | RDW_NOERASE);
    dc = BeginPaint(own, &ps);
    ReleaseDC(own, dc);
    clipbox("own paint after ReleaseDC", dc, own);
    printf("own paint GetDC same %d\n", GetDC(own) == dc);
    EndPaint(own, &ps);
    clipbox("own after EndPaint", dc, own);
    RedrawWindow(own, NULL, 0, RDW_VALIDATE | RDW_NOFRAME | RDW_NOERASE);
    printf("validated: update %d\n", GetUpdateRect(own, &r, FALSE));

    /* a CS_PARENTDC child draws into its parent */
    RECT pr;
    dc = GetDC(pchild);
    GetClipBox(dc, &r);
    MapWindowPoints(pchild, parent, (POINT *)&r, 2);
    GetClientRect(parent, &pr);
    printf("parentdc: clip is the parent's client %d", same(&r, &pr));
    COLORREF set = SetPixel(dc, 10, 10, RGB(255, 0, 0));
    ReleaseDC(pchild, dc);
    dc = GetDC(parent);
    printf(", pixel drawn %d\n", set != (COLORREF)-1 && GetPixel(dc, 50, 50) == set);
    ReleaseDC(parent, dc);

    /* ScrollWindowEx */
    UpdateWindow(cache);
    RECT clip, upd;
    SetRect(&clip, 25, 25, 50, 50);
    int k = ScrollWindowEx(cache, -5, -10, NULL, &clip, NULL, &upd, SW_INVALIDATE);
    printf("scroll: %d update inside the clip %d not empty %d\n", k, upd.left >= 25 && upd.top >= 25 && upd.right <= 50 && upd.bottom <= 50, !IsRectEmpty(&upd));
    dc = BeginPaint(cache, &ps);
    GetClipBox(dc, &r);
    printf("scroll: paint inside the clip %d\n", r.left >= 25 && r.top >= 25 && r.right <= 50 && r.bottom <= 50);
    EndPaint(cache, &ps);

    /* destroyed windows */
    dc = GetDC(cache);
    DestroyWindow(cache);
    a1 = GetROP2(dc);
    a2 = ReleaseDC(cache, dc);
    printf("destroyed cache: rop %d release %d getdc %d\n", a1, a2, GetDC(cache) != NULL);
    c2 = GetDC(cls2);
    SetROP2(c2, R2_WHITE);
    DestroyWindow(cls2);
    a1 = GetROP2(c2);
    a2 = WindowFromDC(c2) != NULL;
    printf("destroyed class: rop %d window %d release %d\n", a1, a2, ReleaseDC(cls2, c2));
    dc = GetDC(own);
    DestroyWindow(own);
    a1 = GetROP2(dc);
    a2 = ReleaseDC(own, dc);
    printf("destroyed own: rop %d release %d getdc %d\n", a1, a2, GetDC(own) != NULL);
    DestroyWindow(cls1);
    DestroyWindow(parent);
    return 0;
}
