/* msi.dll's own dialogs (libs: -lmsi -luser32): msiui.msi is msiinstall.msi with Dialog,
 * Control, ControlEvent and EventMapping tables added by tests/host-tests.sh, and an
 * InstallUISequence showing WelcomeDlg (an edit for USERNAME, a check box for WANTEXTRAS,
 * Next / Cancel), the modeless ProgressDlg (ActionText and a progress bar subscribed to
 * the install's messages) and ExitDlg ("Thanks, [USERNAME]"). At INSTALLUILEVEL_FULL a
 * second thread works the dialogs as a user would: it lists the controls, types a name,
 * ticks the box and presses Next (the Extras feature is added, the name shows on the last
 * dialog), after a first run in which it presses Cancel (1602, nothing installed). An external UI
 * handler reads the progress dialog's text while the install runs. */
#include <windows.h>
#include <msi.h>
#include <stdio.h>
#include <string.h>

static const char *CODE = "{6A0F2E9B-5C1D-4E7A-9B3C-1D2E3F405162}";
static char pkg[MAX_PATH];

struct find {
    const char *text;
    HWND dlg, ctl;
};

static BOOL CALLBACK child(HWND h, LPARAM l) {
    struct find *f = (struct find *)l;
    char t[256] = "";
    GetWindowTextA(h, t, sizeof t);
    if (!strncmp(t, f->text, strlen(f->text))) {
        f->ctl = h;
        return FALSE;
    }
    return TRUE;
}

static BOOL CALLBACK top(HWND h, LPARAM l) {
    struct find *f = (struct find *)l;
    if (!IsWindowVisible(h))
        return TRUE;
    f->ctl = NULL;
    EnumChildWindows(h, child, l);
    if (f->ctl) {
        f->dlg = h;
        return FALSE;
    }
    return TRUE;
}

/* The dialog showing a control whose text starts with text, and that control. */
static HWND find(const char *text, HWND *ctl) {
    struct find f = {text, NULL, NULL};
    EnumWindows(top, (LPARAM)&f);
    if (ctl)
        *ctl = f.ctl;
    return f.dlg;
}

static HWND wait_for(const char *text, HWND *ctl) {
    for (int i = 0; i < 3000; i++) {
        HWND d = find(text, ctl);
        if (d)
            return d;
        Sleep(10);
    }
    printf("  no dialog with \"%s\"\n", text);
    return NULL;
}

static BOOL CALLBACK list(HWND h, LPARAM l) {
    char cls[64] = "", t[256] = "";
    GetClassNameA(h, cls, sizeof cls);
    GetWindowTextA(h, t, sizeof t);
    RECT r;
    GetWindowRect(h, &r);
    MapWindowPoints(NULL, (HWND)l, (POINT *)&r, 2);
    LONG st = GetWindowLongA(h, GWL_STYLE);
    printf("    %s \"%s\" %ld,%ld %ldx%ld%s\n", cls, t, r.left, r.top, r.right - r.left, r.bottom - r.top, (st & WS_VISIBLE) ? "" : " hidden");
    return TRUE;
}

static void show(HWND d) {
    char t[256] = "";
    GetWindowTextA(d, t, sizeof t);
    RECT r;
    GetClientRect(d, &r);
    printf("  dialog \"%s\" %ldx%ld\n", t, r.right, r.bottom);
    EnumChildWindows(d, list, (LPARAM)d);
}

static void click(HWND d, HWND b) {
    SendMessageA(d, WM_COMMAND, MAKEWPARAM(GetDlgCtrlID(b), BN_CLICKED), (LPARAM)b);
}

/* The user: 0 fills in WelcomeDlg and finishes, 1 cancels. */
static DWORD WINAPI user(void *arg) {
    HWND ctl, d = wait_for("Install extras", &ctl);
    if (!d)
        return 0;
    show(d);
    if (arg) {
        find("Cancel", &ctl);
        click(d, ctl);
        return 0;
    }
    HWND box = ctl;
    find("nobody", &ctl);
    SetWindowTextA(ctl, "Kota");
    SendMessageA(box, BM_CLICK, 0, 0);
    printf("  check box: %ld\n", (long)SendMessageA(box, BM_GETCHECK, 0, 0));
    find("&Next", &ctl);
    click(d, ctl);
    d = wait_for("Thanks", &ctl);
    if (!d)
        return 0;
    show(d);
    find("&Finish", &ctl);
    click(d, ctl);
    return 0;
}

static int seen;

static INT CALLBACK handler(void *ctx, UINT kind, LPCSTR msg) {
    HWND ctl;
    if (!seen && find("Copying new files", &ctl)) {
        seen = 1;
        printf("  progress dialog: \"Copying new files\"\n");
    }
    return 0;
}

static void installed(void) {
    printf("  state %d\n", MsiQueryProductStateA(CODE));
    char p[MAX_PATH];
    DWORD n = sizeof p;
    INSTALLSTATE s = MsiGetComponentPathA(CODE, "{33333333-4444-5555-6666-777777777777}", p, &n);
    printf("  extras component: %d\n", s);
}

int main(void) {
    GetModuleFileNameA(NULL, pkg, sizeof pkg);
    strcpy(strrchr(pkg, '\\') + 1, "msiui.msi");
    MsiSetExternalUIA(handler, INSTALLLOGMODE_ACTIONSTART, NULL);
    for (int run = 0; run < 2; run++) {
        printf("== %s\n", run ? "install" : "cancel");
        MsiSetInternalUI(INSTALLUILEVEL_FULL, NULL);
        HANDLE t = CreateThread(NULL, 0, user, (void *)(INT_PTR)!run, 0, NULL);
        UINT r = MsiInstallProductA(pkg, "");
        WaitForSingleObject(t, 30000);
        CloseHandle(t);
        printf("  result %u\n", r);
        installed();
    }
    MsiSetInternalUI(INSTALLUILEVEL_NONE, NULL);
    printf("uninstall: %u\n", MsiInstallProductA(pkg, "REMOVE=ALL"));
    installed();
    return 0;
}
