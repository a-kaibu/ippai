/* The tab control (SysTabControl32, comctl32 5.x as an exe without a common controls 6
 * manifest gets it), as property-sheet-like programs (CPU-Z) use it: the layout of single-line
 * and multi-line tabs (TCM_GETITEMRECT, TCM_ADJUSTRECT, TCM_GETROWCOUNT, TCM_HITTEST), what
 * TCM_SETITEMSIZE / TCM_SETPADDING / TCM_SETMINTABWIDTH / TCS_FIXEDWIDTH change, the selection
 * (TCM_SETCURSEL, TCM_DELETEITEM, TCM_DELETEALLITEMS) and the notifications a click sends
 * (TCN_SELCHANGING may veto), item text and lParam round trips.
 * Sizes are printed relative to the font's metrics (text extents, tmHeight, tmAveCharWidth),
 * so that the rules compare and not the font sake has instead of Windows'.
 * needs: fonts
 * libs: -lcomctl32 -lgdi32 */
#include "../probe.h"
#include <commctrl.h>

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-64s = %lld\n", #expr, r_); } while (0)

static int veto;
static HWND tab;
static LRESULT CALLBACK parent_proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_NOTIFY) {
        NMHDR *n = (NMHDR *)l;
        printf("  notify code %d id %u from-tab %d wparam %u cursel %d\n", (int)n->code, (unsigned)n->idFrom, n->hwndFrom == tab,
               (unsigned)w, (int)SendMessageW(tab, TCM_GETCURSEL, 0, 0));
        if (n->code == TCN_SELCHANGING) return veto;
        return 0;
    }
    if (m == WM_COMMAND) printf("  command %08lx\n", (unsigned long)w);
    return DefWindowProcW(h, m, w, l);
}

static HFONT font;
static TEXTMETRICW tm;
static SIZE extent(const wchar_t *s) {
    HDC dc = GetDC(tab);
    HGDIOBJ old = SelectObject(dc, font);
    SIZE sz;
    GetTextExtentPoint32W(dc, s, lstrlenW(s), &sz);
    SelectObject(dc, old);
    ReleaseDC(tab, dc);
    return sz;
}

static const wchar_t *names[] = { L"CPU", L"Caches", L"Mainboard", L"Memory", L"SPD", L"Graphics", L"Bench", L"About", L"" };

static void insert(int i, const wchar_t *text, LPARAM param) {
    TCITEMW it = { 0 };
    it.mask = TCIF_TEXT | TCIF_PARAM;
    it.pszText = (wchar_t *)text;
    it.lParam = param;
    printf("insert %d -> %d\n", i, (int)SendMessageW(tab, TCM_INSERTITEMW, i, (LPARAM)&it));
}

/* how layout() prints widths and heights: over the text / six average characters and tmHeight
 * (AUTO), as set (FIXED_W: TCS_FIXEDWIDTH, FIXED_H: TCM_SETITEMSIZE's height), or for
 * justified multi-line rows where each row ends (MULTI) */
enum { AUTO, FIXED_W, FIXED_H, MULTI };
static int mode;

/* each item: whether it is shown, its row and top within it, its height, where it starts
 * against the previous one's right, its width (see mode) */
