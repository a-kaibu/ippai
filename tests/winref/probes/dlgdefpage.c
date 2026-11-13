/* DM_GETDEFID and Enter in a wizard-like dialog: the outer dialog holds a page dialog
 * (DS_CONTROL, WS_EX_CONTROLPARENT) whose buttons include the BS_DEFPUSHBUTTON (姦淫特急『松葉』
 * 体験版's installer). What DefDlgProc answers for the outer dialog with the page visible,
 * hidden, disabled and without WS_EX_CONTROLPARENT, and which WM_COMMAND the outer dialog gets
 * for Enter through the modal loop.
 * libs: -luser32 */
#include "../probe.h"

static const char *what;
static int variant, got_id, got_lp, step;
static HWND page;

/* a template: one dialog, optionally with one push button (id, style) */
static WORD *tmpl(WORD *p, DWORD style, DWORD ex, int x, int y, int cx, int cy, int nbut, const DWORD *bstyle, const WORD *bid) {
    DLGTEMPLATE *t = (DLGTEMPLATE *)p;
    t->style = style; t->dwExtendedStyle = ex; t->cdit = nbut; t->x = x; t->y = y; t->cx = cx; t->cy = cy;
    p = (WORD *)(t + 1);
    *p++ = 0; *p++ = 0; *p++ = 0; /* menu, class, title */
    for (int i = 0; i < nbut; i++) {
        p = (WORD *)(((ULONG_PTR)p + 3) & ~3);
        DLGITEMTEMPLATE *it = (DLGITEMTEMPLATE *)p;
        it->style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | bstyle[i]; it->dwExtendedStyle = 0;
        it->x = 10 + 50 * i; it->y = 10; it->cx = 40; it->cy = 14; it->id = bid[i];
        p = (WORD *)(it + 1);
        *p++ = 0xffff; *p++ = 0x0080; /* Button */
        *p++ = 'b'; *p++ = 0; *p++ = 0; /* title, no creation data */
    }
    return p;
}

static INT_PTR CALLBACK page_proc(HWND h, UINT m, WPARAM wp, LPARAM lp) {
    if (m == WM_INITDIALOG) return FALSE;
    return FALSE;
}

static INT_PTR CALLBACK outer_proc(HWND h, UINT m, WPARAM wp, LPARAM lp) {
    static WORD buf[512];
    switch (m) {
    case WM_INITDIALOG: {
        /* the page: buttons 0x3ef (push), 0x3f0 (default push), 0x3f1 (push) */
        DWORD st[3] = { BS_PUSHBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON };
        WORD id[3] = { 0x3ef, 0x3f0, 0x3f1 };
        DWORD style = WS_CHILD | DS_CONTROL | (variant == 1 ? 0 : WS_VISIBLE) | (variant == 2 ? WS_DISABLED : 0);
        DWORD ex = variant == 3 ? 0 : WS_EX_CONTROLPARENT;
        tmpl(buf, style, ex, 0, 0, 200, 60, 3, st, id);
        page = CreateDialogIndirectParamA(GetModuleHandleA(NULL), (DLGTEMPLATE *)buf, h, page_proc, 0);
        LRESULT r = SendMessageA(h, DM_GETDEFID, 0, 0);
        printf("%s: DM_GETDEFID %#lx\n", what, (unsigned long)r);
        HWND f = GetFocus();
        printf("%s: focus %s\n", what, f == h ? "dialog" : f == NULL ? "none" : GetParent(f) == page ? "page control" : "other");
        PostMessageA(h, WM_APP, 0, 0);
        return TRUE;
    }
    case WM_APP: {
        /* Enter, to whatever has the focus, through the modal loop's IsDialogMessage */
        HWND f = GetFocus();
        if (!f) f = h;
        PostMessageA(f, WM_KEYDOWN, VK_RETURN, 0x001c0001);
        PostMessageA(f, WM_KEYUP, VK_RETURN, 0xc01c0001);
        PostMessageA(h, WM_APP + 1, 0, 0);
        return TRUE;
    }
    case WM_APP + 1:
        if (++step < 3) { PostMessageA(h, WM_APP + 1, 0, 0); return TRUE; }
        EndDialog(h, 0);
        return TRUE;
    case WM_COMMAND:
        if (!got_id) {
            got_id = LOWORD(wp) | HIWORD(wp) << 16;
            got_lp = lp == 0 ? 0 : (HWND)lp == page ? 2 : GetParent((HWND)lp) == page ? 1 : 9;
        }
        return TRUE;
    }
    return FALSE;
}

int main(void) {
    static const char *names[] = { "visible page", "hidden page", "disabled page", "no WS_EX_CONTROLPARENT" };
    for (variant = 0; variant < 4; variant++) {
        WORD buf[64];
        what = names[variant];
        got_id = got_lp = step = 0;
        tmpl(buf, WS_POPUP | WS_CAPTION | DS_MODALFRAME, 0, 0, 0, 220, 100, 0, NULL, NULL);
        DialogBoxIndirectParamA(GetModuleHandleA(NULL), (DLGTEMPLATE *)buf, NULL, outer_proc, 0);
        /* (lparam: 0 none, 1 a page button, 2 the page, 9 something else) */
        printf("%s: Enter WM_COMMAND %#x lparam %d\n", what, got_id, got_lp);
    }
    return 0;
}
