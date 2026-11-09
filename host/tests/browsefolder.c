/* libs: -lshell32 -lole32 -luser32 */
/* SHBrowseForFolder with a callback that answers for the player, as programs offer a default
 * folder: BFFM_INITIALIZED (lParam the program's), BFFM_SETSELECTION by path (A and W) or by
 * ID list, BFFM_SELCHANGED for each selection, then IDOK or IDCANCEL posted to the dialog;
 * the ID list it returns (SHGetPathFromIDList), the display name; a path that is not there
 * leaves the root (the desktop) selected. SHUFFLE! Essence+ asks where its saves go this way
 * and quits when it gets NULL. Paths under the current folder are shown as <cwd>, the desktop
 * from %USERPROFILE%. (sake: misc.rs sh_browse_for_folder; compared with Windows by
 * tests/winref)
 */
#include <windows.h>
#include <shlobj.h>
#include <stdio.h>
#include <string.h>

static char cwd[MAX_PATH], prof[MAX_PATH];

static void shown(const char *p, char *out) {
    size_t n = strlen(cwd), m = strlen(prof);
    if (n && !_strnicmp(p, cwd, n) && (p[n] == 0 || p[n] == '\\'))
        sprintf(out, "<cwd>%s", p + n);
    else if (m && !_strnicmp(p, prof, m) && (p[m] == 0 || p[m] == '\\'))
        sprintf(out, "%%USERPROFILE%%%s", p + m);
    else
        strcpy(out, p);
}

/* what the callback does: select `path` (A, W, or the ID list `pidl`), then press `button` */
static struct {
    const char *path;
    int wide;
    LPCITEMIDLIST pidl;
    int button;
    int calls, initialized_first;
    LPARAM lp;
    char last_sel[MAX_PATH];
    HWND dlg;
} T;

static int CALLBACK cb(HWND h, UINT m, LPARAM lp, LPARAM data) {
    T.calls++;
    if (m == BFFM_INITIALIZED) {
        T.initialized_first = T.calls == 1;
        T.lp = data;
        T.dlg = h;
        if (T.pidl)
            SendMessageA(h, BFFM_SETSELECTIONA, FALSE, (LPARAM)T.pidl);
        else if (T.path && T.wide) {
            WCHAR w[MAX_PATH];
            MultiByteToWideChar(CP_ACP, 0, T.path, -1, w, MAX_PATH);
            SendMessageW(h, BFFM_SETSELECTIONW, TRUE, (LPARAM)w);
        } else if (T.path)
            SendMessageA(h, BFFM_SETSELECTIONA, TRUE, (LPARAM)T.path);
        PostMessageA(h, WM_COMMAND, T.button, 0);
    } else if (m == BFFM_SELCHANGED) {
        char p[MAX_PATH] = "";
        if (!SHGetPathFromIDListA((LPCITEMIDLIST)lp, p))
            strcpy(p, "(no path)");
        shown(p, T.last_sel);
    }
    return 0;
}

static LPITEMIDLIST run(const char *what, const char *path, int wide, LPCITEMIDLIST pidl, int button) {
    memset(&T, 0, sizeof T);
    T.path = path;
    T.wide = wide;
    T.pidl = pidl;
    T.button = button;
    char name[MAX_PATH] = "?", p[MAX_PATH] = "", s[2 * MAX_PATH] = "";
    LPITEMIDLIST r;
    if (wide) {
        BROWSEINFOW bi = {0};
        WCHAR wn[MAX_PATH] = L"?";
        bi.pszDisplayName = wn;
        bi.lpszTitle = L"where the saves go";
        bi.ulFlags = BIF_RETURNONLYFSDIRS;
        bi.lpfn = cb;
        bi.lParam = 0x1234;
        r = SHBrowseForFolderW(&bi);
        WideCharToMultiByte(CP_ACP, 0, wn, -1, name, sizeof name, NULL, NULL);
    } else {
        BROWSEINFOA bi = {0};
        bi.pszDisplayName = name;
        bi.lpszTitle = "where the saves go";
        bi.ulFlags = BIF_RETURNONLYFSDIRS;
        bi.lpfn = cb;
        bi.lParam = 0x1234;
        r = SHBrowseForFolderA(&bi);
    }
    printf("%s: ", what);
    if (!r) {
        printf("NULL");
    } else {
        if (SHGetPathFromIDListA(r, p))
            shown(p, s);
        else
            strcpy(s, "(no path)");
        printf("%s, name %s", s, name);
    }
    printf(", initialized first %d, lParam %lx, dialog gone %d, last selchanged %s\n", T.initialized_first, (long)T.lp,
           T.dlg && !IsWindow(T.dlg) ? 1 : 0, T.last_sel);
    return r;
}

int main(void) {
    GetCurrentDirectoryA(sizeof cwd, cwd);
    GetEnvironmentVariableA("USERPROFILE", prof, sizeof prof);
    char a[MAX_PATH], b[MAX_PATH], miss[MAX_PATH];
    sprintf(a, "%s\\bff saves", cwd);
    sprintf(b, "%s\\bff_other", cwd);
    sprintf(miss, "%s\\bff_missing", cwd);
    CreateDirectoryA(a, NULL);
    CreateDirectoryA(b, NULL);
    LPITEMIDLIST pa = run("A path, OK", a, 0, NULL, IDOK);
    LPITEMIDLIST pw = run("W path, OK", b, 1, NULL, IDOK);
    LPITEMIDLIST pp = run("ID list, OK", NULL, 0, pa, IDOK);
    run("A path, cancel", a, 0, NULL, IDCANCEL);
    LPITEMIDLIST pm = run("missing path, OK", miss, 0, NULL, IDOK);
    LPITEMIDLIST pn = run("nothing selected, OK", NULL, 0, NULL, IDOK);
    CoTaskMemFree(pa);
    CoTaskMemFree(pw);
    CoTaskMemFree(pp);
    CoTaskMemFree(pm);
    CoTaskMemFree(pn);
    printf("missing folder made: %d\n", GetFileAttributesA(miss) != INVALID_FILE_ATTRIBUTES);
    return 0;
}
