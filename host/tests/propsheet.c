/* libs: -luser32 -lcomctl32 */
/* Property sheets and wizards (comctl32 PropertySheetA, CreatePropertySheetPageA,
 * DestroyPropertySheetPage): which pages hear which PSN_ notifications as a timer drives the
 * sheet with PSM_ messages, where the selection ends, what PropertySheet returns, and that the
 * pages are released when it returns. As EXEpress CX's installer runs its wizard.
 * (sake: propsheet.rs; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <commctrl.h>
#include <prsht.h>
#include <stdio.h>
#include <string.h>

static char log_[2048];
static int veto, skip1, created[8], released;
static HWND sheet;

static void add(const char *s) {
    if (strlen(log_) + strlen(s) + 2 < sizeof log_) {
        strcat(log_, " ");
        strcat(log_, s);
    }
}

static INT_PTR CALLBACK page_proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    char b[32];
    int i = (int)GetWindowLongPtrA(h, GWLP_USERDATA);
    if (msg == WM_INITDIALOG) {
        i = (int)((PROPSHEETPAGEA *)lp)->lParam;
        SetWindowLongPtrA(h, GWLP_USERDATA, i);
        created[i] = 1;
        sprintf(b, "%d:init", i);
        add(b);
        return TRUE;
    }
    if (msg == PSM_QUERYSIBLINGS) {
        sprintf(b, "%d:query", i);
        add(b);
        SetWindowLongPtrA(h, DWLP_MSGRESULT, i == 2 ? 40 + i : 0);
        return TRUE;
    }
    if (msg != WM_NOTIFY)
        return FALSE;
    NMHDR *n = (NMHDR *)lp;
    LONG_PTR r = 0;
    const char *what = NULL;
    switch (n->code) {
    case PSN_SETACTIVE:
        what = "set";
        if (i == 1 && skip1)
            r = -1;
        break;
    case PSN_KILLACTIVE:
        /* (not logged: when Windows sends it twice is not the point) */
        r = veto && i == 2;
        break;
    case PSN_APPLY: what = "apply"; break;
    case PSN_RESET: what = "reset"; break;
    case PSN_QUERYCANCEL: what = "querycancel"; break;
    case PSN_WIZNEXT: what = "next"; break;
    case PSN_WIZBACK: what = "back"; break;
    case PSN_WIZFINISH: what = "finish"; break;
    }
    if (what) {
        sprintf(b, "%d:%s", i, what);
        add(b);
    }
    SetWindowLongPtrA(h, DWLP_MSGRESULT, r);
    return TRUE;
}

static UINT CALLBACK page_cb(HWND h, UINT msg, LPPROPSHEETPAGEA p) {
    if (msg == PSPCB_RELEASE)
        released++;
    return 1;
}

static int cur(void) {
    HWND p = (HWND)SendMessageA(sheet, PSM_GETCURRENTPAGEHWND, 0, 0);
    return (int)SendMessageA(sheet, PSM_HWNDTOINDEX, (WPARAM)p, 0);
}

