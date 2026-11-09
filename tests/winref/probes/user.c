/* user32 as game loops depend on it: rectangle arithmetic (empty and inverted rectangles), the
 * message queue (posted order, PeekMessage ranges and PM_NOREMOVE, sent messages before posted
 * ones, WM_QUIT last, WM_TIMER and WM_PAINT generated only when nothing is posted, GetQueueStatus
 * bits, message time/pos fields aside), the messages CreateWindow / ShowWindow / SetWindowPos /
 * DestroyWindow send to the window procedure and their order, window longs and styles,
 * client/window rectangles of fixed styles, class registration errors, window properties;
 * windows asked bigger than the screen (kept within WM_GETMINMAXINFO's tracking size by
 * CreateWindowEx and DefWindowProc's WM_WINDOWPOSCHANGING, popups without a sizing frame not).
 * libs: -lgdi32 */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-64s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)

static void prc(const char *l, const RECT *r) { printf("%s %ld,%ld,%ld,%ld\n", l, r->left, r->top, r->right, r->bottom); }

static void rects(void) {
    RECT a = { 0, 0, 10, 10 }, b = { 5, 5, 15, 15 }, e = { 3, 3, 3, 8 }, inv = { 10, 10, 0, 0 }, o;
    BOOL r;
    r = IntersectRect(&o, &a, &b); printf("intersect %d ", r); prc("", &o);
    r = IntersectRect(&o, &a, &e); printf("intersect-empty %d ", r); prc("", &o);
    r = IntersectRect(&o, &a, &inv); printf("intersect-inv %d ", r); prc("", &o);
    RECT apart = { 20, 20, 30, 30 };
    r = IntersectRect(&o, &a, &apart); printf("intersect-apart %d ", r); prc("", &o);
    RECT touch = { 10, 0, 20, 10 };
    r = IntersectRect(&o, &a, &touch); printf("intersect-touch %d ", r); prc("", &o);
    r = UnionRect(&o, &a, &b); printf("union %d ", r); prc("", &o);
    r = UnionRect(&o, &a, &e); printf("union-empty %d ", r); prc("", &o);
    r = UnionRect(&o, &e, &e); printf("union-both-empty %d ", r); prc("", &o);
    r = UnionRect(&o, &a, &inv); printf("union-inv %d ", r); prc("", &o);
    RECT s1 = { 0, 0, 10, 10 }, s2 = { 0, 0, 10, 4 }, s3 = { 2, 2, 4, 4 }, s4 = { 0, 0, 4, 10 }, s5 = { -5, -5, 20, 20 };
    r = SubtractRect(&o, &s1, &s2); printf("subtract-top %d ", r); prc("", &o);
    r = SubtractRect(&o, &s1, &s3); printf("subtract-hole %d ", r); prc("", &o);
    r = SubtractRect(&o, &s1, &s4); printf("subtract-left %d ", r); prc("", &o);
    r = SubtractRect(&o, &s1, &s5); printf("subtract-all %d ", r); prc("", &o);
    o = a; OffsetRect(&o, -3, 7); prc("offset", &o);
    o = a; InflateRect(&o, -6, 2); prc("inflate", &o);
    E(IsRectEmpty(&e)); E(IsRectEmpty(&inv)); E(IsRectEmpty(&a));
    E(EqualRect(&a, &a)); E(EqualRect(&e, &inv));
    POINT pts[] = { { 0, 0 }, { 9, 9 }, { 10, 5 }, { 5, 10 }, { -1, 0 } };
    for (int i = 0; i < 5; i++) printf("ptinrect %ld,%ld %d\n", pts[i].x, pts[i].y, PtInRect(&a, pts[i]));
    E(SetRectEmpty(&o)); prc("empty", &o);
    E(CopyRect(&o, &b)); prc("copy", &o);
}

