#define OEMRESOURCE
/* Window lifetimes the full-read review (b10) found sake getting wrong: RegisterClass with
 * cbClsExtra / cbWndExtra garbage (negative, huge), SetParent cycles (itself, its child or
 * grandchild) and parents that are not windows, and a thread that ends without DestroyWindow:
 * its windows go, and a SendMessage waiting for it returns 0 (sake waited forever). Hidden
 * top-level windows of any size are made and taken apart (sake now gives them pixels only when
 * drawn). Also PostMessage's 10000-message quota, DestroyMenu taking the popups under it (and a
 * window's menu bar going with the window), shared LoadCursor / LoadIcon handles, and window
 * handles staying distinct over many creates and destroys.
 * libs: -lgdi32 */
#include "../probe.h"

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_USER + 1) return 77;
    return DefWindowProcA(h, m, w, l);
}

static void reg(const char *label, int cls, int wnd) {
    WNDCLASSA c = { 0 };
    c.lpfnWndProc = proc;
    c.hInstance = GetModuleHandleA(NULL);
    c.lpszClassName = "probe_extra";
    c.cbClsExtra = cls;
    c.cbWndExtra = wnd;
    SetLastError(0xdead);
    ATOM a = RegisterClassA(&c);
    DWORD e = GetLastError();
    printf("register %s %s err %lu\n", label, a ? "ok" : "fails", a ? 0 : e);
    if (a) {
        HWND h = CreateWindowA("probe_extra", "", WS_POPUP, 0, 0, 10, 10, NULL, NULL, c.hInstance, NULL);
        printf("  window %s extra %ld\n", h ? "ok" : "fails", h ? (long)GetClassLongA(h, GCL_CBWNDEXTRA) : -1);
        if (h && wnd >= 4) {
            SetLastError(0);
            SetWindowLongA(h, wnd - 4, 0x1234);
            printf("  last long %lx err %lu\n", (unsigned long)GetWindowLongA(h, wnd - 4), GetLastError());
        }
        if (h) DestroyWindow(h);
        UnregisterClassA("probe_extra", c.hInstance);
    }
}

static void extras(void) {
    reg("0 0", 0, 0);
    reg("40 40", 40, 40);
    reg("4096 4096", 4096, 4096);
    reg("-1 0", -1, 0);
    reg("0 -1", 0, -1);
    reg("cccccccc", (int)0xcccccccc, (int)0xcccccccc);
    reg("0 0x7fffffff", 0, 0x7fffffff);
}

static HWND make(const char *cls, HWND parent, DWORD style) {
    return CreateWindowA(cls, "", style, 0, 0, 50, 50, parent, NULL, GetModuleHandleA(NULL), NULL);
}

static void parents(void) {
    HWND top = make("probe_life", NULL, WS_POPUP);
    HWND child = make("probe_life", top, WS_CHILD);
    HWND grand = make("probe_life", child, WS_CHILD);
    HWND other = make("probe_life", NULL, WS_POPUP);
#define SP(label, h, p) do { SetLastError(0xdead); HWND r_ = SetParent(h, p); DWORD e_ = GetLastError(); \
        printf("SetParent %-24s %s err %lu\n", label, r_ ? "ok" : "fails", r_ ? 0 : e_); } while (0)
    SP("itself", top, top);
    SP("child of its child", top, child);
    SP("child of its grandchild", top, grand);
    SP("child under itself", child, child);
    SP("not a window", child, (HWND)(INT_PTR)0x1234560);
    SP("of not a window", (HWND)(INT_PTR)0x1234560, top);
    printf("parents %d %d\n", GetParent(child) == top, GetParent(grand) == child);
    printf("IsChild %d %d %d\n", IsChild(top, grand), IsChild(grand, top), IsChild(child, top));
    printf("ancestor %d\n", GetAncestor(grand, GA_ROOT) == top);
    SP("grandchild to other", grand, other);
    printf("moved %d %d %d\n", GetParent(grand) == other, IsChild(top, grand), IsChild(other, grand));
    SP("grandchild back", grand, child);
    POINT pt = { 0, 0 };
    ClientToScreen(grand, &pt);
    printf("origin %ld,%ld\n", pt.x, pt.y);
    DestroyWindow(top);
    printf("after destroy %d %d %d %d\n", IsWindow(top), IsWindow(child), IsWindow(grand), IsWindow(other));
    DestroyWindow(other);
}

static HANDLE made, go;
static HWND theirs, their_child;
static volatile LONG answered;
static LRESULT CALLBACK count_proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_USER + 1) { InterlockedIncrement(&answered); return 77; }
    return DefWindowProcA(h, m, w, l);
}

