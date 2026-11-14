/* An installer's pages as SAKE_AUTO (crates/sake/src/autopilot.rs) goes through them: run with
 * SAKE_AUTO=1 (tests/host-tests.sh), nobody at the keyboard. Page 1 is a licence (同意しない
 * checked; 次へ greyed out until 同意する is), page 2 has 「インストール後にゲームを起動する」
 * checked, 「インストール先の変更」 and 「インストール」; then the files go to C:\autoinstall and
 * the setup asks whether to start the game (No) and to go on (OK). Each page reports what was
 * pressed. The installed files stay (tests/corpus/install_test.py runs it as a trial's setup).
 * (libs: -luser32 -lgdi32) */
#include <windows.h>
#include <stdio.h>
#include <string.h>

enum { ID_BACK = 10, ID_NO = 11, ID_YES = 12, ID_LAUNCH = 20, ID_FOLDER = 21 };

/* a DLGTEMPLATE in memory (one build for x86 and x64: no resources) */
static WORD tmpl[1024];
static int at;
static void w(WORD v) { tmpl[at++] = v; }
static void str(const WCHAR *s) { do w(*s); while (*s++); }
static void align4(void) { if (at & 1) w(0); }
static void dialog(int n, const WCHAR *title) {
    at = 0;
    DWORD style = DS_MODALFRAME | DS_SETFONT | WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE;
    w(LOWORD(style)); w(HIWORD(style)); w(0); w(0); /* exstyle */
    w(n); w(10); w(10); w(240); w(140);
    w(0); w(0); /* no menu, default class */
    str(title);
    w(9); str(L"MS UI Gothic");
}
static void item(DWORD style, int x, int y, int cx, int cy, int id, WORD cls, const WCHAR *text) {
    align4();
    style |= WS_CHILD | WS_VISIBLE;
    w(LOWORD(style)); w(HIWORD(style)); w(0); w(0);
    w(x); w(y); w(cx); w(cy); w(id);
    w(0xffff); w(cls);
    str(text);
    w(0);
}

static int pressed_back, folder_changes, launch_left;

static INT_PTR CALLBACK licence(HWND h, UINT m, WPARAM wp, LPARAM lp) {
    (void)lp;
    if (m == WM_INITDIALOG) {
        CheckRadioButton(h, ID_NO, ID_YES, ID_NO);
        EnableWindow(GetDlgItem(h, IDOK), FALSE);
        return TRUE;
    }
    if (m == WM_COMMAND && HIWORD(wp) == BN_CLICKED) {
        switch (LOWORD(wp)) {
        case ID_NO: case ID_YES:
            EnableWindow(GetDlgItem(h, IDOK), IsDlgButtonChecked(h, ID_YES) == BST_CHECKED);
            return TRUE;
        case IDOK: EndDialog(h, IsDlgButtonChecked(h, ID_YES) == BST_CHECKED ? 1 : 9); return TRUE;
        case ID_BACK: pressed_back++; return TRUE;
        case IDCANCEL: EndDialog(h, 2); return TRUE;
        }
    }
    return FALSE;
}

static INT_PTR CALLBACK options(HWND h, UINT m, WPARAM wp, LPARAM lp) {
    (void)lp;
    if (m == WM_INITDIALOG) {
        CheckDlgButton(h, ID_LAUNCH, BST_CHECKED);
        return TRUE;
    }
    if (m == WM_COMMAND && HIWORD(wp) == BN_CLICKED) {
        switch (LOWORD(wp)) {
        case ID_FOLDER: folder_changes++; return TRUE;
        case IDOK:
            launch_left = IsDlgButtonChecked(h, ID_LAUNCH) == BST_CHECKED;
            EndDialog(h, 1);
            return TRUE;
        case IDCANCEL: EndDialog(h, 2); return TRUE;
        }
    }
    return FALSE;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    dialog(6, L"テストのインストール");
    item(SS_LEFT, 8, 8, 220, 10, -1, 0x82, L"使用許諾契約書をお読みください。");
    item(BS_AUTORADIOBUTTON | WS_GROUP, 8, 30, 120, 10, ID_NO, 0x80, L"同意しない(&D)");
    item(BS_AUTORADIOBUTTON, 8, 44, 120, 10, ID_YES, 0x80, L"同意する(&A)");
    item(BS_PUSHBUTTON | WS_TABSTOP, 60, 118, 50, 14, ID_BACK, 0x80, L"< 戻る(&B)");
    item(BS_DEFPUSHBUTTON | WS_TABSTOP, 112, 118, 50, 14, IDOK, 0x80, L"次へ(&N) >");
    item(BS_PUSHBUTTON | WS_TABSTOP, 180, 118, 50, 14, IDCANCEL, 0x80, L"キャンセル");
    INT_PTR r = DialogBoxIndirectParamW(GetModuleHandleW(NULL), (LPCDLGTEMPLATEW)tmpl, NULL, licence, 0);
    printf("licence: %s, back pressed %d times\n", r == 1 ? "next, accepted" : r == 9 ? "next, not accepted" : "cancelled", pressed_back);
    if (r != 1) return 1;

    dialog(4, L"テストのインストール");
    item(BS_AUTOCHECKBOX | WS_TABSTOP, 8, 30, 200, 10, ID_LAUNCH, 0x80, L"インストール後にゲームを起動する");
    item(BS_PUSHBUTTON | WS_TABSTOP, 8, 60, 100, 14, ID_FOLDER, 0x80, L"インストール先の変更(&C)...");
    item(BS_PUSHBUTTON | WS_TABSTOP, 112, 118, 60, 14, IDOK, 0x80, L"インストール(&I)");
    item(BS_PUSHBUTTON | WS_TABSTOP, 180, 118, 50, 14, IDCANCEL, 0x80, L"キャンセル");
    r = DialogBoxIndirectParamW(GetModuleHandleW(NULL), (LPCDLGTEMPLATEW)tmpl, NULL, options, 0);
    printf("options: %s, start the game afterwards %s, folder changed %d times\n", r == 1 ? "install" : "cancelled",
           launch_left ? "left on" : "cleared", folder_changes);
    if (r != 1) return 1;

    CreateDirectoryA("C:\\autoinstall", NULL);
    HANDLE f = CreateFileA("C:\\autoinstall\\game.exe", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD n;
    WriteFile(f, "MZ", 2, &n, NULL);
    CloseHandle(f);
    printf("installed: %s\n", GetFileAttributesA("C:\\autoinstall\\game.exe") != INVALID_FILE_ATTRIBUTES ? "yes" : "no");
    printf("start it now: %s\n", MessageBoxW(NULL, L"インストール完了しました。実行しますか。", L"終了します。", MB_YESNO) == IDYES ? "yes" : "no");
    printf("go on: %d\n", MessageBoxW(NULL, L"フォルダが存在しないため作成します。続行しますか？", L"確認", MB_OKCANCEL));
    return 0;
}