static void layout(const char *label) {
    int n = (int)SendMessageW(tab, TCM_GETITEMCOUNT, 0, 0);
    printf("%s: %d items, %d rows, cursel %d\n", label, n, (int)SendMessageW(tab, TCM_GETROWCOUNT, 0, 0), (int)SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    LONG prev = 0, lowest = 0;
    for (int i = 0; i < n; i++) {
        RECT r = { -7, -7, -7, -7 };
        wchar_t buf[64] = { 0 };
        TCITEMW it = { 0 };
        it.mask = TCIF_TEXT;
        it.pszText = buf;
        it.cchTextMax = 64;
        SendMessageW(tab, TCM_GETITEMW, i, (LPARAM)&it);
        SIZE sz = extent(buf);
        int ok = (int)SendMessageW(tab, TCM_GETITEMRECT, i, (LPARAM)&r);
        LONG h = r.bottom - r.top, w = r.right - r.left, row = h > 0 ? (r.top - 2) / h : -1;
        printf("  item %d ret %d row %ld top-in-row %ld ", i, ok, row, r.top - row * h);
        if (mode == FIXED_H) printf("height %ld", h);
        else printf("height-tm %ld", h - tm.tmHeight);
        printf(" left-prev %ld ", r.left - prev);
        RECT next = { 0 };
        if (mode == FIXED_W) printf("width %ld\n", w);
        else if (mode == MULTI) {
            /* the last of a row: its right against the client's */
            RECT c;
            GetClientRect(tab, &c);
            if (i == n - 1 || !SendMessageW(tab, TCM_GETITEMRECT, i + 1, (LPARAM)&next) || next.top != r.top)
                printf("row-end right-client %ld\n", r.right - c.right);
            else printf("\n");
        } else printf("width-max(text,6avechar) %ld\n", w - max(sz.cx, 6 * tm.tmAveCharWidth));
        prev = r.right;
        if (r.bottom > lowest) lowest = r.bottom;
    }
    RECT c, a;
    GetClientRect(tab, &c);
    a = c;
    SendMessageW(tab, TCM_ADJUSTRECT, FALSE, (LPARAM)&a);
    if (n) printf("  display %ld,(lowest tab's bottom)%+ld,%ld,%ld of client %ldx%ld\n", a.left, a.top - lowest, a.right - c.right, a.bottom - c.bottom, c.right, c.bottom);
    else printf("  display %ld,%ld,%ld,%ld of client %ldx%ld\n", a.left, a.top, a.right - c.right, a.bottom - c.bottom, c.right, c.bottom);
    RECT b = a;
    SendMessageW(tab, TCM_ADJUSTRECT, TRUE, (LPARAM)&b);
    printf("  larger again %ld,%ld,%ld,%ld\n", b.left - c.left, b.top - c.top, b.right - c.right, b.bottom - c.bottom);
}

static void hit(const char *where, int x, int y) {
    TCHITTESTINFO h = { { x, y }, 0xdead };
    int i = (int)SendMessageW(tab, TCM_HITTEST, 0, (LPARAM)&h);
    printf("hittest %s -> %d flags %u\n", where, i, h.flags);
}

static void click(int item) {
    RECT r;
    SendMessageW(tab, TCM_GETITEMRECT, item, (LPARAM)&r);
    LPARAM at = MAKELPARAM((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    printf("click %d (veto %d)\n", item, veto);
    SendMessageW(tab, WM_LBUTTONDOWN, MK_LBUTTON, at);
    SendMessageW(tab, WM_LBUTTONUP, 0, at);
    printf("  cursel %d\n", (int)SendMessageW(tab, TCM_GETCURSEL, 0, 0));
}

static HWND make(HWND parent, DWORD style, int w, int h) {
    HWND t = CreateWindowExW(0, WC_TABCONTROLW, L"", WS_CHILD | WS_VISIBLE | style, 0, 0, w, h, parent, (HMENU)77, GetModuleHandleW(NULL), NULL);
    SendMessageW(t, WM_SETFONT, (WPARAM)font, 0);
    return t;
}

int main(void) {
    INITCOMMONCONTROLSEX icc = { sizeof icc, ICC_TAB_CLASSES };
    E(InitCommonControlsEx(&icc));
    WNDCLASSW wc = { 0 };
    wc.lpfnWndProc = parent_proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"tabparent";
    RegisterClassW(&wc);
    HWND parent = CreateWindowExW(0, L"tabparent", L"tabs", WS_OVERLAPPEDWINDOW, 0, 0, 800, 600, NULL, NULL, wc.hInstance, NULL);
    font = CreateFontW(-11, 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, 0, 0, 0, 0, L"Tahoma");

    tab = make(parent, 0, 500, 300);
    {
        HDC dc = GetDC(tab);
        HGDIOBJ old = SelectObject(dc, font);
        GetTextMetricsW(dc, &tm);
        SelectObject(dc, old);
        ReleaseDC(tab, dc);
    }
    E(SendMessageW(tab, WM_GETFONT, 0, 0) == (LRESULT)font);
    E(GetWindowLongW(tab, GWL_STYLE) & 0xffff);
    E(SendMessageW(tab, TCM_GETITEMCOUNT, 0, 0));
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    E(SendMessageW(tab, TCM_GETROWCOUNT, 0, 0));
    RECT r = { -7, -7, -7, -7 };
    E(SendMessageW(tab, TCM_GETITEMRECT, 0, (LPARAM)&r));
    printf("empty itemrect %ld,%ld,%ld height-tm %ld\n", r.left, r.top, r.right, r.bottom - r.top - tm.tmHeight);
    layout("empty");

    /* the first insert does not select */
    insert(0, names[0], 100);
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    for (int i = 1; i < 8; i++) insert(i + 5, names[i], 100 + i);
    insert(-1, L"neg", 1);
    E(SendMessageW(tab, TCM_DELETEITEM, 8, 0));
    E(SendMessageW(tab, TCM_DELETEITEM, 8, 0));
    layout("eight");

    /* lParam and text round trips, a short buffer */
    {
        wchar_t buf[4] = { 'x', 'x', 'x', 'x' };
        TCITEMW it = { 0 };
        it.mask = TCIF_TEXT | TCIF_PARAM;
        it.pszText = buf;
        it.cchTextMax = 4;
        E(SendMessageW(tab, TCM_GETITEMW, 2, (LPARAM)&it));
        printf("item 2 text '%ls' (same buffer %d) param %ld\n", it.pszText, it.pszText == buf, (long)it.lParam);
        char abuf[16];
        TCITEMA ia = { 0 };
        ia.mask = TCIF_TEXT;
        ia.pszText = abuf;
        ia.cchTextMax = 16;
        E(SendMessageA(tab, TCM_GETITEMA, 5, (LPARAM)&ia));
        printf("item 5 text A '%s'\n", ia.pszText);
        E(SendMessageW(tab, TCM_GETITEMW, 99, (LPARAM)&it));
        it.mask = TCIF_TEXT;
        it.pszText = L"Mainboard and more";
        E(SendMessageW(tab, TCM_SETITEMW, 2, (LPARAM)&it));
        layout("longer text");
        it.pszText = L"Mainboard";
        SendMessageW(tab, TCM_SETITEMW, 2, (LPARAM)&it);
    }

    hit("left of the tabs", -5, 5);
    RECT i3;
    SendMessageW(tab, TCM_GETITEMRECT, 3, (LPARAM)&i3);
    hit("item 3 middle", (i3.left + i3.right) / 2, (i3.top + i3.bottom) / 2);
    hit("item 3 top left", i3.left, i3.top);
    hit("item 3 top right", i3.right, i3.top);
    hit("item 3 bottom", (i3.left + i3.right) / 2, i3.bottom);
    /* where the label starts: inside the horizontal / vertical padding (6, 3) or not */
    for (int d = 1; d <= 8; d++) hit("item 3 left+d, middle", i3.left + d, (i3.top + i3.bottom) / 2);
    for (int d = 1; d <= 5; d++) hit("item 3 middle, top+d", (i3.left + i3.right) / 2, i3.top + d);
    hit("item 3 right-1, middle", i3.right - 1, (i3.top + i3.bottom) / 2);
    hit("item 3 middle, bottom-1", (i3.left + i3.right) / 2, i3.bottom - 1);
    hit("display area", 10, 200);

    /* selection: SETCURSEL sends nothing, a click asks the parent first */
    E(SendMessageW(tab, TCM_SETCURSEL, 2, 0));
    E(SendMessageW(tab, TCM_SETCURSEL, 2, 0));
    E(SendMessageW(tab, TCM_SETCURSEL, 99, 0));
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    E(SendMessageW(tab, TCM_GETCURFOCUS, 0, 0));
    layout("selected 2");
    click(4);
    click(4);
    veto = 1;
    click(6);
    veto = 0;
    click(0);
    E(SendMessageW(tab, TCM_SETCURSEL, 5, 0));
    E(SendMessageW(tab, TCM_DELETEITEM, 2, 0));
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    E(SendMessageW(tab, TCM_DELETEITEM, 4, 0));
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    layout("after deletes");

    /* sizes: fixed width, item size, padding, minimum width */
    E(SendMessageW(tab, TCM_SETMINTABWIDTH, 0, 80));
    E(SendMessageW(tab, TCM_SETMINTABWIDTH, 0, -1));
    E(SendMessageW(tab, TCM_SETPADDING, 0, MAKELPARAM(10, 5)));
    layout("padding 10,5");
    SendMessageW(tab, TCM_SETPADDING, 0, MAKELPARAM(6, 3));
    layout("padding 6,3");
    /* padding takes effect at the next layout change (here TCM_SETMINTABWIDTH): item 0's height
     * and width for each */
    static const int pads[][2] = { { 6, 0 }, { 6, 1 }, { 6, 2 }, { 6, 3 }, { 6, 4 }, { 6, 6 }, { 6, 9 }, { 0, 3 }, { 2, 3 }, { 9, 3 }, { 6, 3 } };
    for (unsigned k = 0; k < sizeof pads / sizeof *pads; k++) {
        SendMessageW(tab, TCM_SETPADDING, 0, MAKELPARAM(pads[k][0], pads[k][1]));
        SendMessageW(tab, TCM_SETMINTABWIDTH, 0, -1);
        RECT p0, a;
        SendMessageW(tab, TCM_GETITEMRECT, 0, (LPARAM)&p0);
        GetClientRect(tab, &a);
        SendMessageW(tab, TCM_ADJUSTRECT, FALSE, (LPARAM)&a);
        SIZE sz = extent(names[0]);
        printf("padding %d,%d: item 0 top %ld height-tm %ld width-max(text,6avechar) %ld display top-item0bottom %ld\n", pads[k][0], pads[k][1], p0.top,
               p0.bottom - p0.top - tm.tmHeight, p0.right - p0.left - max(sz.cx, 6 * tm.tmAveCharWidth), a.top - p0.bottom);
    }
    E(SendMessageW(tab, TCM_SETITEMSIZE, 0, MAKELPARAM(70, 30)));
    E(SendMessageW(tab, TCM_SETITEMSIZE, 0, MAKELPARAM(70, 30)));
    mode = FIXED_H;
    layout("item size 70x30");
    mode = AUTO;
    E(SendMessageW(tab, TCM_DELETEALLITEMS, 0, 0));
    E(SendMessageW(tab, TCM_GETCURSEL, 0, 0));
    E(SendMessageW(tab, TCM_GETITEMCOUNT, 0, 0));
    DestroyWindow(tab);

    tab = make(parent, TCS_FIXEDWIDTH, 500, 300);
    E(SendMessageW(tab, TCM_SETITEMSIZE, 0, MAKELPARAM(90, 0)));
    for (int i = 0; i < 4; i++) insert(i, names[i], i);
    mode = FIXED_W;
    layout("fixed width 90");
    mode = AUTO;
    DestroyWindow(tab);

    /* multi-line: rows of a narrow control, the selected row moves next to the display */
    tab = make(parent, TCS_MULTILINE, 220, 300);
    for (int i = 0; i < 8; i++) insert(i, names[i], i);
    mode = MULTI;
    layout("multiline");
    SendMessageW(tab, TCM_SETCURSEL, 0, 0);
    layout("multiline sel 0");
    SendMessageW(tab, TCM_SETCURSEL, 6, 0);
    layout("multiline sel 6");
    mode = AUTO;
    DestroyWindow(tab);

    /* too many for a narrow single-line control: the items keep their places (scrolled) */
    tab = make(parent, 0, 200, 100);
    for (int i = 0; i < 8; i++) insert(i, names[i], i);
    layout("narrow");
    DestroyWindow(tab);

    DestroyWindow(parent);
    return 0;
}
