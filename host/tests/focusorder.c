/* SetFocus on a window whose top-level window is not active activates that window first:
 * WM_ACTIVATE (active) comes before WM_SETFOCUS, and ShowWindow afterwards sends no second
 * activation. Unity 6 sets the focus on its still hidden window, then shows it; sake sent
 * WM_SETFOCUS alone and the activation only at ShowWindow, and Unity's input stayed off as
 * for an unfocused player (化けきつねたいじ, onihaz). Also: a disabled window takes no focus,
 * SetFocus on a child activates its top-level window, and an invalid window fails with
 * ERROR_INVALID_WINDOW_HANDLE. (sake: user.rs SetFocus) */
#include <stdio.h>
#include <windows.h>

static char log_[512];
static int n;
static HWND w1, w2;

static void note(const char *s)
{
    if (n + 8 < (int)sizeof(log_))
        n += sprintf(log_ + n, "%s ", s);
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM wp, LPARAM lp)
{
    const char *who = h == w1 ? "1" : h == w2 ? "2" : "c";
    char b[32];
    if (m == WM_ACTIVATE)
    {
        sprintf(b, "act%s:%d", who, LOWORD(wp) != WA_INACTIVE);
        note(b);
    }
    else if (m == WM_SETFOCUS)
    {
        sprintf(b, "set%s", who);
        note(b);
    }
    else if (m == WM_KILLFOCUS)
    {
        sprintf(b, "kill%s", who);
        note(b);
    }
    return DefWindowProcA(h, m, wp, lp);
}

static void show(const char *what)
{
    printf("%s: %s\n", what, n ? log_ : "(none)");
    n = 0;
    log_[0] = 0;
}

int main(void)
{
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "focusorder";
    RegisterClassA(&wc);
    w1 = CreateWindowExA(0, "focusorder", "one", WS_OVERLAPPEDWINDOW, 10, 10, 200, 150, NULL, NULL, wc.hInstance, NULL);
    w2 = CreateWindowExA(0, "focusorder", "two", WS_OVERLAPPEDWINDOW, 40, 40, 200, 150, NULL, NULL, wc.hInstance, NULL);
    HWND child = CreateWindowExA(0, "focusorder", "c", WS_CHILD | WS_VISIBLE, 0, 0, 50, 50, w2, NULL, wc.hInstance, NULL);
    show("created");

    HWND prev = SetFocus(w1);
    show("SetFocus(hidden one)");
    printf("  previous none %d, active one %d, focus one %d\n", prev == NULL, GetActiveWindow() == w1, GetFocus() == w1);
    ShowWindow(w1, SW_SHOW);
    show("ShowWindow(one)");
    printf("  active one %d, focus one %d\n", GetActiveWindow() == w1, GetFocus() == w1);

    prev = SetFocus(w1);
    show("SetFocus(one) again");
    printf("  returns one %d\n", prev == w1);

    prev = SetFocus(child);
    show("SetFocus(child of two)");
    printf("  previous one %d, active two %d, focus child %d\n", prev == w1, GetActiveWindow() == w2, GetFocus() == child);

    EnableWindow(w1, FALSE);
    show("EnableWindow(one, FALSE)");
    SetLastError(0xdead);
    prev = SetFocus(w1);
    show("SetFocus(disabled one)");
    printf("  returns NULL %d, focus still child %d, active still two %d\n", prev == NULL, GetFocus() == child, GetActiveWindow() == w2);

    SetLastError(0xdead);
    prev = SetFocus((HWND)(ULONG_PTR)0x12345678);
    printf("SetFocus(invalid): NULL %d, error %lu\n", prev == NULL, GetLastError());
    show("  messages");
    return 0;
}