/* makes a window (and a child) and ends without DestroyWindow; `wait`: only after its window
 * got a message it never handles (it does not pump) */
static DWORD WINAPI maker(void *wait) {
    theirs = CreateWindowA("probe_count", "", WS_POPUP, 0, 0, 20, 20, NULL, NULL, GetModuleHandleA(NULL), NULL);
    their_child = CreateWindowA("probe_count", "", WS_CHILD, 0, 0, 5, 5, theirs, NULL, GetModuleHandleA(NULL), NULL);
    SetEvent(made);
    if (wait) {
        WaitForSingleObject(go, INFINITE);
        Sleep(200);
    }
    return 0;
}

static DWORD WINAPI sender(void *h) {
    LRESULT r = SendMessageA((HWND)h, WM_USER + 1, 0, 0);
    printf("send to an ending thread's window: %ld\n", (long)r);
    return 0;
}

static void thread_end(void) {
    WNDCLASSA c = { 0 };
    c.lpfnWndProc = count_proc;
    c.hInstance = GetModuleHandleA(NULL);
    c.lpszClassName = "probe_count";
    RegisterClassA(&c);
    made = CreateEventA(NULL, FALSE, FALSE, NULL);
    go = CreateEventA(NULL, FALSE, FALSE, NULL);

    HANDLE t = CreateThread(NULL, 0, maker, NULL, 0, NULL);
    WaitForSingleObject(made, INFINITE);
    WaitForSingleObject(t, INFINITE);
    CloseHandle(t);
    printf("ended thread's windows %d %d\n", IsWindow(theirs), IsWindow(their_child));
    SetLastError(0xdead);
    LRESULT r = SendMessageA(theirs, WM_USER + 1, 0, 0);
    printf("send after the end %ld err %lu\n", (long)r, GetLastError());
    SetLastError(0xdead);
    BOOL p = PostMessageA(theirs, WM_USER + 1, 0, 0);
    printf("post after the end %d err %lu\n", p, p ? 0 : GetLastError());

    /* a sender (another thread) waiting while the window's thread ends */
    t = CreateThread(NULL, 0, maker, (void *)1, 0, NULL);
    WaitForSingleObject(made, INFINITE);
    HANDLE s = CreateThread(NULL, 0, sender, theirs, 0, NULL);
    Sleep(50);
    SetEvent(go);
    DWORD w1 = WaitForSingleObject(t, 5000);
    DWORD w2 = WaitForSingleObject(s, 5000);
    printf("ended %s, sender %s, answered %ld, windows %d %d\n", w1 ? "no" : "yes", w2 ? "still waiting" : "returned", answered, IsWindow(theirs), IsWindow(their_child));
    CloseHandle(t);
    CloseHandle(s);
}

static void hidden(void) {
    static const struct { DWORD style, ex; int w, h; const char *what; } k[] = {
        { WS_POPUP, 0, 8000, 8000, "popup 8000x8000" },
        { WS_POPUP, 0, 100000, 100000, "popup 100000x100000" },
        { WS_OVERLAPPEDWINDOW, 0, CW_USEDEFAULT, CW_USEDEFAULT, "overlapped default" },
    };
    for (int i = 0; i < 3; i++) {
        HWND h = CreateWindowExA(k[i].ex, "probe_life", "", k[i].style, 0, 0, k[i].w, k[i].h, NULL, NULL, GetModuleHandleA(NULL), NULL);
        RECT r;
        GetClientRect(h, &r);
        printf("hidden %s: %s, visible %d, client >= 100 %d\n", k[i].what, h ? "ok" : "fails", IsWindowVisible(h), r.right >= 100 && r.bottom >= 100);
        DestroyWindow(h);
    }
    HWND m = CreateWindowExA(0, "probe_life", "", 0, 0, 0, 100, 100, HWND_MESSAGE, NULL, GetModuleHandleA(NULL), NULL);
    printf("message-only %s, send %ld\n", m ? "ok" : "fails", (long)SendMessageA(m, WM_USER + 1, 0, 0));
    /* drawn after the fact: a hidden window's DC works */
    HWND h = CreateWindowExA(0, "probe_life", "", WS_POPUP, 0, 0, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
    HDC dc = GetDC(h);
    printf("hidden dc %d\n", dc != NULL);
    ShowWindow(h, SW_SHOWNA);
    UpdateWindow(h);
    HBRUSH b = CreateSolidBrush(RGB(10, 200, 30));
    RECT rr = { 0, 0, 64, 64 };
    FillRect(dc, &rr, b);
    printf("shown pixel %06lx\n", (unsigned long)GetPixel(dc, 5, 5));
    DeleteObject(b);
    ReleaseDC(h, dc);
    DestroyWindow(h);
    DestroyWindow(m);
}

/* PostMessage to a window whose thread does not pump: Windows stops at 10000 posted
   messages (ERROR_NOT_ENOUGH_QUOTA); the queue is not grown without end */
static void quota(void) {
    HWND h = CreateWindowExA(0, "probe_life", "", WS_POPUP, 0, 0, 10, 10, NULL, NULL, GetModuleHandleA(NULL), NULL);
    int ok = 0;
    DWORD e = 0;
    for (int i = 0; i < 10050; i++) {
        SetLastError(0);
        if (PostMessageA(h, WM_USER + 7, i, 0)) {
            ok++;
        } else if (!e) {
            e = GetLastError();
        }
    }
    printf("posted %d of 10050, error %lu\n", ok, e);
    SetLastError(0);
    BOOL t = PostThreadMessageA(GetCurrentThreadId(), WM_USER + 8, 0, 0);
    printf("thread post when full %d, error %lu\n", t, GetLastError());
    MSG m;
    int got = 0;
    while (PeekMessageA(&m, NULL, WM_USER + 7, WM_USER + 8, PM_REMOVE)) {
        got++;
    }
    printf("taken %d; post again %d\n", got, PostMessageA(h, WM_USER + 7, 0, 0));
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) {
    }
    DestroyWindow(h);
}