/* the window procedure logs what it gets, numbered; nested sends are indented */
static int depth, logging;
static const char *mname(UINT m) {
    switch (m) {
#define M(x) case x: return #x;
        M(WM_CREATE) M(WM_DESTROY) M(WM_MOVE) M(WM_SIZE) M(WM_ACTIVATE) M(WM_SETFOCUS) M(WM_KILLFOCUS) M(WM_ENABLE) M(WM_SETTEXT) M(WM_GETTEXT)
        M(WM_GETTEXTLENGTH) M(WM_PAINT) M(WM_CLOSE) M(WM_QUIT) M(WM_ERASEBKGND) M(WM_SHOWWINDOW) M(WM_ACTIVATEAPP) M(WM_SETCURSOR) M(WM_GETMINMAXINFO)
        M(WM_WINDOWPOSCHANGING) M(WM_WINDOWPOSCHANGED) M(WM_STYLECHANGING) M(WM_STYLECHANGED) M(WM_NCCREATE) M(WM_NCDESTROY) M(WM_NCCALCSIZE)
        M(WM_NCHITTEST) M(WM_NCPAINT) M(WM_NCACTIVATE) M(WM_TIMER) M(WM_USER) M(WM_SYNCPAINT) M(WM_GETICON) M(WM_CHILDACTIVATE) M(WM_PARENTNOTIFY)
        M(WM_IME_SETCONTEXT) M(WM_IME_NOTIFY) M(WM_QUERYOPEN) M(WM_MOVING) M(WM_SIZING) M(WM_ENTERSIZEMOVE) M(WM_EXITSIZEMOVE)
#undef M
    default: return NULL;
    }
}
static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    const char *n = mname(m);
    if (logging && n) {
        printf("%*s%s", depth * 2, "", n);
        if (m == WM_SIZE) printf(" %u %ux%u", (unsigned)w, LOWORD(l), HIWORD(l));
        if (m == WM_SHOWWINDOW || m == WM_ACTIVATE || m == WM_ACTIVATEAPP || m == WM_NCACTIVATE || m == WM_TIMER || m >= WM_USER) printf(" w %u", (unsigned)w);
        if (m == WM_WINDOWPOSCHANGING || m == WM_WINDOWPOSCHANGED) {
            WINDOWPOS *p = (WINDOWPOS *)l;
            printf(" %d,%d %dx%d flags %x", p->x, p->y, p->cx, p->cy, p->flags & ~0x1800); /* (SWP_NOCLIENTSIZE/MOVE: internal) */
        }
        if (m == WM_STYLECHANGING || m == WM_STYLECHANGED) printf(" %d %lx->%lx", (int)w, ((STYLESTRUCT *)l)->styleOld, ((STYLESTRUCT *)l)->styleNew);
        printf("\n");
    }
    depth++;
    LRESULT r = m == WM_USER + 1 ? 77 : DefWindowProcA(h, m, w, l);
    depth--;
    return r;
}

static void drain(const char *label) {
    MSG msg;
    printf("%s:", label);
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
        const char *n = mname(msg.message);
        if (n) printf(" %s(%u)", n, (unsigned)msg.wParam); else if (msg.message < 0xc000) printf(" %#x", msg.message);
        DispatchMessageA(&msg);
    }
    printf("\n");
}

