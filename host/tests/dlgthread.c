/* libs: -luser32 */
/* A modal dialog ended by a message another thread sends it: the dialog procedure calls
 * EndDialog while handling a WM_COMMAND that came through SendMessage from a second thread
 * (as msi's dialogs, or a game's launcher, driven by a helper thread), so DialogBox has to
 * return although no message is ever posted to its own queue. Also a timer's WM_TIMER and
 * a posted WM_COMMAND ending it. (sake: user.rs dialog_box; compared with Windows by
 * tests/winref)
 */
#include <windows.h>
#include <stdio.h>

static HWND dlg;
static int how;

static INT_PTR CALLBACK proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_INITDIALOG:
        dlg = h;
        if (how == 2)
            SetTimer(h, 1, 10, NULL);
        if (how == 3)
            PostMessageA(h, WM_COMMAND, 7, 0);
        return TRUE;
    case WM_TIMER:
        KillTimer(h, 1);
        EndDialog(h, 33);
        return TRUE;
    case WM_COMMAND:
        EndDialog(h, LOWORD(wp) * 6);
        return TRUE;
    }
    return FALSE;
}

static DWORD WINAPI other(void *arg) {
    while (!dlg)
        Sleep(1);
    LRESULT r = SendMessageA(dlg, WM_COMMAND, 7, 0);
    printf("  sent: %ld\n", (long)r);
    return 0;
}

int main(void) {
    /* (a dialog with no controls: DLGTEMPLATE, no menu, class or title) */
    static WORD t[16] __attribute__((aligned(4)));
    DLGTEMPLATE *d = (DLGTEMPLATE *)t;
    d->style = WS_POPUP | WS_CAPTION | WS_VISIBLE;
    d->cx = 100;
    d->cy = 50;
    for (how = 1; how <= 3; how++) {
        dlg = NULL;
        HANDLE th = how == 1 ? CreateThread(NULL, 0, other, NULL, 0, NULL) : NULL;
        INT_PTR r = DialogBoxIndirectParamA(GetModuleHandleA(NULL), d, NULL, proc, 0);
        if (th) {
            WaitForSingleObject(th, 5000);
            CloseHandle(th);
        }
        printf("%s: %ld, window %d\n", how == 1 ? "other thread" : how == 2 ? "timer" : "posted", (long)r, IsWindow(dlg));
    }
    return 0;
}
