/* libs: -luser32 */
/* The ListBox control (Inno Setup's wizard fills its task and component lists with one and
 * checks LB_GETCOUNT against its own items): strings kept sorted with LBS_SORT (LB_INSERTSTRING
 * never sorts), item data, LB_FIND*, the selection in single and multiple-selection lists,
 * an owner-drawn list without strings (WM_MEASUREITEM per item, WM_DELETEITEM on removal,
 * the data as the "text"), and what the parent hears: messages that change the selection send
 * nothing, a click or an arrow key sends LBN_SELCHANGE with LBS_NOTIFY and nothing without it.
 * (sake: listbox.rs; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <stdio.h>

static int codes[32], n;
static char log_[512];

static LRESULT CALLBACK proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg == WM_COMMAND && lp && n < 32 && HIWORD(wp) != LBN_SETFOCUS && HIWORD(wp) != LBN_KILLFOCUS)
        codes[n++] = (LOWORD(wp) << 16) | HIWORD(wp);
    if (msg == WM_MEASUREITEM) {
        MEASUREITEMSTRUCT *m = (MEASUREITEMSTRUCT *)lp;
        if (m->CtlID == 30) {
            sprintf(log_ + strlen(log_), " measure(type %u id %u item %u data %lu)", m->CtlType, m->CtlID, m->itemID, (unsigned long)m->itemData);
            m->itemHeight = 10 + (UINT)m->itemData;
        }
        return TRUE;
    }
    if (msg == WM_DELETEITEM) {
        DELETEITEMSTRUCT *d = (DELETEITEMSTRUCT *)lp;
        sprintf(log_ + strlen(log_), " delete(type %u id %u item %u data %lu)", d->CtlType, d->CtlID, d->itemID, (unsigned long)d->itemData);
        return TRUE;
    }
    if (msg == WM_DRAWITEM)
        return TRUE;
    return DefWindowProcA(h, msg, wp, lp);
}

static void report(const char *what) {
    printf("%s:", what);
    for (int i = 0; i < n; i++)
        printf(" %d/%x", codes[i] >> 16, codes[i] & 0xffff);
    printf("%s\n", log_);
    n = 0;
    log_[0] = 0;
}

static void items(HWND l) {
    int k = (int)SendMessageA(l, LB_GETCOUNT, 0, 0);
    printf("  %d items:", k);
    for (int i = 0; i < k; i++) {
        char t[64] = "";
        int len = (int)SendMessageA(l, LB_GETTEXTLEN, i, 0);
        int got = (int)SendMessageA(l, LB_GETTEXT, i, (LPARAM)t);
        LRESULT d = SendMessageA(l, LB_GETITEMDATA, i, 0);
        printf(" %s(%d/%d,%ld)", t, len, got, (long)d);
    }
    printf("\n");
}

static void click(HWND l, int i) {
    RECT r;
    SendMessageA(l, LB_GETITEMRECT, i, (LPARAM)&r);
    LPARAM p = MAKELPARAM(r.left + 2, r.top + 1);
    SendMessageA(l, WM_LBUTTONDOWN, MK_LBUTTON, p);
    SendMessageA(l, WM_LBUTTONUP, 0, p);
}

static void key(HWND l, int vk) {
    SendMessageA(l, WM_KEYDOWN, vk, 1);
    SendMessageA(l, WM_KEYUP, vk, 0xc0000001);
}

int main(void) {
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "SakeListBox";
    RegisterClassA(&wc);
    HWND top = CreateWindowExA(0, "SakeListBox", "list", WS_OVERLAPPEDWINDOW, 10, 10, 400, 400, NULL, NULL, wc.hInstance, NULL);

    HWND l = CreateWindowExA(0, "ListBox", "", WS_CHILD | WS_VISIBLE | LBS_SORT | LBS_NOTIFY | WS_VSCROLL, 10, 10, 150, 200, top, (HMENU)10, wc.hInstance, NULL);
    const char *words[] = {"cherry", "Apple", "banana", "apricot", "Date"};
    printf("add:");
    for (int i = 0; i < 5; i++) {
        int r = (int)SendMessageA(l, LB_ADDSTRING, 0, (LPARAM)words[i]);
        printf(" %d", r);
    }
    printf("\n");
    int r = (int)SendMessageA(l, LB_INSERTSTRING, 1, (LPARAM)"zz");
    printf("insert at 1: %d\n", r);
    r = (int)SendMessageA(l, LB_INSERTSTRING, -1, (LPARAM)"aa");
    printf("insert at end: %d\n", r);
    r = (int)SendMessageA(l, LB_INSERTSTRING, 99, (LPARAM)"x");
    printf("insert past end: %d\n", r);
    SendMessageA(l, LB_SETITEMDATA, 2, 42);
    items(l);
    r = (int)SendMessageA(l, LB_FINDSTRING, -1, (LPARAM)"AP");
    printf("find AP: %d\n", r);
    r = (int)SendMessageA(l, LB_FINDSTRING, r, (LPARAM)"AP");
    printf("find AP after it: %d\n", r);
    r = (int)SendMessageA(l, LB_FINDSTRINGEXACT, -1, (LPARAM)"BANANA");
    printf("exact BANANA: %d\n", r);
    r = (int)SendMessageA(l, LB_FINDSTRINGEXACT, -1, (LPARAM)"ban");
    printf("exact ban: %d\n", r);
    r = (int)SendMessageA(l, LB_FINDSTRING, -1, (LPARAM)"q");
    printf("find q: %d\n", r);
    report("filling");

    r = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("cursel at first: %d\n", r);
    r = (int)SendMessageA(l, LB_SETCURSEL, 3, 0);
    int c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    int s3 = (int)SendMessageA(l, LB_GETSEL, 3, 0);
    int s2 = (int)SendMessageA(l, LB_GETSEL, 2, 0);
    printf("setcursel 3: %d, cursel %d, sel(3) %d sel(2) %d\n", r, c, s3 != 0, s2);
    r = (int)SendMessageA(l, LB_SETCURSEL, 50, 0);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("setcursel 50: %d, cursel %d\n", r, c);
    r = (int)SendMessageA(l, LB_SETCURSEL, 3, 0);
    r = (int)SendMessageA(l, LB_SELECTSTRING, -1, (LPARAM)"ch");
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("selectstring ch: %d, cursel %d\n", r, c);
    r = (int)SendMessageA(l, LB_GETSELCOUNT, 0, 0);
    printf("selcount on a single-selection list: %d\n", r);
    report("selecting by message");

    click(l, 1);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("clicked 1: cursel %d\n", c);
    report("click");
    click(l, 1);
    report("click the same item");
    key(l, VK_DOWN);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("down: cursel %d\n", c);
    key(l, VK_END);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("end: cursel %d\n", c);
    key(l, VK_HOME);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("home: cursel %d\n", c);
    report("keys");

    r = (int)SendMessageA(l, LB_DELETESTRING, 0, 0);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("delete 0: %d left, cursel %d\n", r, c);
    r = (int)SendMessageA(l, LB_DELETESTRING, 40, 0);
    printf("delete 40: %d\n", r);
    report("delete (data 42 is on item 1)");
    SendMessageA(l, LB_RESETCONTENT, 0, 0);
    r = (int)SendMessageA(l, LB_GETCOUNT, 0, 0);
    c = (int)SendMessageA(l, LB_GETCURSEL, 0, 0);
    printf("reset: %d items, cursel %d\n", r, c);
    report("reset");

    /* the same clicks without LBS_NOTIFY */
    HWND q = CreateWindowExA(0, "ListBox", "", WS_CHILD | WS_VISIBLE, 170, 10, 100, 100, top, (HMENU)11, wc.hInstance, NULL);
    SendMessageA(q, LB_ADDSTRING, 0, (LPARAM)"one");
    SendMessageA(q, LB_ADDSTRING, 0, (LPARAM)"two");
    click(q, 1);
    key(q, VK_UP);
    c = (int)SendMessageA(q, LB_GETCURSEL, 0, 0);
    printf("quiet list: cursel %d\n", c);
    report("quiet list");

    /* multiple selection */
    HWND m = CreateWindowExA(0, "ListBox", "", WS_CHILD | WS_VISIBLE | LBS_MULTIPLESEL | LBS_NOTIFY, 10, 220, 100, 100, top, (HMENU)20, wc.hInstance, NULL);
    for (int i = 0; i < 6; i++) {
        char t[8];
        sprintf(t, "m%d", i);
        SendMessageA(m, LB_ADDSTRING, 0, (LPARAM)t);
    }
    r = (int)SendMessageA(m, LB_SETSEL, TRUE, 1);
    int r2 = (int)SendMessageA(m, LB_SETSEL, TRUE, 4);
    int cnt = (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0);
    printf("setsel 1, 4: %d %d, count %d\n", r, r2, cnt);
    int sel[8] = {0};
    cnt = (int)SendMessageA(m, LB_GETSELITEMS, 8, (LPARAM)sel);
    printf("selitems %d: %d %d\n", cnt, sel[0], sel[1]);
    r = (int)SendMessageA(m, LB_SETCURSEL, 2, 0);
    printf("setcursel on a multiple-selection list: %d\n", r);
    SendMessageA(m, LB_SETSEL, TRUE, -1);
    printf("select all: %d\n", (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0));
    SendMessageA(m, LB_SETSEL, FALSE, -1);
    printf("select none: %d\n", (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0));
    r = (int)SendMessageA(m, LB_SELITEMRANGE, TRUE, MAKELPARAM(2, 4));
    printf("range 2-4: %d, count %d, sel(1) %d sel(3) %d\n", r, (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0), (int)SendMessageA(m, LB_GETSEL, 1, 0), (int)SendMessageA(m, LB_GETSEL, 3, 0) != 0);
    click(m, 3);
    printf("click 3 toggles: sel(3) %d count %d\n", (int)SendMessageA(m, LB_GETSEL, 3, 0), (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0));
    click(m, 0);
    printf("click 0 toggles: sel(0) %d count %d\n", (int)SendMessageA(m, LB_GETSEL, 0, 0) != 0, (int)SendMessageA(m, LB_GETSELCOUNT, 0, 0));
    report("multiple selection");

    /* owner-drawn, variable heights, no strings */
    HWND o = CreateWindowExA(0, "ListBox", "", WS_CHILD | WS_VISIBLE | LBS_OWNERDRAWVARIABLE, 170, 220, 100, 100, top, (HMENU)30, wc.hInstance, NULL);
    report("owner-drawn created");
    SendMessageA(o, LB_ADDSTRING, 0, 5);
    SendMessageA(o, LB_ADDSTRING, 0, 7);
    SendMessageA(o, LB_INSERTSTRING, 0, 1);
    report("owner-drawn added");
    printf("  heights %d %d %d, data %ld %ld %ld\n", (int)SendMessageA(o, LB_GETITEMHEIGHT, 0, 0), (int)SendMessageA(o, LB_GETITEMHEIGHT, 1, 0), (int)SendMessageA(o, LB_GETITEMHEIGHT, 2, 0),
           (long)SendMessageA(o, LB_GETITEMDATA, 0, 0), (long)SendMessageA(o, LB_GETITEMDATA, 1, 0), (long)SendMessageA(o, LB_GETITEMDATA, 2, 0));
    RECT ir;
    SendMessageA(o, LB_GETITEMRECT, 2, (LPARAM)&ir);
    printf("  item 2 rect y %ld-%ld\n", ir.top, ir.bottom);
    LRESULT d = 0;
    r = (int)SendMessageA(o, LB_GETTEXT, 1, (LPARAM)&d);
    int tl = (int)SendMessageA(o, LB_GETTEXTLEN, 1, 0);
    printf("  gettext 1: %s, %ld; textlen %s\n", r == sizeof(ULONG_PTR) ? "sizeof(ULONG_PTR)" : "?", (long)d, tl == sizeof(ULONG_PTR) ? "sizeof(ULONG_PTR)" : "?");
    printf("  find data 7: %d\n", (int)SendMessageA(o, LB_FINDSTRING, -1, 7));
    SendMessageA(o, LB_DELETESTRING, 1, 0);
    report("owner-drawn delete 1");
    DestroyWindow(o);
    report("owner-drawn destroyed");
    DestroyWindow(top);
    return 0;
}