static int bigmax;
static LRESULT CALLBACK bigproc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_GETMINMAXINFO && bigmax) {
        MINMAXINFO *i = (MINMAXINFO *)l;
        i->ptMaxTrackSize.x = i->ptMaxTrackSize.y = bigmax;
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

/* what a window asked 5000x5000 gets, as 'tx'/'ty' for SM_CX/CYMAXTRACK, else its size */
static void bigsize(const char *label, HWND h) {
    RECT r;
    GetWindowRect(h, &r);
    int w = r.right - r.left, ht = r.bottom - r.top, mx = GetSystemMetrics(SM_CXMAXTRACK), my = GetSystemMetrics(SM_CYMAXTRACK);
    char a[16], b[16];
    if (w == mx) strcpy(a, "tx"); else sprintf(a, "%d", w);
    if (ht == my) strcpy(b, "ty"); else sprintf(b, "%d", ht);
    printf("%s: %s x %s\n", label, a, b);
}

static void big(void) {
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = bigproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "ippai-big";
    RegisterClassA(&wc);
    printf("maxtrack beyond screen %d %d\n", GetSystemMetrics(SM_CXMAXTRACK) > GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYMAXTRACK) > GetSystemMetrics(SM_CYSCREEN));
    printf("maxtrack x != y %d\n", GetSystemMetrics(SM_CXMAXTRACK) != GetSystemMetrics(SM_CYMAXTRACK));
    static const struct { const char *name; DWORD style; } st[] = {
        { "overlapped", WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX },
        { "overlappedwindow", WS_OVERLAPPEDWINDOW },
        { "popup", WS_POPUP },
        { "popup-thickframe", WS_POPUP | WS_THICKFRAME },
        { "popup-caption", WS_POPUP | WS_CAPTION },
    };
    for (int k = 0; k < 5; k++) {
        char l[64];
        HWND h = CreateWindowExA(0, "ippai-big", "big", st[k].style, 0, 0, 5000, 5000, NULL, NULL, wc.hInstance, NULL);
        sprintf(l, "create %s", st[k].name); bigsize(l, h);
        SetWindowPos(h, NULL, 0, 0, 100, 100, SWP_NOZORDER | SWP_NOACTIVATE);
        SetWindowPos(h, NULL, 0, 0, 5000, 5000, SWP_NOZORDER | SWP_NOACTIVATE);
        sprintf(l, "setpos %s", st[k].name); bigsize(l, h);
        MoveWindow(h, 0, 0, 5000, 5000, FALSE);
        sprintf(l, "move %s", st[k].name); bigsize(l, h);
        DestroyWindow(h);
    }
    /* a window procedure that allows more: it gets it */
    bigmax = 6000;
    HWND h = CreateWindowExA(0, "ippai-big", "big", WS_OVERLAPPEDWINDOW, 0, 0, 5000, 5000, NULL, NULL, wc.hInstance, NULL);
    bigsize("create allowed", h);
    SetWindowPos(h, NULL, 0, 0, 5500, 5500, SWP_NOZORDER | SWP_NOACTIVATE);
    bigsize("setpos allowed", h);
    DestroyWindow(h);
    /* and one that allows less */
    bigmax = 300;
    h = CreateWindowExA(0, "ippai-big", "big", WS_OVERLAPPEDWINDOW, 0, 0, 640, 480, NULL, NULL, wc.hInstance, NULL);
    bigsize("create max 300", h);
    SetWindowPos(h, NULL, 0, 0, 200, 900, SWP_NOZORDER | SWP_NOACTIVATE);
    bigsize("setpos max 300", h);
    /* SWP_NOSIZE keeps the size it has */
    SetWindowPos(h, NULL, 5, 5, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE);
    bigsize("nosize max 300", h);
    DestroyWindow(h);
    bigmax = 0;
    UnregisterClassA("ippai-big", wc.hInstance);
}

