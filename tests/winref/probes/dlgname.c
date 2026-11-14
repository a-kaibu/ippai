/* What a dialog control's window procedure gets as its name: the dialog manager creates each
 * item with CreateWindowExW and the template's title as lpWindowName, and a class registered
 * with RegisterClassA sees it in ANSI in WM_NCCREATE / WM_CREATE (InstallShield 12's setup
 * launcher, マスクド上海: its is_gif_class control loads the GIF resource named by its title,
 * FindResourceA(inst, cs->lpszName, "GIF")). Also a W class made by CreateWindowExA, and
 * GetWindowText of each afterwards.
 * libs: -luser32 */
#include "../probe.h"

static void show_cs(const char *who, UINT msg, CREATESTRUCTA *cs, BOOL wide) {
    if (wide) {
        const WCHAR *n = (const WCHAR *)cs->lpszName;
        printf("%s %s name", who, msg == WM_NCCREATE ? "WM_NCCREATE" : "WM_CREATE");
        if (!n) printf(" NULL");
        else if ((UINT_PTR)n < 0x10000) printf(" #%u", (unsigned)(UINT_PTR)n);
        else for (int i = 0; i < 12 && n[i]; i++) printf(" %04x", n[i]);
    } else {
        const unsigned char *n = (const unsigned char *)cs->lpszName;
        printf("%s %s name", who, msg == WM_NCCREATE ? "WM_NCCREATE" : "WM_CREATE");
        if (!n) printf(" NULL");
        else if ((UINT_PTR)n < 0x10000) printf(" #%u", (unsigned)(UINT_PTR)n);
        else for (int i = 0; i < 12 && n[i]; i++) printf(" %02x", n[i]);
    }
    printf("\n");
}

static LRESULT CALLBACK proc_a(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_NCCREATE || m == WM_CREATE) show_cs("A class", m, (CREATESTRUCTA *)l, FALSE);
    return DefWindowProcA(h, m, w, l);
}
static LRESULT CALLBACK proc_w(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_NCCREATE || m == WM_CREATE) show_cs("W class", m, (CREATESTRUCTA *)l, TRUE);
    return DefWindowProcW(h, m, w, l);
}

static INT_PTR CALLBACK dlg(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_INITDIALOG) {
        char t[64];
        GetWindowTextA(GetDlgItem(h, 100), t, sizeof t);
        printf("item 100 text [%s]\n", t);
        GetWindowTextA(GetDlgItem(h, 101), t, sizeof t);
        printf("item 101 text [%s]\n", t);
        EndDialog(h, 7);
        return TRUE;
    }
    return FALSE;
}

static WORD *wstr(WORD *p, const char *s) {
    while ((*p++ = (WORD)(unsigned char)*s++)) {}
    return p;
}
static WORD *align4(WORD *p) {
    return (WORD *)(((UINT_PTR)p + 3) & ~(UINT_PTR)3);
}

int main(void) {
    HINSTANCE inst = GetModuleHandleA(NULL);
    WNDCLASSA ca = { 0, proc_a, 0, 0, inst, 0, 0, 0, 0, "probe_a_class" };
    WNDCLASSW cw = { 0, proc_w, 0, 0, inst, 0, 0, 0, 0, L"probe_w_class" };
    printf("RegisterClassA %d\n", RegisterClassA(&ca) != 0);
    printf("RegisterClassW %d\n", RegisterClassW(&cw) != 0);
    /* a dialog with two items: probe_a_class "IDR_GIF1" (100) and probe_a_class with an ordinal
     * title (101) */
    static WORD buf[512];
    WORD *p = buf;
    DLGTEMPLATE *t = (DLGTEMPLATE *)p;
    t->style = WS_POPUP | DS_MODALFRAME;
    t->dwExtendedStyle = 0;
    t->cdit = 2;
    t->x = 0; t->y = 0; t->cx = 100; t->cy = 50;
    p = (WORD *)(t + 1);
    *p++ = 0; *p++ = 0; *p++ = 0; /* menu, class, title */
    for (int k = 0; k < 2; k++) {
        p = align4(p);
        DLGITEMTEMPLATE *it = (DLGITEMTEMPLATE *)p;
        it->style = WS_CHILD | WS_VISIBLE;
        it->dwExtendedStyle = 0;
        it->x = 5; it->y = 5 + 20 * k; it->cx = 40; it->cy = 10;
        it->id = (WORD)(100 + k);
        p = (WORD *)(it + 1);
        p = wstr(p, "probe_a_class");
        if (k == 0) p = wstr(p, "IDR_GIF1");
        else { *p++ = 0xffff; *p++ = 5; }
        *p++ = 0;
    }
    INT_PTR r = DialogBoxIndirectParamA(inst, (DLGTEMPLATE *)buf, NULL, dlg, 0);
    printf("DialogBoxIndirectParam = %d\n", (int)r);
    HWND w = CreateWindowExA(0, "probe_w_class", "abc", WS_POPUP, 0, 0, 10, 10, NULL, NULL, inst, NULL);
    DestroyWindow(w);
    w = CreateWindowExW(0, L"probe_a_class", L"xyz", WS_POPUP, 0, 0, 10, 10, NULL, NULL, inst, NULL);
    DestroyWindow(w);
    w = CreateWindowExA(0, "probe_a_class", NULL, WS_POPUP, 0, 0, 10, 10, NULL, NULL, inst, NULL);
    DestroyWindow(w);
    return 0;
}
