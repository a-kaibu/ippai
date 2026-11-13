/* Picture statics (SS_BITMAP) in a dialog template whose text is a bitmap's number: the static
 * loads that bitmap from the dialog's module (STM_GETIMAGE gives it) and takes its size unless
 * SS_CENTERIMAGE or SS_REALSIZECONTROL; STM_SETIMAGE gives back the old picture, takes only
 * IMAGE_BITMAP, and resizes the same way. Bishop's 満淫電車2 体験版 startup.exe shows its
 * banner this way: sake left the static empty (a grey launcher).
 * libs: -lgdi32 */
#include "../probe.h"

static INT_PTR CALLBACK dlgproc(HWND h, UINT m, WPARAM w, LPARAM l) { return m == WM_INITDIALOG; }

static void show(HWND dlg, int id, RECT dlu) {
    HWND c = GetDlgItem(dlg, id);
    RECT r;
    GetClientRect(c, &r);
    MapDialogRect(dlg, &dlu);
    int w = r.right, h = r.bottom;
    printf("%d: image %d size %s\n", id, SendMessageA(c, STM_GETIMAGE, IMAGE_BITMAP, 0) != 0,
           w == dlu.right - dlu.left && h == dlu.bottom - dlu.top ? "template" : "");
    if (!(w == dlu.right - dlu.left && h == dlu.bottom - dlu.top)) printf("%d: %dx%d\n", id, w, h);
}

static void size(const char *what, HWND c) {
    RECT r;
    GetClientRect(c, &r);
    printf("%s: %dx%d\n", what, (int)r.right, (int)r.bottom);
}

int main(void) {
    HINSTANCE inst = GetModuleHandleA(NULL);
    HWND dlg = CreateDialogParamA(inst, MAKEINTRESOURCEA(100), NULL, dlgproc, 0);
    printf("dialog %d\n", dlg != NULL);
    if (!dlg) return 1;
    RECT t[] = { { 4, 4, 44, 44 }, { 50, 4, 90, 44 }, { 100, 4, 140, 44 }, { 150, 4, 190, 44 }, { 4, 60, 44, 100 }, { 50, 60, 90, 100 } };
    for (int i = 0; i < 6; i++) show(dlg, 1001 + i, t[i]);
    HWND a = GetDlgItem(dlg, 1001);
    HBITMAP res = (HBITMAP)SendMessageA(a, STM_GETIMAGE, IMAGE_BITMAP, 0);
    BITMAP bm = { 0 };
    GetObjectA(res, sizeof bm, &bm);
    printf("loaded: %dx%d\n", (int)bm.bmWidth, (int)bm.bmHeight);
    SetLastError(0);
    HBITMAP missing = LoadBitmapA(inst, MAKEINTRESOURCEA(77));
    printf("LoadBitmap missing: %d error %d\n", missing != NULL, (int)GetLastError());
    /* the other kinds are refused and change nothing */
    HICON ic = LoadIconA(NULL, (LPCSTR)IDI_APPLICATION);
    printf("set icon: %d, image same %d\n", SendMessageA(a, STM_SETIMAGE, IMAGE_ICON, (LPARAM)ic) != 0,
           (HBITMAP)SendMessageA(a, STM_GETIMAGE, IMAGE_BITMAP, 0) == res);
    printf("get as icon: %d\n", SendMessageA(a, STM_GETIMAGE, IMAGE_ICON, 0) == (LRESULT)res);
    /* a new bitmap: the old one back, the new size */
    HDC sc = GetDC(NULL);
    HBITMAP b2 = CreateCompatibleBitmap(sc, 20, 10);
    ReleaseDC(NULL, sc);
    printf("set: old back %d\n", SendMessageA(a, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)b2) == (LRESULT)res);
    size("after set", a);
    printf("set NULL: old back %d\n", SendMessageA(a, STM_SETIMAGE, IMAGE_BITMAP, 0) == (LRESULT)b2);
    size("after NULL", a);
    printf("get after NULL: %d\n", SendMessageA(a, STM_GETIMAGE, IMAGE_BITMAP, 0) != 0);
    /* the empty-text one, given a bitmap */
    HWND e = GetDlgItem(dlg, 1005);
    printf("empty set: old %d\n", SendMessageA(e, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)res) != 0);
    size("empty after set", e);
    /* centred / real-size-control: never resized */
    HWND cc = GetDlgItem(dlg, 1004), rc = GetDlgItem(dlg, 1003);
    RECT r1, r2;
    GetClientRect(cc, &r1);
    GetClientRect(rc, &r2);
    SendMessageA(cc, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)b2);
    SendMessageA(rc, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)b2);
    RECT r3, r4;
    GetClientRect(cc, &r3);
    GetClientRect(rc, &r4);
    printf("centred kept %d, real-size-control kept %d\n", EqualRect(&r1, &r3), EqualRect(&r2, &r4));
    /* a static made by CreateWindow */
    HWND n = CreateWindowExA(0, "Static", NULL, SS_BITMAP | WS_CHILD, 0, 0, 50, 50, dlg, NULL, inst, NULL);
    printf("created: image %d\n", SendMessageA(n, STM_GETIMAGE, IMAGE_BITMAP, 0) != 0);
    SendMessageA(n, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)b2);
    size("created after set", n);
    /* a text static ignores STM_SETIMAGE */
    HWND tx = CreateWindowExA(0, "Static", "text", SS_LEFT | WS_CHILD, 0, 0, 50, 50, dlg, NULL, inst, NULL);
    printf("text static set: %d get %d\n", (int)SendMessageA(tx, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)b2), SendMessageA(tx, STM_GETIMAGE, IMAGE_BITMAP, 0) != 0);
    size("text after set", tx);
    DestroyWindow(dlg);
    return 0;
}