static int step, wizard;
static void CALLBACK tick(HWND h, UINT m, UINT_PTR id, DWORD t) {
    char b[48];
    if (!wizard) {
        switch (step++) {
        case 0: /* the start page is 1 */
            sprintf(b, "[cur %d]", cur());
            add(b);
            PropSheet_SetCurSel(sheet, NULL, 2);
            break;
        case 1:
            sprintf(b, "[cur %d]", cur());
            add(b);
            veto = 1; /* page 2 keeps the selection */
            PropSheet_SetCurSel(sheet, NULL, 0);
            break;
        case 2:
            sprintf(b, "[veto cur %d]", cur());
            add(b);
            veto = 0;
            sprintf(b, "[siblings %d]", (int)PropSheet_QuerySiblings(sheet, 5, 6));
            add(b);
            sprintf(b, "[tab %d]", TabCtrl_GetItemCount(PropSheet_GetTabControl(sheet)));
            add(b);
            PropSheet_Changed(sheet, (HWND)SendMessageA(sheet, PSM_GETCURRENTPAGEHWND, 0, 0));
            PropSheet_PressButton(sheet, PSBTN_OK);
            break;
        }
    } else {
        switch (step++) {
        case 0:
            sprintf(b, "[cur %d]", cur());
            add(b);
            skip1 = 1; /* page 1 skips itself */
            PropSheet_PressButton(sheet, PSBTN_NEXT);
            break;
        case 1:
            sprintf(b, "[cur %d]", cur());
            add(b);
            PropSheet_PressButton(sheet, PSBTN_BACK);
            break;
        case 2:
            sprintf(b, "[cur %d]", cur());
            add(b);
            skip1 = 0;
            PropSheet_PressButton(sheet, PSBTN_NEXT);
            break;
        case 3:
            sprintf(b, "[cur %d]", cur());
            add(b);
            PropSheet_SetWizButtons(sheet, PSWIZB_BACK | PSWIZB_FINISH);
            PropSheet_PressButton(sheet, PSBTN_FINISH);
            break;
        case 4:
            /* (a Cancel the third time round) */
            PropSheet_PressButton(sheet, PSBTN_CANCEL);
            break;
        }
    }
}

static int CALLBACK sheet_cb(HWND h, UINT msg, LPARAM lp) {
    if (msg == PSCB_INITIALIZED) {
        sheet = h;
        SetTimer(h, 1, 20, tick);
    }
    return 0;
}

/* a page's template: a child dialog 200 x 120 without controls */
static WORD tmpl[16] __attribute__((aligned(4)));

static INT_PTR run(int wiz, int start, int n, int cancel) {
    PROPSHEETPAGEA p[4];
    HPROPSHEETPAGE hp[4];
    static const char *titles[] = { "zero", "one", "two", "three" };
    memset(log_, 0, sizeof log_);
    memset(created, 0, sizeof created);
    released = 0;
    step = cancel ? 4 : 0;
    wizard = wiz;
    for (int i = 0; i < n; i++) {
        memset(&p[i], 0, sizeof p[i]);
        p[i].dwSize = sizeof p[i];
        p[i].dwFlags = PSP_DLGINDIRECT | PSP_USETITLE | PSP_USECALLBACK;
        p[i].hInstance = GetModuleHandleA(NULL);
        p[i].pResource = (LPCDLGTEMPLATE)tmpl;
        p[i].pszTitle = titles[i];
        p[i].pfnDlgProc = page_proc;
        p[i].pfnCallback = page_cb;
        p[i].lParam = i;
        hp[i] = CreatePropertySheetPageA(&p[i]);
    }
    PROPSHEETHEADERA h;
    memset(&h, 0, sizeof h);
    h.dwSize = sizeof h;
    h.dwFlags = PSH_USECALLBACK | PSH_NOAPPLYNOW | (wiz ? PSH_WIZARD : 0);
    h.hwndParent = NULL;
    h.hInstance = GetModuleHandleA(NULL);
    h.pszCaption = "sheet";
    h.nPages = n;
    h.nStartPage = start;
    h.phpage = hp;
    h.pfnCallback = sheet_cb;
    return PropertySheetA(&h);
}

int main(void) {
    INITCOMMONCONTROLSEX icc = { sizeof icc, ICC_TAB_CLASSES };
    InitCommonControlsEx(&icc);
    DLGTEMPLATE *t = (DLGTEMPLATE *)tmpl;
    t->style = WS_CHILD | WS_DISABLED | DS_3DLOOK;
    t->cx = 200;
    t->cy = 120;
    INT_PTR r = run(0, 1, 3, 0);
    printf("sheet:%s\n", log_);
    printf("sheet returns %d, pages released %d, created %d%d%d\n", (int)r, released, created[0], created[1], created[2]);
    r = run(1, 0, 4, 0);
    printf("wizard:%s\n", log_);
    printf("wizard returns %d, pages released %d\n", (int)(r > 0), released);
    r = run(1, 0, 2, 1);
    printf("cancel:%s\n", log_);
    printf("cancel returns %d, pages released %d\n", (int)r, released);
    return 0;
}
