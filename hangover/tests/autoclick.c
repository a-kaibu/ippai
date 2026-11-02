/* Corpus helper (tests/corpus-run.py): starts a game and plays the user at its launcher.
 *   autoclick.exe <game exe> [args]
 * Runs the game in its own folder, then every 500 ms looks at the visible top-level windows of
 * the game and the processes it started: logs the text of each new dialog (message boxes and launchers, so the run's log says
 * what a dialog said) and presses the first button whose caption starts a game ("ゲーム開始",
 * "起動", "OK", ...), never cancel/exit/settings. Quits when they are all gone.
 * Build: i686-w64-mingw32-gcc -O2 autoclick.c -o autoclick.exe -municode */
#include <windows.h>
#include <tlhelp32.h>
#include <stdio.h>
#include <wchar.h>
#include <stdarg.h>
#include <string.h>
#include <wctype.h>

/* the log as UTF-8 (wprintf would drop what the console code page can't show) */
static void out(const WCHAR *fmt, ...)
{
    WCHAR w[2048];
    char u[6144];
    va_list ap;
    va_start(ap, fmt);
    _vsnwprintf(w, 2047, fmt, ap);
    va_end(ap);
    w[2047] = 0;
    int n = WideCharToMultiByte(CP_UTF8, 0, w, -1, u, sizeof(u), NULL, NULL);
    if (n > 1) fwrite(u, 1, n - 1, stdout);
    fflush(stdout);
}

static DWORD game_pid, pids[64];
static int npids;

/* the game and the processes it starts (a launcher runs the real exe) */
static void update_pids(void)
{
    PROCESSENTRY32W e = { sizeof(e) };
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snap == INVALID_HANDLE_VALUE) return;
    for (BOOL ok = Process32FirstW(snap, &e); ok; ok = Process32NextW(snap, &e))
    {
        int known = 0, parent = 0;
        for (int i = 0; i < npids; i++) { known |= pids[i] == e.th32ProcessID; parent |= pids[i] == e.th32ParentProcessID; }
        if (!known && parent && npids < 64)
        {
            pids[npids++] = e.th32ProcessID;
            out(L"autoclick: child process %lu %ls\n", e.th32ProcessID, e.szExeFile);
        }
    }
    CloseHandle(snap);
}

static int ours(DWORD pid)
{
    for (int i = 0; i < npids; i++) if (pids[i] == pid) return 1;
    return 0;
}
static HWND seen[256];
static int nseen;

/* best first; matched as a substring of the caption with '&' removed */
static const WCHAR *const go[] = {
    L"ゲーム開始", L"ゲームスタート", L"ゲームを開始", L"ゲーム起動", L"ゲームを起動", L"ゲームをはじめる",
    L"設定変更を反映してゲーム起動", L"起動", L"開始", L"スタート", L"PLAY", L"Play", L"START", L"Start",
    L"ウインドウ", L"ウィンドウ", L"Window", L"はい", L"OK", L"Ok", L"了解", L"次へ", L"閉じる", NULL };
static const WCHAR *const never[] = { L"キャンセル", L"終了", L"中止", L"Cancel", L"Exit", L"Quit", L"いいえ",
    L"マニュアル", L"アンインストール", L"Uninstall", L"インストール", L"Install", NULL };

struct pick { HWND button; int rank; int verbose; WCHAR text[256]; };

static void clean(const WCHAR *in, WCHAR *out, int n)
{
    int j = 0;
    for (int i = 0; in[i] && j < n - 1; i++) if (in[i] != '&') out[j++] = in[i];
    out[j] = 0;
}

static BOOL CALLBACK child(HWND h, LPARAM lp)
{
    struct pick *p = (struct pick *)lp;
    WCHAR cls[64], raw[256], text[256];
    GetClassNameW(h, cls, 64);
    GetWindowTextW(h, raw, 256);
    clean(raw, text, 256);
    if (p->verbose && text[0] && IsWindowVisible(h)) out(L"autoclick:   [%ls] %ls\n", cls, text);
    if (_wcsicmp(cls, L"Button") || !IsWindowVisible(h) || !IsWindowEnabled(h)) return TRUE;
    LONG style = GetWindowLongW(h, GWL_STYLE) & 0x0f;
    if (style != BS_PUSHBUTTON && style != BS_DEFPUSHBUTTON) return TRUE;
    for (int i = 0; never[i]; i++) if (wcsstr(text, never[i])) return TRUE;
    for (int i = 0; go[i] && i < p->rank; i++)
        if (wcsstr(text, go[i])) { p->button = h; p->rank = i; wcscpy(p->text, text); break; }
    return TRUE;
}

static BOOL CALLBACK top(HWND h, LPARAM lp)
{
    struct pick p = { NULL, 1000, 1, L"" };
    WCHAR title[256], cls[64];
    DWORD pid;

    GetWindowThreadProcessId(h, &pid);
    if (!ours(pid) || !IsWindowVisible(h)) return TRUE;
    for (int i = 0; i < nseen; i++) if (seen[i] == h) p.verbose = 0;
    if (p.verbose)
    {
        GetWindowTextW(h, title, 256);
        GetClassNameW(h, cls, 64);
        out(L"autoclick: window [%ls] \"%ls\"\n", cls, title);
        if (nseen < 256) seen[nseen++] = h;
    }
    /* a game's own window has no push buttons, so only dialogs get pressed */
    EnumChildWindows(h, child, (LPARAM)&p);
    if (!p.button) return TRUE;
    out(L"autoclick: press \"%ls\"\n", p.text);
    fflush(stdout);
    SetForegroundWindow(h);
    SendMessageW(p.button, BM_CLICK, 0, 0);
    return FALSE;
}