/* DestroyMenu takes the popups under it; a popup on its own stays */
static void menus(void) {
    HMENU top = CreateMenu(), pop = CreatePopupMenu(), deep = CreatePopupMenu(), alone = CreatePopupMenu();
    AppendMenuA(deep, MF_STRING, 3, "c");
    AppendMenuA(pop, MF_POPUP, (UINT_PTR)deep, "b");
    AppendMenuA(top, MF_POPUP, (UINT_PTR)pop, "a");
    printf("menus %d %d %d %d\n", IsMenu(top), IsMenu(pop), IsMenu(deep), IsMenu(alone));
    DestroyMenu(top);
    printf("after DestroyMenu %d %d %d %d\n", IsMenu(top), IsMenu(pop), IsMenu(deep), IsMenu(alone));
    DestroyMenu(alone);
    /* a window's menu bar goes with the window */
    HMENU bar = CreateMenu(), sub = CreatePopupMenu();
    AppendMenuA(sub, MF_STRING, 4, "d");
    AppendMenuA(bar, MF_POPUP, (UINT_PTR)sub, "e");
    HWND h = CreateWindowExA(0, "probe_life", "", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, bar, GetModuleHandleA(NULL), NULL);
    DestroyWindow(h);
    printf("bar after DestroyWindow %d %d\n", IsMenu(bar), IsMenu(sub));
}

/* LoadCursor / LoadIcon of a system resource: the same shared handle each time */
static void shared(void) {
    HCURSOR a = LoadCursorA(NULL, (LPCSTR)IDC_ARROW), b = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    HICON i = LoadIconA(NULL, (LPCSTR)IDI_APPLICATION), j = LoadIconA(NULL, (LPCSTR)IDI_APPLICATION);
    HANDLE s1 = LoadImageA(NULL, (LPCSTR)OCR_NORMAL, IMAGE_CURSOR, 0, 0, LR_SHARED | LR_DEFAULTSIZE);
    printf("shared cursor %d icon %d image %d\n", a == b && a != NULL, i == j && i != NULL, s1 == a);
}

/* many windows made and destroyed: handles stay valid and distinct from live ones */
static void churn(void) {
    HWND keep = CreateWindowExA(0, "probe_life", "", WS_POPUP, 0, 0, 10, 10, NULL, NULL, GetModuleHandleA(NULL), NULL);
    int bad = 0;
    for (int i = 0; i < 3000; i++) {
        HWND h = CreateWindowExA(0, "probe_life", "", WS_POPUP, 0, 0, 10, 10, NULL, NULL, GetModuleHandleA(NULL), NULL);
        if (!h || h == keep) {
            bad++;
        }
        DestroyWindow(h);
    }
    printf("churn bad %d, kept %d\n", bad, IsWindow(keep));
    DestroyWindow(keep);
}

int main(void) {
    WNDCLASSA c = { 0 };
    c.lpfnWndProc = proc;
    c.hInstance = GetModuleHandleA(NULL);
    c.lpszClassName = "probe_life";
    RegisterClassA(&c);
    extras();
    parents();
    thread_end();
    hidden();
    quota();
    menus();
    shared();
    churn();
    return 0;
}
