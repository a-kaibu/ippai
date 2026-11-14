/* Subclassing in a 16-bit program: an EDIT control's (a 32-bit procedure) and one of its own
 * windows', through SetWindowLong / GetWindowLong(GWL_WNDPROC) and SetClassLong /
 * GetClassLong(GCL_WNDPROC); the old procedures called through CallWindowProc and called
 * directly, as programs do, with messages whose lParam points at data. (#37)
 * libs: */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static WNDPROC old_edit, old_own;
static int seen;

LONG FAR PASCAL __export EditSub(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_SETTEXT) {
        seen++;
        printf("EditSub WM_SETTEXT \"%s\"\n", (LPSTR)lp);
    }
    return CallWindowProc((FARPROC)old_edit, w, m, wp, lp);
}

LONG FAR PASCAL __export OwnProc(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_USER) return 1000 + wp;
    return DefWindowProc(w, m, wp, lp);
}

LONG FAR PASCAL __export OwnSub(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_USER) return 2000 + CallWindowProc((FARPROC)old_own, w, m, wp, lp);
    return CallWindowProc((FARPROC)old_own, w, m, wp, lp);
}

LONG FAR PASCAL __export ClassProc(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_USER) return 3000 + wp;
    return DefWindowProc(w, m, wp, lp);
}

int PASCAL WinMain(HINSTANCE hi, HINSTANCE hp, LPSTR cmd, int sh) {
    WNDCLASS wc;
    HWND top, edit, own, own2;
    char buf[32];
    LONG r;
    WNDPROC p;
    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = OwnProc;
    wc.hInstance = hi;
    wc.lpszClassName = "own16";
    RegisterClass(&wc);
    top = CreateWindow("own16", "top", WS_OVERLAPPEDWINDOW, 0, 0, 200, 100, 0, 0, hi, 0);
    edit = CreateWindow("EDIT", "", WS_CHILD, 0, 0, 100, 20, top, (HMENU)1, hi, 0);

    old_edit = (WNDPROC)SetWindowLong(edit, GWL_WNDPROC, (LONG)EditSub);
    printf("subclassed edit %d\n", old_edit != 0);
    printf("GetWindowLong is ours %d\n", (WNDPROC)GetWindowLong(edit, GWL_WNDPROC) == EditSub);
    SendMessage(edit, WM_SETTEXT, 0, (LPARAM)(LPSTR)"hello");
    GetWindowText(edit, buf, sizeof buf);
    printf("text \"%s\" seen %d\n", buf, seen);
    printf("old called directly: length %ld\n", old_edit(edit, WM_GETTEXTLENGTH, 0, 0));
    memset(buf, 0, sizeof buf);
    r = old_edit(edit, WM_GETTEXT, sizeof buf, (LPARAM)(LPSTR)buf);
    printf("old called directly: WM_GETTEXT %ld \"%s\"\n", r, buf);
    r = old_edit(edit, WM_SETTEXT, 0, (LPARAM)(LPSTR)"direct");
    GetWindowText(edit, buf, sizeof buf);
    printf("old called directly: WM_SETTEXT %ld \"%s\" seen %d\n", r, buf, seen);
    p = (WNDPROC)GetClassLong(edit, GCL_WNDPROC);
    printf("class procedure: length %ld\n", p(edit, WM_GETTEXTLENGTH, 0, 0));
    p = (WNDPROC)SetWindowLong(edit, GWL_WNDPROC, (LONG)old_edit);
    printf("restored: returned ours %d, now the old %d\n", p == EditSub, (WNDPROC)GetWindowLong(edit, GWL_WNDPROC) == old_edit);

    own = CreateWindow("own16", "own", WS_CHILD, 0, 0, 10, 10, top, (HMENU)2, hi, 0);
    old_own = (WNDPROC)SetWindowLong(own, GWL_WNDPROC, (LONG)OwnSub);
    printf("own old is OwnProc %d\n", old_own == OwnProc);
    printf("own WM_USER %ld\n", SendMessage(own, WM_USER, 7, 0));
    printf("own old directly %ld\n", old_own(own, WM_USER, 8, 0));

    p = (WNDPROC)SetClassLong(own, GCL_WNDPROC, (LONG)ClassProc);
    printf("SetClassLong returned OwnProc %d, GetClassLong ClassProc %d\n", p == OwnProc, (WNDPROC)GetClassLong(own, GCL_WNDPROC) == ClassProc);
    own2 = CreateWindow("own16", "own2", WS_CHILD, 0, 0, 10, 10, top, (HMENU)3, hi, 0);
    printf("new window WM_USER %ld, subclassed one %ld\n", SendMessage(own2, WM_USER, 9, 0), SendMessage(own, WM_USER, 7, 0));
    DestroyWindow(top);
    return 0;
}