/* once, at 20 s: every top-level window of ours, visible or not (what a black screen has) */
static BOOL CALLBACK dump(HWND h, LPARAM lp)
{
    DWORD pid, tid = GetWindowThreadProcessId(h, &pid);
    WCHAR title[128], cls[64];
    RECT r;
    if (!ours(pid)) return TRUE;
    GetWindowTextW(h, title, 128);
    GetClassNameW(h, cls, 64);
    GetWindowRect(h, &r);
    out(L"autoclick: list %p tid %04lx [%ls] \"%ls\" %ld,%ld-%ld,%ld%ls\n", h, tid, cls, title,
        r.left, r.top, r.right, r.bottom, IsWindowVisible(h) ? L" visible" : L"");
    return TRUE;
}

/* _vnvm_reg.tsv next to us (tests/corpus-prep.sh, from an unpacked installer's registry
 * table): root<TAB>key<TAB>name<TAB>value lines, UTF-8; "#<number>" values are DWORDs */
static void write_registry(void)
{
    WCHAR path[MAX_PATH], *slash;
    GetModuleFileNameW(NULL, path, MAX_PATH);
    if ((slash = wcsrchr(path, L'\\'))) wcscpy(slash + 1, L"_vnvm_reg.tsv");
    FILE *f = _wfopen(path, L"rb");
    if (!f) return;
    char line[4096];
    while (fgets(line, sizeof(line), f))
    {
        WCHAR w[4096], *field[4] = { w };
        line[strcspn(line, "\r\n")] = 0;
        if (!MultiByteToWideChar(CP_UTF8, 0, line, -1, w, 4096)) continue;
        int n = 1;
        for (WCHAR *p = w; *p && n < 4; p++) if (*p == L'\t') { *p = 0; field[n++] = p + 1; }
        if (n < 4) continue;
        HKEY root = !wcscmp(field[0], L"HKLM") ? HKEY_LOCAL_MACHINE : !wcscmp(field[0], L"HKCR") ? HKEY_CLASSES_ROOT :
                    !wcscmp(field[0], L"HKU") ? HKEY_USERS : HKEY_CURRENT_USER;
        HKEY k;
        if (RegCreateKeyExW(root, field[1], 0, NULL, 0, KEY_SET_VALUE, NULL, &k, NULL)) continue;
        const WCHAR *name = *field[2] ? field[2] : NULL, *v = field[3];
        if (v[0] == L'#' && (iswdigit(v[1]) || v[1] == L'-'))
        {
            DWORD d = wcstol(v + 1, NULL, 10);
            RegSetValueExW(k, name, 0, REG_DWORD, (BYTE *)&d, sizeof(d));
        }
        else RegSetValueExW(k, name, 0, REG_SZ, (BYTE *)v, (wcslen(v) + 1) * sizeof(WCHAR));
        RegCloseKey(k);
        out(L"autoclick: registry %ls\\%ls %ls = %ls\n", field[0], field[1], field[2], v);
    }
    fclose(f);
}

int wmain(int argc, WCHAR **argv)
{
    if (argc < 2) { out(L"usage: autoclick game.exe [args]\n"); return 2; }
    write_registry();
    WCHAR cmd[4096], dir[MAX_PATH];
    swprintf(cmd, 4096, L"\"%ls\"", argv[1]);
    for (int i = 2; i < argc; i++) { wcscat(cmd, L" "); wcscat(cmd, argv[i]); }
    wcscpy(dir, argv[1]);
    WCHAR *slash = wcsrchr(dir, L'\\');
    if (slash) *slash = 0;
    STARTUPINFOW si = { sizeof(si) };
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(argv[1], cmd, NULL, NULL, FALSE, 0, NULL, slash ? dir : NULL, &si, &pi))
    {
        out(L"autoclick: CreateProcess failed %lu\n", GetLastError());
        return 1;
    }
    game_pid = pi.dwProcessId;
    pids[npids++] = game_pid;
    out(L"autoclick: started pid %lu\n", game_pid);
    fflush(stdout);
    /* until the game and what it started are gone */
    int ticks = 0;
    for (;;)
    {
        Sleep(500);
        update_pids();
        int alive = 0;
        for (int i = 0; i < npids; i++)
        {
            HANDLE h = OpenProcess(SYNCHRONIZE, FALSE, pids[i]);
            if (h) { alive |= WaitForSingleObject(h, 0) == WAIT_TIMEOUT; CloseHandle(h); }
        }
        if (!alive) break;
        EnumWindows(top, 0);
        if (++ticks == 40) EnumWindows(dump, 0);
        fflush(stdout);
    }
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    out(L"autoclick: game exited %lu\n", code);
    return 0;
}