int main(void) {
    rects();
    big();

    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "ippai-winref";
    wc.hbrBackground = (HBRUSH)GetStockObject(BLACK_BRUSH);
    E(RegisterClassA(&wc) != 0);
    E(RegisterClassA(&wc) != 0);
    WNDCLASSA got;
    E(GetClassInfoA(wc.hInstance, "ippai-winref", &got));
    E(GetClassInfoA(wc.hInstance, "ippai-nothere", &got));

    /* creation: the messages, then the rectangles of a fixed-size bordered window */
    logging = 1;
    printf("-- create hidden\n");
    HWND h = CreateWindowExA(0, "ippai-winref", "title", WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, 100, 50, 640, 480, NULL, NULL, wc.hInstance, NULL);
    logging = 0;
    E(h != NULL);
    RECT wr, cr;
    GetWindowRect(h, &wr); GetClientRect(h, &cr);
    printf("window %ldx%ld client %ldx%ld at %ld,%ld\n", wr.right - wr.left, wr.bottom - wr.top, cr.right, cr.bottom, wr.left, wr.top);
    RECT adj = { 0, 0, 640, 480 };
    E(AdjustWindowRectEx(&adj, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX, FALSE, 0)); prc("adjust", &adj);
    adj = (RECT){ 0, 0, 640, 480 };
    E(AdjustWindowRectEx(&adj, WS_POPUP, FALSE, 0)); prc("adjust-popup", &adj);
    adj = (RECT){ 0, 0, 640, 480 };
    E(AdjustWindowRectEx(&adj, WS_OVERLAPPEDWINDOW, TRUE, 0)); prc("adjust-menu", &adj);
    E(GetWindowLongA(h, GWL_STYLE));
    E(GetWindowLongA(h, GWL_EXSTYLE));
    E(IsWindowVisible(h));
    E(GetWindowTextLengthA(h));
    E(IsWindow(h));
    E(IsWindow((HWND)(ULONG_PTR)0x1234));

    printf("-- SetWindowLong style\n");
    logging = 1;
    E(SetWindowLongA(h, GWL_STYLE, WS_POPUP));
    logging = 0;
    E(GetWindowLongA(h, GWL_STYLE));
    E(SetWindowLongA(h, GWLP_USERDATA, 0x1234));
    E(GetWindowLongA(h, GWLP_USERDATA));
    E(SetWindowLongA(h, 4000, 1));

    printf("-- SetWindowPos size\n");
    logging = 1;
    E(SetWindowPos(h, NULL, 10, 20, 800, 600, SWP_NOZORDER | SWP_NOACTIVATE));
    printf("-- SetWindowPos same\n");
    E(SetWindowPos(h, NULL, 10, 20, 800, 600, SWP_NOZORDER | SWP_NOACTIVATE));
    printf("-- SendMessage\n");
    E(SendMessageA(h, WM_USER + 1, 5, 0));
    E(SendMessageA(h, WM_SETTEXT, 0, (LPARAM)"new"));
    logging = 0;
    char t[16];
    E(GetWindowTextA(h, t, sizeof t)); printf("text %s\n", t);
    E(GetWindowTextA(h, t, 2)); printf("text %s\n", t);

    /* the queue */
    drain("start");
    PostMessageA(h, WM_USER + 3, 1, 0);
    PostMessageA(h, WM_USER + 2, 2, 0);
    PostMessageA(h, WM_USER + 5, 3, 0);
    PostMessageA(NULL, WM_USER + 4, 4, 0); /* thread message */
    E(GetQueueStatus(QS_ALLINPUT));
    E(GetQueueStatus(QS_POSTMESSAGE));
    MSG msg;
    E(PeekMessageA(&msg, h, WM_USER + 4, WM_USER + 5, PM_REMOVE)); printf("got %#x %u\n", msg.message, (unsigned)msg.wParam);
    E(PeekMessageA(&msg, NULL, WM_USER + 2, WM_USER + 2, PM_NOREMOVE)); printf("got %#x %u\n", msg.message, (unsigned)msg.wParam);
    E(PeekMessageA(&msg, (HWND)-1, 0, 0, PM_REMOVE)); printf("got %#x %u hwnd-null %d\n", msg.message, (unsigned)msg.wParam, msg.hwnd == NULL);
    E(PeekMessageA(&msg, NULL, WM_USER + 9, WM_USER + 9, PM_REMOVE));
    drain("posted");
    PostQuitMessage(3);
    PostMessageA(h, WM_USER + 6, 6, 0);
    E(GetQueueStatus(QS_POSTMESSAGE));
    E(GetMessageA(&msg, NULL, 0, 0)); printf("got %#x %u\n", msg.message, (unsigned)msg.wParam);
    E(GetMessageA(&msg, NULL, 0, 0)); printf("got %#x %u\n", msg.message, (unsigned)msg.wParam);
    E(PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE));

    /* timers and paint come after posted messages */
    E(SetTimer(h, 9, 1, NULL));
    Sleep(50);
    PostMessageA(h, WM_USER + 7, 7, 0);
    InvalidateRect(h, NULL, FALSE);
    E(GetQueueStatus(QS_TIMER | QS_POSTMESSAGE) & 0xffff);
    E(PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)); printf("first %#x\n", msg.message);
    E(KillTimer(h, 9));
    E(KillTimer(h, 9));
    E(SetTimer(NULL, 0, 10, NULL) != 0);

    /* properties */
    E(SetPropA(h, "p1", (HANDLE)0x55));
    E(GetPropA(h, "p1"));
    E(GetPropA(h, "P1"));
    E(RemovePropA(h, "p1"));
    E(GetPropA(h, "p1"));

    printf("-- ShowWindow\n");
    logging = 1;
    E(ShowWindow(h, SW_SHOWNOACTIVATE));
    E(ShowWindow(h, SW_HIDE));
    E(ShowWindow(h, SW_HIDE));
    printf("-- DestroyWindow\n");
    E(DestroyWindow(h));
    logging = 0;
    E(DestroyWindow(h));
    E(IsWindow(h));
    E(UnregisterClassA("ippai-winref", wc.hInstance));
    E(UnregisterClassA("ippai-winref", wc.hInstance));
    return 0;
}
